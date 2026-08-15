#!/usr/bin/env node
'use strict';

/**
 * Resolve the platform binary, verify it, and execute it.
 *
 * Release assets are fetched lazily so installs also work with `--ignore-scripts`. The npm
 * provenance attestation covers checksums.json, and those SHA-256 digests cover the binaries.
 * Cache entries are verified on every execution; the cache is never a trust boundary.
 */

const { spawnSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const { version } = require('../package.json');

// The release binary is ~21 MB, so 30s demanded roughly 700 KB/s just to finish the first run —
// a link at 370 KB/s failed with a bare "download timed out" and no way to retry differently. This
// is a ceiling on a stalled transfer, not a target, so it costs a fast connection nothing.
// PIGEONPOST_DOWNLOAD_TIMEOUT_MS overrides it for a genuinely slow or metered link.
const DEFAULT_DOWNLOAD_TIMEOUT_MS = 300_000;

function configuredDownloadTimeoutMs() {
  const raw = process.env.PIGEONPOST_DOWNLOAD_TIMEOUT_MS;
  if (raw === undefined) return DEFAULT_DOWNLOAD_TIMEOUT_MS;
  const parsed = Number(raw);
  // A malformed override must not silently become 0 and fail every download instantly.
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(
      `PIGEONPOST_DOWNLOAD_TIMEOUT_MS must be a positive number of milliseconds; got ${raw}`,
    );
  }
  return parsed;
}

const DOWNLOAD_TIMEOUT_MS = configuredDownloadTimeoutMs();
const MAX_DOWNLOAD_BYTES = 64 * 1024 * 1024;
const MAX_REDIRECTS = 3;
const STALE_RUN_DIRECTORY_AGE_MS = 7 * 24 * 60 * 60 * 1000;
const MAX_STALE_RUN_DIRECTORIES_SCANNED = 128;
const MAX_STALE_RUN_DIRECTORIES_REMOVED = 16;
const MAX_PUBLICATION_ATTEMPTS = 3;
const MAX_CACHE_PATH_BYTES = 4096;
const MAX_CACHE_PATH_COMPONENTS = 128;
const OFFICIAL_ORIGIN = 'https://github.com';
const SERVICE_HANDOFF_PROTOCOL = 'npm-v1';
const SERVICE_HANDOFF_PROTOCOL_ENV = 'PIGEONPOST_NPM_LAUNCHER_PROTOCOL';
const SERVICE_HANDOFF_NODE_ENV = 'PIGEONPOST_NPM_LAUNCHER_NODE';
const SERVICE_HANDOFF_ENTRY_ENV = 'PIGEONPOST_NPM_LAUNCHER_ENTRY';
const OFFICIAL_REDIRECT_ORIGINS = new Set([
  OFFICIAL_ORIGIN,
  'https://release-assets.githubusercontent.com',
  'https://objects.githubusercontent.com',
]);

const TARGETS = {
  'darwin-arm64': 'pigeonpost-darwin-arm64',
  'darwin-x64': 'pigeonpost-darwin-x64',
  'linux-arm64': 'pigeonpost-linux-arm64',
  'linux-x64': 'pigeonpost-linux-x64',
  'win32-arm64': 'pigeonpost-win32-arm64.exe',
  'win32-x64': 'pigeonpost-win32-x64.exe',
};
const RELEASE_ASSETS = Object.freeze(Object.values(TARGETS).sort());
const MAX_CHECKSUM_MANIFEST_BYTES = 16 * 1024;
const SUPPORTED_NODE_ENGINE_RANGE = '^22.23.2 || ^24.19.0';
const SUPPORTED_NODE_FLOORS = new Map([
  [22, [23, 2]],
  [24, [19, 0]],
]);

function assertSupportedNodeVersion(value = process.versions.node) {
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.exec(value);
  if (match) {
    const [major, minor, patch] = match.slice(1).map(Number);
    const floor = SUPPORTED_NODE_FLOORS.get(major);
    const supportedFloor =
      floor && (minor > floor[0] || (minor === floor[0] && patch >= floor[1]));
    if (supportedFloor) return;
  }
  // Say which Node was found and how to get a supported one. The bare range left people staring
  // at a version string with nothing to act on — including, repeatedly, the person who wrote it.
  // Homebrew's `node` formula tracks Current, so `brew install node` lands on a line this package
  // does not support yet, and `npm i -g` under it succeeds and leaves a binary that refuses to run.
  throw new Error(
    `Pigeonpost requires Node.js ${SUPPORTED_NODE_ENGINE_RANGE} (supported LTS lines: 22 and 24), ` +
      `but this is Node ${value}.\n` +
      '\n' +
      'Install a supported line and reinstall Pigeonpost under it:\n' +
      '\n' +
      '  nvm install 24.19.0 && nvm alias default 24.19.0\n' +
      '  npm i -g @bekirdag/pigeonpost\n' +
      '\n' +
      "If `node -v` already reports a supported version, another copy is shadowing this one: run\n" +
      '`which -a pigeonpost`, remove the one outside your current Node prefix, and open a new shell.'
  );
}

function checkedHttpsUrl(value, label) {
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new Error(`${label} is not a valid URL: ${value}`);
  }
  if (url.protocol !== 'https:') {
    throw new Error(`${label} must use HTTPS; got ${url.protocol || 'no scheme'}`);
  }
  if (url.username || url.password) {
    throw new Error(`${label} must not contain credentials`);
  }
  if (url.search || url.hash) {
    throw new Error(`${label} must not contain a query string or fragment`);
  }
  return url;
}

function releaseLocation(env = process.env) {
  const official = `https://github.com/bekirdag/pigeonpost/releases/download/v${version}`;
  const configured = env.PIGEONPOST_RELEASE_BASE || official;
  const parsed = checkedHttpsUrl(configured, 'PIGEONPOST_RELEASE_BASE');
  const base = parsed.href.replace(/\/+$/, '');
  return {
    base,
    // An operator mirror may redirect within its own origin, but never gets GitHub's broader
    // release-asset allowlist. Explicitly setting the canonical URL keeps canonical behaviour.
    official:
      parsed.origin === OFFICIAL_ORIGIN &&
      parsed.pathname.replace(/\/+$/, '') === new URL(official).pathname,
  };
}

function platformKey(platform = process.platform, arch = process.arch) {
  const key = `${platform}-${arch}`;
  if (!TARGETS[key]) {
    throw new Error(
      `Pigeonpost has no prebuilt binary for ${key}.\n` +
        `Supported: ${Object.keys(TARGETS).join(', ')}\n` +
        'Build from source: https://github.com/bekirdag/pigeonpost'
    );
  }
  return key;
}

function selectedCacheRoot(options = {}) {
  const platform = options.platform || process.platform;
  const env = options.env || process.env;
  const pathApi = platform === 'win32' ? path.win32 : path;
  const homeDirectory = options.homeDirectory ?? os.homedir();
  let configured = options.cacheRoot || env.PIGEONPOST_CACHE;
  if (!configured && platform === 'win32') {
    const localAppData = env.LOCALAPPDATA ||
      (homeDirectory ? pathApi.join(homeDirectory, 'AppData', 'Local') : null);
    if (!localAppData) {
      throw new Error('Pigeonpost could not locate the current Windows user profile cache');
    }
    configured = pathApi.join(localAppData, 'Pigeonpost', 'cache');
  }
  if (!configured) {
    configured = pathApi.join(homeDirectory || os.tmpdir(), '.cache', 'pigeonpost');
  }
  return pathApi.resolve(configured);
}

function cachePath(key, options = {}) {
  const platform = options.platform || process.platform;
  const pathApi = platform === 'win32' ? path.win32 : path;
  const exe = platform === 'win32' ? 'pigeonpost.exe' : 'pigeonpost';
  return pathApi.join(selectedCacheRoot(options), 'bin', version, key, exe);
}

function pathIsInside(root, candidate) {
  const relative = path.relative(root, candidate);
  return relative === '' || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative));
}

function boundedAbsoluteCachePath(value, label) {
  if (typeof value !== 'string' || value.length === 0 || value.includes('\0')) {
    throw new Error(`${label} must be a non-empty path without embedded NUL`);
  }
  if (value.split(/[\\/]+/u).includes('..')) {
    throw new Error(`${label} must not contain a parent-directory component`);
  }
  const absolute = path.resolve(value);
  const relativeComponents = absolute
    .slice(path.parse(absolute).root.length)
    .split(path.sep)
    .filter(Boolean);
  if (
    Buffer.byteLength(absolute) > MAX_CACHE_PATH_BYTES ||
    relativeComponents.length > MAX_CACHE_PATH_COMPONENTS
  ) {
    throw new Error(`${label} exceeds the bounded Pigeonpost cache path limit`);
  }
  return absolute;
}

function rejectMacOsAllowAcl(directory) {
  if (process.platform !== 'darwin') return;
  const result = spawnSync('/bin/ls', ['-lde', directory], {
    encoding: 'utf8',
    env: { ...process.env, LANG: 'C', LC_ALL: 'C' },
    windowsHide: true,
  });
  if (result.status !== 0 || result.error) {
    throw new Error(`Pigeonpost could not verify the cache ACL: ${directory}`);
  }
  const hasAllowAcl = result.stdout
    .split(/\r?\n/u)
    .some((line) => /^\s*\d+:/u.test(line) && /\ballow\b/iu.test(line));
  if (hasAllowAcl) {
    throw new Error(`Pigeonpost cache path has an extended allow ACL: ${directory}`);
  }
}

function validateUnixAncestor(directory) {
  const stat = fs.lstatSync(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink()) {
    throw new Error(`Pigeonpost cache ancestor must be a real directory: ${directory}`);
  }
  const uid = process.getuid();
  const rootOwnedSticky = stat.uid === 0 && (stat.mode & 0o1000) !== 0;
  if (stat.uid !== 0 && stat.uid !== uid) {
    throw new Error(`Pigeonpost cache ancestor has an untrusted owner: ${directory}`);
  }
  if ((stat.mode & 0o022) !== 0 && !rootOwnedSticky) {
    throw new Error(`Pigeonpost cache ancestor is mutable by another user: ${directory}`);
  }
  rejectMacOsAllowAcl(directory);
}

function resolveTrustedMacOsRootAlias(absolute) {
  if (process.platform !== 'darwin') return absolute;
  const root = path.parse(absolute).root;
  const components = absolute.slice(root.length).split(path.sep).filter(Boolean);
  const first = components[0];
  const targets = new Map([
    ['var', 'private/var'],
    ['tmp', 'private/tmp'],
    ['etc', 'private/etc'],
  ]);
  const expected = targets.get(first);
  if (!expected) return absolute;

  const alias = path.join(root, first);
  const aliasStat = fs.lstatSync(alias);
  if (!aliasStat.isSymbolicLink()) return absolute;
  if (aliasStat.uid !== 0) {
    throw new Error(`Pigeonpost refuses a non-root cache path alias: ${alias}`);
  }
  const target = fs.readlinkSync(alias, { encoding: 'buffer' });
  if (!Buffer.isBuffer(target) || !target.equals(Buffer.from(expected))) {
    throw new Error(`Pigeonpost refuses an unexpected cache path alias: ${alias}`);
  }

  const physical = path.join(root, expected);
  validateUnixAncestor(path.join(root, 'private'));
  validateUnixAncestor(physical);
  const followed = fs.statSync(alias);
  const opened = fs.lstatSync(physical);
  if (followed.dev !== opened.dev || followed.ino !== opened.ino) {
    throw new Error(`Pigeonpost cache path alias changed during validation: ${alias}`);
  }
  return path.join(physical, ...components.slice(1));
}

function ensureUnixCacheRoot(root) {
  const requested = boundedAbsoluteCachePath(root, 'Pigeonpost cache root');
  const absolute = resolveTrustedMacOsRootAlias(requested);
  const parsed = path.parse(absolute);
  let current = parsed.root;
  validateUnixAncestor(current);
  const components = absolute.slice(parsed.root.length).split(path.sep).filter(Boolean);
  for (let index = 0; index < components.length; index += 1) {
    current = path.join(current, components[index]);
    try {
      fs.mkdirSync(current, { mode: 0o700 });
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
    }
    if (index + 1 === components.length) validatePrivateDirectory(current, true);
    else validateUnixAncestor(current);
  }
  if (components.length === 0) validatePrivateDirectory(current, true);
  return absolute;
}

function validatePrivateDirectory(directory, cacheRoot = false) {
  const stat = fs.lstatSync(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink()) {
    throw new Error(`Pigeonpost cache path must be a real directory: ${directory}`);
  }
  if (process.platform !== 'win32') {
    const uid = process.getuid();
    if (stat.uid !== uid) {
      const kind = cacheRoot ? 'root' : 'directory';
      throw new Error(`Pigeonpost cache ${kind} is not owned by the current user: ${directory}`);
    }
    if ((stat.mode & 0o077) !== 0) {
      throw new Error(`Pigeonpost cache directory must be mode 0700 or stricter: ${directory}`);
    }
    rejectMacOsAllowAcl(directory);
  }
}

function ensurePrivateCachePath(root, destinationParent) {
  const requestedRoot = boundedAbsoluteCachePath(root, 'Pigeonpost cache root');
  const requestedParent = boundedAbsoluteCachePath(
    destinationParent,
    'Pigeonpost cache destination'
  );
  const cacheRoot = process.platform === 'win32'
    ? requestedRoot
    : resolveTrustedMacOsRootAlias(requestedRoot);
  const parent = process.platform === 'win32'
    ? requestedParent
    : resolveTrustedMacOsRootAlias(requestedParent);
  if (!pathIsInside(cacheRoot, parent)) {
    throw new Error('Pigeonpost cache destination escapes its configured root');
  }

  let canonicalRoot;
  if (process.platform === 'win32') {
    fs.mkdirSync(cacheRoot, { recursive: true, mode: 0o700 });
    validatePrivateDirectory(cacheRoot, true);
    canonicalRoot = fs.realpathSync.native(cacheRoot);
    validatePrivateDirectory(canonicalRoot, true);
  } else {
    canonicalRoot = ensureUnixCacheRoot(cacheRoot);
  }

  const relative = path.relative(cacheRoot, parent);
  let current = canonicalRoot;
  for (const component of relative.split(path.sep).filter(Boolean)) {
    current = path.join(current, component);
    try {
      fs.mkdirSync(current, { mode: 0o700 });
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
    }
    validatePrivateDirectory(current);
  }
  return canonicalRoot;
}

function verifyPackageChecksums(
  checksumsPath = path.join(__dirname, '..', 'checksums.json')
) {
  let stat;
  try {
    stat = fs.lstatSync(checksumsPath);
  } catch (error) {
    if (error.code === 'ENOENT') {
      throw new Error(
        'This Pigeonpost package is missing checksums.json, so a binary could not be verified. ' +
          'Refusing to pack, publish, or run it.'
      );
    }
    throw error;
  }
  if (!stat.isFile()) {
    throw new Error('Pigeonpost checksums.json must be a regular file. Refusing to continue.');
  }
  if (stat.size === 0 || stat.size > MAX_CHECKSUM_MANIFEST_BYTES) {
    throw new Error(
      `Pigeonpost checksums.json must be non-empty and at most ${MAX_CHECKSUM_MANIFEST_BYTES} bytes.`
    );
  }

  let raw;
  let checksums;
  try {
    raw = fs.readFileSync(checksumsPath, 'utf8');
    checksums = JSON.parse(raw);
  } catch (error) {
    throw new Error(`Pigeonpost checksums.json is not valid JSON: ${error.message}`);
  }
  if (!checksums || typeof checksums !== 'object' || Array.isArray(checksums)) {
    throw new Error('Pigeonpost checksums.json must contain one JSON object.');
  }

  const names = Object.keys(checksums).sort();
  if (JSON.stringify(names) !== JSON.stringify(RELEASE_ASSETS)) {
    throw new Error(
      'Pigeonpost checksums.json must cover the exact six online release binaries.'
    );
  }
  for (const name of RELEASE_ASSETS) {
    if (typeof checksums[name] !== 'string' || !/^[a-f0-9]{64}$/.test(checksums[name])) {
      throw new Error(`Pigeonpost checksums.json has no valid SHA-256 digest for ${name}.`);
    }
  }

  const canonical = Object.fromEntries(RELEASE_ASSETS.map((name) => [name, checksums[name]]));
  if (raw !== `${JSON.stringify(canonical, null, 2)}\n`) {
    throw new Error(
      'Pigeonpost checksums.json must use the canonical sorted release-manifest encoding.'
    );
  }
  return checksums;
}

function expectedDigest(asset) {
  const checksums = verifyPackageChecksums();
  const digest = checksums[asset];
  if (typeof digest !== 'string' || !/^[a-f0-9]{64}$/.test(digest)) {
    throw new Error(`No valid SHA-256 checksum is recorded for ${asset}. Refusing to run.`);
  }
  return digest;
}

function digestsEqual(left, right) {
  if (!/^[a-f0-9]{64}$/.test(left) || !/^[a-f0-9]{64}$/.test(right)) return false;
  return crypto.timingSafeEqual(Buffer.from(left, 'hex'), Buffer.from(right, 'hex'));
}

function redirectAllowed(initial, next, official) {
  if (next.protocol !== 'https:' || next.username || next.password) return false;
  if (official) return OFFICIAL_REDIRECT_ORIGINS.has(next.origin);
  return next.origin === initial.origin;
}

async function fetchWithRedirects(startUrl, options) {
  const initial = checkedHttpsUrl(startUrl, 'release asset URL');
  let current = initial;

  for (let redirects = 0; redirects <= MAX_REDIRECTS; redirects += 1) {
    const response = await options.fetchImpl(current, {
      redirect: 'manual',
      signal: options.signal,
    });
    if (response.status < 300 || response.status >= 400) return response;

    const location = response.headers.get('location');
    if (response.body) await response.body.cancel();
    if (!location) throw new Error(`Release asset redirect ${response.status} had no Location`);
    if (redirects === MAX_REDIRECTS) {
      throw new Error(`Release asset exceeded the ${MAX_REDIRECTS}-redirect limit`);
    }

    const next = new URL(location, current);
    if (!redirectAllowed(initial, next, options.official)) {
      throw new Error(`Refusing release asset redirect from ${current.origin} to ${next.origin}`);
    }
    current = next;
  }
  throw new Error('Release asset redirect loop');
}

async function boundedBody(response, maxBytes) {
  const length = response.headers.get('content-length');
  if (length !== null) {
    if (!/^\d+$/.test(length)) throw new Error(`Invalid release asset Content-Length: ${length}`);
    if (Number(length) > maxBytes) {
      throw new Error(`Release asset exceeds the ${maxBytes}-byte download limit`);
    }
  }
  if (!response.body) return Buffer.alloc(0);

  const chunks = [];
  let received = 0;
  const reader = response.body.getReader();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      const chunk = Buffer.from(value);
      received += chunk.length;
      if (received > maxBytes) {
        await reader.cancel('release asset too large');
        throw new Error(`Release asset exceeds the ${maxBytes}-byte download limit`);
      }
      chunks.push(chunk);
    }
  } finally {
    reader.releaseLock();
  }
  return Buffer.concat(chunks, received);
}

async function fetchAsset(startUrl, options = {}) {
  const fetchImpl = options.fetchImpl || globalThis.fetch;
  if (typeof fetchImpl !== 'function') {
    throw new Error('This supported Node.js runtime has no fetch implementation');
  }
  const timeoutMs = options.timeoutMs || DOWNLOAD_TIMEOUT_MS;
  const maxBytes = options.maxBytes || MAX_DOWNLOAD_BYTES;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);

  try {
    const response = await fetchWithRedirects(startUrl, {
      fetchImpl,
      signal: controller.signal,
      official: options.official === true,
    });
    if (!response.ok) {
      if (response.body) await response.body.cancel();
      throw new Error(`Could not download the release asset — HTTP ${response.status}`);
    }
    return await boundedBody(response, maxBytes);
  } catch (error) {
    if (controller.signal.aborted) {
      throw new Error(`Release asset download timed out after ${timeoutMs} ms`);
    }
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

function discardCacheEntry(destination, expectedIdentity = null) {
  try {
    const stat = fs.lstatSync(destination);
    if (stat.isDirectory()) {
      throw new Error(`Pigeonpost cache entry is a directory and will not be removed: ${destination}`);
    }
    // A concurrent publisher may have replaced the rejected entry. Never unlink that winner based
    // on an identity which was checked earlier in this invocation.
    if (expectedIdentity && !sameFileIdentity(expectedIdentity, stat)) return false;
    fs.unlinkSync(destination);
    return true;
  } catch (error) {
    if (error.code === 'ENOENT') return false;
    throw error;
  }
}

async function sha256Descriptor(file, fd) {
  const hash = crypto.createHash('sha256');
  let bytes = 0;
  for await (const chunk of fs.createReadStream(file, { fd, autoClose: false, start: 0 })) {
    bytes += chunk.length;
    if (bytes > MAX_DOWNLOAD_BYTES) throw new Error('cached binary exceeds the size limit');
    hash.update(chunk);
  }
  return hash.digest('hex');
}

function sameFileIdentity(left, right) {
  return (
    left.dev === right.dev &&
    left.ino === right.ino &&
    left.size === right.size &&
    left.mtimeMs === right.mtimeMs
  );
}

function cacheFileIsPrivate(stat) {
  if (!stat.isFile() || stat.isSymbolicLink() || stat.nlink !== 1) return false;
  if (process.platform === 'win32') return true;
  return stat.uid === process.getuid() && (stat.mode & 0o022) === 0;
}

async function verifyCachedBinary(destination, expected, options = {}) {
  let stat;
  try {
    stat = fs.lstatSync(destination);
  } catch (error) {
    if (error.code === 'ENOENT') return false;
    throw error;
  }

  let actual = null;
  let fd = null;
  if (cacheFileIsPrivate(stat) && stat.size <= MAX_DOWNLOAD_BYTES) {
    try {
      if (options.cacheRoot) {
        const parent = boundedAbsoluteCachePath(
          path.dirname(destination),
          'Pigeonpost cache entry parent'
        );
        ensurePrivateCachePath(options.cacheRoot, parent);
      }
      const noFollow = fs.constants.O_NOFOLLOW || 0;
      fd = fs.openSync(destination, fs.constants.O_RDONLY | noFollow);
      const opened = fs.fstatSync(fd);
      if (!cacheFileIsPrivate(opened) || !sameFileIdentity(stat, opened)) {
        throw new Error('cache entry changed while it was opened');
      }
      actual = await sha256Descriptor(destination, fd);
      const hashed = fs.fstatSync(fd);
      if (!sameFileIdentity(opened, hashed)) {
        throw new Error('cache entry changed while it was hashed');
      }
    } catch {
      actual = null;
    } finally {
      if (fd !== null) fs.closeSync(fd);
    }
  }
  if (!actual || !digestsEqual(actual, expected)) {
    discardCacheEntry(destination, stat);
    return false;
  }
  if (process.platform !== 'win32') fs.chmodSync(destination, 0o555);
  return true;
}

function boundedCleanupLimit(value, fallback, maximum) {
  if (!Number.isSafeInteger(value) || value < 0) return fallback;
  return Math.min(value, maximum);
}

function stagedRunProcessId(name) {
  if (/^exec-[A-Za-z0-9]{6}$/.test(name)) return null;
  const match = /^exec-([1-9][0-9]{0,9})-[A-Za-z0-9]{6}$/.exec(name);
  if (!match) return undefined;
  const pid = Number(match[1]);
  return Number.isSafeInteger(pid) ? pid : undefined;
}

function processAppearsAlive(pid, processKill = process.kill.bind(process)) {
  if (pid === process.pid) return true;
  try {
    processKill(pid, 0);
    return true;
  } catch (error) {
    if (error.code === 'ESRCH') return false;
    // EPERM means the process exists but cannot be signalled. Unknown errors also retain the
    // directory: cleanup is best effort and must not guess that a process is dead.
    return true;
  }
}

function singleDirectoryEntry(directory) {
  const handle = fs.opendirSync(directory);
  try {
    const first = handle.readSync();
    if (handle.readSync() !== null) return undefined;
    return first;
  } finally {
    handle.closeSync();
  }
}

function removeRecognizedStaleRunDirectory(runRoot, name, cutoffMs, options = {}) {
  const pid = stagedRunProcessId(name);
  if (pid === undefined) return false;
  if (pid !== null && processAppearsAlive(pid, options.processKill)) return false;

  const directory = path.join(runRoot, name);
  const initialDirectory = fs.lstatSync(directory);
  if (
    !initialDirectory.isDirectory() ||
    initialDirectory.isSymbolicLink() ||
    initialDirectory.mtimeMs > cutoffMs
  ) {
    return false;
  }
  validatePrivateDirectory(directory);

  const entry = singleDirectoryEntry(directory);
  const currentDirectory = fs.lstatSync(directory);
  if (!sameFileIdentity(initialDirectory, currentDirectory)) return false;
  if (entry === null) {
    fs.rmdirSync(directory);
    return true;
  }
  if (entry === undefined) return false;

  const platform = options.platform || process.platform;
  const executableName = platform === 'win32' ? 'pigeonpost.exe' : 'pigeonpost';
  if (entry.name !== executableName || !entry.isFile()) return false;
  const executable = path.join(directory, executableName);
  const initialExecutable = fs.lstatSync(executable);
  if (
    !cacheFileIsPrivate(initialExecutable) ||
    initialExecutable.size > MAX_DOWNLOAD_BYTES ||
    initialExecutable.mtimeMs > cutoffMs
  ) {
    return false;
  }
  const currentExecutable = fs.lstatSync(executable);
  if (!sameFileIdentity(initialExecutable, currentExecutable)) return false;

  // These are the only destructive operations: one exact recognized file and its now-empty
  // parent. Unknown entries, nested directories, links, and changed identities are retained.
  fs.unlinkSync(executable);
  fs.rmdirSync(directory);
  return true;
}

function cleanupStaleRunDirectories(cacheRoot, options = {}) {
  const canonicalRoot = ensurePrivateCachePath(cacheRoot, cacheRoot);
  const runRoot = path.join(canonicalRoot, 'run');
  try {
    validatePrivateDirectory(runRoot);
  } catch (error) {
    if (error.code === 'ENOENT') return { removed: 0, scanned: 0 };
    throw error;
  }
  if (!pathIsInside(canonicalRoot, runRoot)) {
    throw new Error('Pigeonpost staging directory escaped its configured cache root');
  }

  const maxScanned = boundedCleanupLimit(
    options.maxScanned,
    MAX_STALE_RUN_DIRECTORIES_SCANNED,
    MAX_STALE_RUN_DIRECTORIES_SCANNED
  );
  const maxRemoved = boundedCleanupLimit(
    options.maxRemoved,
    MAX_STALE_RUN_DIRECTORIES_REMOVED,
    MAX_STALE_RUN_DIRECTORIES_REMOVED
  );
  const nowMs = Number.isFinite(options.nowMs) ? options.nowMs : Date.now();
  const staleAfterMs = Number.isFinite(options.staleAfterMs) && options.staleAfterMs >= 0
    ? options.staleAfterMs
    : STALE_RUN_DIRECTORY_AGE_MS;
  const cutoffMs = nowMs - staleAfterMs;
  let scanned = 0;
  let removed = 0;
  const handle = fs.opendirSync(runRoot);
  try {
    while (scanned < maxScanned && removed < maxRemoved) {
      const entry = handle.readSync();
      if (entry === null) break;
      scanned += 1;
      try {
        if (removeRecognizedStaleRunDirectory(runRoot, entry.name, cutoffMs, options)) {
          removed += 1;
        }
      } catch {
        // Stale cleanup must not make a valid invocation unavailable. A failed exact deletion is
        // retained for a later bounded pass; no fallback recursively removes it.
      }
    }
  } finally {
    handle.closeSync();
  }
  return { removed, scanned };
}

function discardStagedBinary(staged) {
  try {
    const stat = fs.lstatSync(staged.path);
    if (stat.isDirectory()) {
      throw new Error(`Pigeonpost staged executable became a directory: ${staged.path}`);
    }
    fs.unlinkSync(staged.path);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  try {
    fs.rmdirSync(staged.directory);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
}

function stageVerifiedBinary(destination, expected, options = {}) {
  const platform = options.platform || process.platform;
  const destinationParent = boundedAbsoluteCachePath(
    path.dirname(destination),
    'Pigeonpost cache entry parent'
  );
  const cacheRoot = ensurePrivateCachePath(options.cacheRoot, destinationParent);
  const runRoot = path.join(cacheRoot, 'run');
  ensurePrivateCachePath(cacheRoot, runRoot);
  cleanupStaleRunDirectories(cacheRoot, { platform });
  const directory = fs.mkdtempSync(path.join(runRoot, `exec-${process.pid}-`));
  if (process.platform !== 'win32') fs.chmodSync(directory, 0o700);
  validatePrivateDirectory(directory);

  const staged = {
    directory,
    path: path.join(directory, platform === 'win32' ? 'pigeonpost.exe' : 'pigeonpost'),
  };
  let sourceFd = null;
  let destinationFd = null;
  try {
    const named = fs.lstatSync(destination);
    if (!cacheFileIsPrivate(named) || named.size > MAX_DOWNLOAD_BYTES) {
      throw new Error('Pigeonpost cache entry is not a private bounded regular file');
    }
    sourceFd = fs.openSync(
      destination,
      fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0)
    );
    const opened = fs.fstatSync(sourceFd);
    if (!cacheFileIsPrivate(opened) || !sameFileIdentity(named, opened)) {
      throw new Error('Pigeonpost cache entry changed before private staging');
    }

    destinationFd = fs.openSync(staged.path, 'wx+', 0o500);
    const hash = crypto.createHash('sha256');
    const buffer = Buffer.allocUnsafe(64 * 1024);
    let position = 0;
    while (position < opened.size) {
      const length = Math.min(buffer.length, opened.size - position);
      const bytesRead = fs.readSync(sourceFd, buffer, 0, length, position);
      if (bytesRead === 0) throw new Error('Pigeonpost cache entry ended during private staging');
      hash.update(buffer.subarray(0, bytesRead));
      let written = 0;
      while (written < bytesRead) {
        written += fs.writeSync(
          destinationFd,
          buffer,
          written,
          bytesRead - written,
          position + written
        );
      }
      position += bytesRead;
    }
    if (!sameFileIdentity(opened, fs.fstatSync(sourceFd))) {
      throw new Error('Pigeonpost cache entry changed during private staging');
    }
    const actual = hash.digest('hex');
    if (!digestsEqual(actual, expected)) {
      throw new Error('Pigeonpost cache entry failed verification during private staging');
    }
    fs.fsyncSync(destinationFd);
    const stagedHash = crypto.createHash('sha256');
    let stagedPosition = 0;
    while (stagedPosition < opened.size) {
      const length = Math.min(buffer.length, opened.size - stagedPosition);
      const bytesRead = fs.readSync(destinationFd, buffer, 0, length, stagedPosition);
      if (bytesRead === 0) throw new Error('Pigeonpost private execution copy ended early');
      stagedHash.update(buffer.subarray(0, bytesRead));
      stagedPosition += bytesRead;
    }
    if (!digestsEqual(stagedHash.digest('hex'), expected)) {
      throw new Error('Pigeonpost private execution copy failed verification');
    }
    if (process.platform !== 'win32') fs.fchmodSync(destinationFd, 0o500);
    const stagedStat = fs.fstatSync(destinationFd);
    if (!stagedStat.isFile() || stagedStat.nlink !== 1 || stagedStat.size !== opened.size) {
      throw new Error('Pigeonpost private execution copy has an unsafe file identity');
    }
    fs.closeSync(destinationFd);
    destinationFd = null;
    fs.closeSync(sourceFd);
    sourceFd = null;
    return staged;
  } catch (error) {
    if (destinationFd !== null) fs.closeSync(destinationFd);
    if (sourceFd !== null) fs.closeSync(sourceFd);
    try {
      discardStagedBinary(staged);
    } catch {
      // Preserve the verification failure. Cleanup is exact and never recursive.
    }
    throw error;
  }
}

function isPublicationRace(error, platform) {
  return error && (error.code === 'EEXIST' || (platform === 'win32' && error.code === 'EPERM'));
}

async function publishCachedBinary(temp, destination, expected, options = {}) {
  const platform = options.platform || process.platform;
  const renameSync = options.renameSync || fs.renameSync;
  let lastRace = null;

  for (let attempt = 0; attempt < MAX_PUBLICATION_ATTEMPTS; attempt += 1) {
    try {
      renameSync(temp, destination);
      if (!(await verifyCachedBinary(destination, expected, { cacheRoot: options.cacheRoot }))) {
        throw new Error(`Published binary failed cache verification: ${destination}`);
      }
      return { usedExisting: false };
    } catch (error) {
      if (!isPublicationRace(error, platform)) throw error;
      lastRace = error;
    }

    // Windows rename reports an already-published destination as EEXIST or EPERM. It is a winner
    // only if the ordinary full descriptor/hash verification accepts it in this invocation.
    if (await verifyCachedBinary(destination, expected, { cacheRoot: options.cacheRoot })) {
      return { usedExisting: true };
    }
  }

  const code = lastRace && lastRace.code ? ` (${lastRace.code})` : '';
  throw new Error(`Could not publish the verified Pigeonpost cache entry${code}`);
}

function isNpmExecCachePath(value) {
  if (typeof value !== 'string' || value.length === 0) return false;
  return value
    .replace(/\\/g, '/')
    .split('/')
    .some((component) => component.toLowerCase() === '_npx');
}

function serviceInstallationRequested(args) {
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === '--home') {
      index += 1;
      continue;
    }
    if (argument === '--json' || argument.startsWith('--home=')) continue;
    return (
      argument === 'install' &&
      !args.slice(index + 1).some((value) =>
        ['--no-service', '--help', '-h'].includes(value)
      )
    );
  }
  return false;
}

async function download(asset, destination, options = {}) {
  const expected = expectedDigest(asset);
  const release = options.release || releaseLocation();
  const url = `${release.base}/${encodeURIComponent(asset)}`;
  process.stderr.write(`pigeonpost: fetching ${asset} (${version})\n`);
  const bytes = await fetchAsset(url, {
    fetchImpl: options.fetchImpl,
    official: release.official,
  });
  const actual = crypto.createHash('sha256').update(bytes).digest('hex');
  if (!digestsEqual(actual, expected)) {
    throw new Error(
      `Checksum mismatch for ${asset}.\n  expected ${expected}\n  got      ${actual}\n` +
        'Refusing to run. This binary is not the one this package was published with.'
    );
  }

  ensurePrivateCachePath(options.cacheRoot, path.dirname(destination));
  const temp = `${destination}.${process.pid}.${Date.now()}.${crypto.randomBytes(8).toString('hex')}.tmp`;
  try {
    const fd = fs.openSync(temp, 'wx', 0o755);
    try {
      fs.writeFileSync(fd, bytes);
      fs.fsyncSync(fd);
      if (process.platform !== 'win32') fs.fchmodSync(fd, 0o555);
    } finally {
      fs.closeSync(fd);
    }
    await publishCachedBinary(temp, destination, expected, options);
  } finally {
    try {
      fs.unlinkSync(temp);
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
  }
}

async function resolveBinary(options = {}) {
  const platform = options.platform || process.platform;
  const key = platformKey(platform, options.arch || process.arch);
  const asset = TARGETS[key];
  const requestedRoot = selectedCacheRoot(options);
  const requestedParent = path.join(requestedRoot, 'bin', version, key);
  const cacheRoot = ensurePrivateCachePath(requestedRoot, requestedParent);
  const destination = cachePath(key, { platform, cacheRoot });
  const expected = expectedDigest(asset);

  if (await verifyCachedBinary(destination, expected, { cacheRoot })) {
    return { cacheRoot, destination, expected, platform };
  }
  await download(asset, destination, { ...options, cacheRoot });
  // Verify after a download as well as on cache hits. This catches disk corruption and keeps one
  // invariant at the execution boundary: every path returned here was hashed in this invocation.
  if (!(await verifyCachedBinary(destination, expected, { cacheRoot }))) {
    throw new Error(`Downloaded binary failed cache verification: ${destination}`);
  }
  return { cacheRoot, destination, expected, platform };
}

/**
 * Carry the stable npm entrypoint into the native process.
 *
 * `pigeonpost install` runs inside the downloaded native binary, so `current_exe()` is the
 * versioned cache entry. A service must start this launcher instead: npm replaces the package at a
 * stable entrypoint, and this launcher re-hashes the selected cache entry before every execution.
 * Preserve the path used to invoke the launcher when it resolves to this module (normally npm's
 * global bin link); otherwise use this module's own absolute path.
 */
function serviceHandoffEnvironment(options = {}) {
  const env = { ...(options.env || process.env) };
  const args = options.args || process.argv.slice(2);
  const modulePath = path.resolve(options.modulePath || __filename);
  const invokedPath = path.resolve(options.invokedPath || process.argv[1] || modulePath);
  const nodePath = path.resolve(options.nodePath || process.execPath);
  const realpath = options.realpath || fs.realpathSync.native;

  let entryPath = modulePath;
  let resolvedEntryPath = null;
  try {
    if (realpath(invokedPath) === realpath(modulePath)) entryPath = invokedPath;
  } catch {
    // The module path is known to exist because Node loaded it. An unresolvable argv path is not
    // suitable for a persistent service, so fall back to the loaded package entrypoint.
  }

  try {
    resolvedEntryPath = realpath(entryPath);
  } catch {
    // The native boundary requires the entrypoint to exist. Preserve that fail-closed check while
    // still detecting an ephemeral lexical path here.
  }

  if (
    serviceInstallationRequested(args) &&
    [entryPath, resolvedEntryPath].some(isNpmExecCachePath)
  ) {
    throw new Error(
      'Service installation cannot persist a disposable npm-exec/npx cache launcher. ' +
        'Install globally with `npm i -g @bekirdag/pigeonpost@0.2.0` and rerun `pigeonpost install`.'
    );
  }

  env[SERVICE_HANDOFF_PROTOCOL_ENV] = SERVICE_HANDOFF_PROTOCOL;
  env[SERVICE_HANDOFF_NODE_ENV] = nodePath;
  env[SERVICE_HANDOFF_ENTRY_ENV] = entryPath;
  return env;
}

async function main() {
  assertSupportedNodeVersion();
  const binary = await resolveBinary();
  const staged = stageVerifiedBinary(binary.destination, binary.expected, binary);
  let result;
  try {
    result = spawnSync(staged.path, process.argv.slice(2), {
      env: serviceHandoffEnvironment(),
      stdio: 'inherit',
      windowsHide: true,
    });
  } finally {
    discardStagedBinary(staged);
  }
  if (result.error) throw result.error;
  if (result.signal) process.kill(process.pid, result.signal);
  process.exit(result.status === null ? 1 : result.status);
}

if (require.main === module) {
  main().catch((error) => {
    console.error(error.message);
    process.exit(1);
  });
}

module.exports = {
  DOWNLOAD_TIMEOUT_MS,
  MAX_DOWNLOAD_BYTES,
  MAX_STALE_RUN_DIRECTORIES_REMOVED,
  MAX_STALE_RUN_DIRECTORIES_SCANNED,
  RELEASE_ASSETS,
  STALE_RUN_DIRECTORY_AGE_MS,
  SUPPORTED_NODE_ENGINE_RANGE,
  assertSupportedNodeVersion,
  boundedBody,
  cachePath,
  cleanupStaleRunDirectories,
  discardStagedBinary,
  ensurePrivateCachePath,
  fetchAsset,
  isNpmExecCachePath,
  platformKey,
  publishCachedBinary,
  releaseLocation,
  selectedCacheRoot,
  serviceHandoffEnvironment,
  stageVerifiedBinary,
  verifyCachedBinary,
  verifyPackageChecksums,
};
