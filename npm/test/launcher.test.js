'use strict';

const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { afterEach, test } = require('node:test');

const {
  MAX_STALE_RUN_DIRECTORIES_REMOVED,
  RELEASE_ASSETS,
  STALE_RUN_DIRECTORY_AGE_MS,
  SUPPORTED_NODE_ENGINE_RANGE,
  assertSupportedNodeVersion,
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
} = require('../bin/pigeonpost.js');

const temporaryDirectories = [];
const SUPPORTED_NODE_ERROR = /requires Node\.js \^22\.23\.2 \|\| \^24\.19\.0/;
const ADVERTISED_NODE_LINES = [
  { major: 22, floor: '22.23.2', newer: '22.24.0', below: '22.23.1' },
  { major: 24, floor: '24.19.0', newer: '24.20.0', below: '24.18.99' },
];

for (const { major, floor, newer } of ADVERTISED_NODE_LINES) {
  test(`the launcher accepts the advertised Node ${major} range`, () => {
    assert.doesNotThrow(() => assertSupportedNodeVersion(floor), floor);
    assert.doesNotThrow(() => assertSupportedNodeVersion(newer), newer);
  });
}

test('the launcher rejects versions below every advertised floor', () => {
  for (const { below } of ADVERTISED_NODE_LINES) {
    assert.throws(() => assertSupportedNodeVersion(below), SUPPORTED_NODE_ERROR, below);
  }
});

test('the launcher rejects Node 25 as an odd unadvertised release line', () => {
  assert.throws(() => assertSupportedNodeVersion('25.8.0'), SUPPORTED_NODE_ERROR);
});

test('the launcher rejects unadvertised future even release lines', () => {
  for (const version of ['26.0.0', '28.0.0']) {
    assert.throws(() => assertSupportedNodeVersion(version), SUPPORTED_NODE_ERROR, version);
  }
});

test('the launcher rejects stale, odd, prerelease, and malformed versions', () => {
  for (const version of [
    '18.20.8',
    '20.19.5',
    '23.11.1',
    '24.19.0-rc.1',
    'not-a-version',
  ]) {
    assert.throws(() => assertSupportedNodeVersion(version), SUPPORTED_NODE_ERROR, version);
  }
});

test('the npm engine enumerates exactly the launcher-advertised Node ranges', () => {
  assert.equal(SUPPORTED_NODE_ENGINE_RANGE, '^22.23.2 || ^24.19.0');
  assert.equal(require('../package.json').engines.node, SUPPORTED_NODE_ENGINE_RANGE);
});

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    fs.rmSync(directory, { force: true, recursive: true });
  }
});

function temporaryDirectory() {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'pigeonpost-launcher-'));
  temporaryDirectories.push(directory);
  return directory;
}

function fakeResponse({ body = new Uint8Array(), headers = {}, status = 200 } = {}) {
  return new Response(body, { headers, status });
}

function releaseChecksums() {
  return Object.fromEntries(
    RELEASE_ASSETS.map((asset) => [
      asset,
      crypto.createHash('sha256').update(`fixture:${asset}`).digest('hex'),
    ])
  );
}

function writeChecksums(file, checksums = releaseChecksums()) {
  fs.writeFileSync(file, `${JSON.stringify(checksums, null, 2)}\n`, { mode: 0o644 });
}

function runFixtureNpm(root, args) {
  const npmExecPath = process.env.npm_execpath;
  const command = npmExecPath ? process.execPath : process.platform === 'win32' ? 'npm.cmd' : 'npm';
  const commandArgs = npmExecPath ? [npmExecPath, ...args] : args;
  return spawnSync(command, commandArgs, {
    cwd: root,
    encoding: 'utf8',
    env: {
      ...process.env,
      // These are packaging-hook tests, not registry integration tests. npm 11 otherwise waits
      // for its registry timeout even with `publish --dry-run`, making the Node 24 floor job both
      // network-dependent and roughly seventy times slower.
      npm_config_offline: 'true',
      npm_config_cache: path.join(root, '.npm-cache'),
      npm_config_globalconfig: path.join(root, 'global.npmrc'),
      npm_config_userconfig: path.join(root, 'user.npmrc'),
    },
  });
}

test('the package checksum guard accepts one exact canonical six-target manifest', () => {
  const file = path.join(temporaryDirectory(), 'checksums.json');
  const checksums = releaseChecksums();
  writeChecksums(file, checksums);

  assert.deepEqual(verifyPackageChecksums(file), checksums);
});

test('the package checksum guard rejects missing, non-regular, and linked manifests', () => {
  const directory = temporaryDirectory();
  const missing = path.join(directory, 'missing.json');
  assert.throws(() => verifyPackageChecksums(missing), /missing checksums\.json/);

  const nestedDirectory = path.join(directory, 'directory.json');
  fs.mkdirSync(nestedDirectory);
  assert.throws(() => verifyPackageChecksums(nestedDirectory), /must be a regular file/);

  const target = path.join(directory, 'target.json');
  const linked = path.join(directory, 'linked.json');
  writeChecksums(target);
  fs.symlinkSync(target, linked);
  assert.throws(() => verifyPackageChecksums(linked), /must be a regular file/);
});

test('the package checksum guard rejects malformed, incomplete, and invalid digests', () => {
  const directory = temporaryDirectory();
  const file = path.join(directory, 'checksums.json');
  fs.writeFileSync(file, '{not-json}\n');
  assert.throws(() => verifyPackageChecksums(file), /not valid JSON/);

  const incomplete = releaseChecksums();
  delete incomplete[RELEASE_ASSETS[0]];
  writeChecksums(file, incomplete);
  assert.throws(() => verifyPackageChecksums(file), /exact six online release binaries/);

  const extra = { ...releaseChecksums(), unexpected: 'a'.repeat(64) };
  writeChecksums(file, extra);
  assert.throws(() => verifyPackageChecksums(file), /exact six online release binaries/);

  const invalid = releaseChecksums();
  invalid[RELEASE_ASSETS[0]] = 'A'.repeat(64);
  writeChecksums(file, invalid);
  assert.throws(() => verifyPackageChecksums(file), /no valid SHA-256 digest/);
});

test('the package checksum guard rejects non-canonical JSON encoding', () => {
  const file = path.join(temporaryDirectory(), 'checksums.json');
  fs.writeFileSync(file, JSON.stringify(releaseChecksums()));
  assert.throws(() => verifyPackageChecksums(file), /canonical sorted release-manifest encoding/);
});

test('npm pack and source-publish lifecycle hooks fail closed without release checksums', () => {
  const fixtureRoot = temporaryDirectory();
  const sourceRoot = path.resolve(__dirname, '..');
  const packageRoot = path.join(fixtureRoot, 'package');
  fs.cpSync(sourceRoot, packageRoot, {
    recursive: true,
    filter: (source) =>
      source !== path.join(sourceRoot, 'checksums.json') && path.basename(source) !== 'node_modules',
  });

  const missingPack = runFixtureNpm(fixtureRoot, [
    '--silent',
    'pack',
    '--dry-run',
    '--json',
    packageRoot,
  ]);
  assert.notEqual(missingPack.status, 0);
  assert.match(`${missingPack.stdout}\n${missingPack.stderr}`, /missing checksums\.json/);

  const missingPublish = runFixtureNpm(fixtureRoot, [
    '--silent',
    'publish',
    packageRoot,
    '--dry-run',
    '--access',
    'public',
    '--ignore-scripts=false',
  ]);
  assert.notEqual(missingPublish.status, 0);
  assert.match(`${missingPublish.stdout}\n${missingPublish.stderr}`, /missing checksums\.json/);

  writeChecksums(path.join(packageRoot, 'checksums.json'));
  const guardedPack = runFixtureNpm(fixtureRoot, [
    '--silent',
    'pack',
    '--dry-run',
    '--json',
    packageRoot,
  ]);
  assert.equal(guardedPack.status, 0, guardedPack.stderr);
  const packed = JSON.parse(guardedPack.stdout)[0];
  assert.equal(packed.entryCount, 5);
  assert.deepEqual(
    packed.files.map(({ mode, path: member }) => ({ member, mode })),
    [
      { member: 'LICENSE', mode: 0o644 },
      { member: 'README.md', mode: 0o644 },
      { member: 'bin/pigeonpost.js', mode: 0o755 },
      { member: 'checksums.json', mode: 0o644 },
      { member: 'package.json', mode: 0o644 },
    ]
  );

  const guardedPublish = runFixtureNpm(fixtureRoot, [
    '--silent',
    'publish',
    packageRoot,
    '--dry-run',
    '--access',
    'public',
    '--ignore-scripts=false',
  ]);
  assert.equal(guardedPublish.status, 0, guardedPublish.stderr);
});

test('all six SDS release targets have a launcher mapping', () => {
  assert.equal(platformKey('darwin', 'arm64'), 'darwin-arm64');
  assert.equal(platformKey('darwin', 'x64'), 'darwin-x64');
  assert.equal(platformKey('linux', 'arm64'), 'linux-arm64');
  assert.equal(platformKey('linux', 'x64'), 'linux-x64');
  assert.equal(platformKey('win32', 'arm64'), 'win32-arm64');
  assert.equal(platformKey('win32', 'x64'), 'win32-x64');
  assert.throws(() => platformKey('freebsd', 'x64'), /no prebuilt binary/);
});

test('the Windows cache defaults to the current user LocalAppData profile', () => {
  const homeDirectory = 'C:\\Users\\pigeon';
  assert.equal(
    selectedCacheRoot({
      platform: 'win32',
      env: { LOCALAPPDATA: `${homeDirectory}\\AppData\\Local` },
      homeDirectory,
    }),
    `${homeDirectory}\\AppData\\Local\\Pigeonpost\\cache`
  );
  assert.equal(
    selectedCacheRoot({ platform: 'win32', env: {}, homeDirectory }),
    `${homeDirectory}\\AppData\\Local\\Pigeonpost\\cache`
  );
});

test('a valid cached binary is hashed on every verification', async () => {
  const file = path.join(temporaryDirectory(), 'pigeonpost');
  const bytes = Buffer.from('verified release binary');
  fs.writeFileSync(file, bytes);
  const digest = crypto.createHash('sha256').update(bytes).digest('hex');

  assert.equal(await verifyCachedBinary(file, digest), true);
  assert.equal(fs.existsSync(file), true);
});

test('cache roots and descendants must remain owner-only real directories', () => {
  if (process.platform === 'win32') return;
  const parent = temporaryDirectory();
  const root = path.join(parent, 'cache');
  fs.mkdirSync(root, { mode: 0o700 });
  fs.chmodSync(root, 0o770);
  assert.throws(
    () => ensurePrivateCachePath(root, path.join(root, 'bin')),
    /mode 0700 or stricter/
  );

  const target = path.join(parent, 'target');
  fs.mkdirSync(target, { mode: 0o700 });
  const linkedRoot = path.join(parent, 'linked-cache');
  fs.symlinkSync(target, linkedRoot, 'dir');
  assert.throws(
    () => ensurePrivateCachePath(linkedRoot, path.join(linkedRoot, 'bin')),
    /real directory/
  );
});

test('cache path validation rejects ambiguous and redirected ancestors before mutation', () => {
  if (process.platform === 'win32') return;
  const base = temporaryDirectory();

  const lexicalParent = path.join(base, 'missing');
  const lexicalRoot = `${lexicalParent}${path.sep}..${path.sep}escaped${path.sep}cache`;
  assert.throws(
    () => ensurePrivateCachePath(lexicalRoot, path.join(lexicalRoot, 'bin')),
    /parent-directory component/
  );
  assert.equal(fs.existsSync(lexicalParent), false);
  assert.equal(fs.existsSync(path.join(base, 'escaped')), false);

  const outside = path.join(base, 'outside');
  fs.mkdirSync(outside, { mode: 0o700 });
  const linked = path.join(base, 'linked');
  fs.symlinkSync(outside, linked, 'dir');
  const linkedRoot = path.join(linked, 'redirected-cache');
  assert.throws(
    () => ensurePrivateCachePath(linkedRoot, path.join(linkedRoot, 'bin')),
    /real directory/
  );
  assert.equal(fs.existsSync(path.join(outside, 'redirected-cache')), false);

  const mutable = path.join(base, 'mutable');
  fs.mkdirSync(mutable, { mode: 0o700 });
  fs.chmodSync(mutable, 0o777);
  const mutableRoot = path.join(mutable, 'cache');
  assert.throws(
    () => ensurePrivateCachePath(mutableRoot, path.join(mutableRoot, 'bin')),
    /mutable by another user/
  );
  assert.equal(fs.existsSync(mutableRoot), false);
});

test('macOS cache paths use only the proven root alias rewrite', () => {
  if (process.platform !== 'darwin') return;
  const root = temporaryDirectory();
  const requested = path.join(root, 'cache');
  const secured = ensurePrivateCachePath(requested, path.join(requested, 'bin'));
  if (requested.startsWith('/var/')) {
    assert.equal(secured.startsWith('/private/var/'), true);
  } else if (requested.startsWith('/tmp/')) {
    assert.equal(secured.startsWith('/private/tmp/'), true);
  } else {
    assert.equal(secured, requested);
  }
});

test('hard-linked cache entries are rejected and only the named link is discarded', async () => {
  if (process.platform === 'win32') return;
  const root = temporaryDirectory();
  const source = path.join(root, 'source');
  const cached = path.join(root, 'cached');
  fs.writeFileSync(source, 'shared inode', { mode: 0o600 });
  fs.linkSync(source, cached);
  const digest = crypto.createHash('sha256').update('shared inode').digest('hex');

  assert.equal(await verifyCachedBinary(cached, digest, { cacheRoot: root }), false);
  assert.equal(fs.existsSync(cached), false);
  assert.equal(fs.readFileSync(source, 'utf8'), 'shared inode');
});

test('execution uses a private verified copy with an independent inode', async () => {
  const root = temporaryDirectory();
  const cacheParent = path.join(root, 'bin', 'test');
  const canonicalRoot = ensurePrivateCachePath(root, cacheParent);
  const cached = path.join(cacheParent, 'pigeonpost');
  const bytes = Buffer.from('verified executable bytes');
  fs.writeFileSync(cached, bytes, { mode: 0o600 });
  const digest = crypto.createHash('sha256').update(bytes).digest('hex');
  assert.equal(await verifyCachedBinary(cached, digest, { cacheRoot: canonicalRoot }), true);

  const staged = stageVerifiedBinary(cached, digest, {
    cacheRoot: canonicalRoot,
    platform: process.platform,
  });
  try {
    if (process.platform !== 'win32') {
      assert.notEqual(fs.statSync(staged.path).ino, fs.statSync(cached).ino);
      fs.chmodSync(cached, 0o700);
    }
    fs.writeFileSync(cached, 'replacement after staging');
    assert.deepEqual(fs.readFileSync(staged.path), bytes);
    if (process.platform !== 'win32') {
      assert.equal(fs.statSync(staged.directory).mode & 0o077, 0);
      assert.equal(fs.statSync(staged.path).mode & 0o277, 0);
    }
  } finally {
    discardStagedBinary(staged);
  }
  assert.equal(fs.existsSync(staged.directory), false);
});

test('stale execution cleanup removes only old exact launcher staging shapes', () => {
  const root = temporaryDirectory();
  const runRoot = path.join(root, 'run');
  const canonicalRoot = ensurePrivateCachePath(root, runRoot);
  const executableName = process.platform === 'win32' ? 'pigeonpost.exe' : 'pigeonpost';
  const nowMs = Date.now();
  const oldDate = new Date(nowMs - STALE_RUN_DIRECTORY_AGE_MS - 60_000);

  function stagingDirectory(name, entries, old = true) {
    const directory = path.join(runRoot, name);
    fs.mkdirSync(directory, { mode: 0o700 });
    for (const entry of entries) {
      const target = path.join(directory, entry.name);
      if (entry.directory) fs.mkdirSync(target, { mode: 0o700 });
      else fs.writeFileSync(target, entry.body || 'staged binary', { mode: 0o500 });
      if (old) fs.utimesSync(target, oldDate, oldDate);
    }
    if (old) fs.utimesSync(directory, oldDate, oldDate);
    return directory;
  }

  const staleBinary = stagingDirectory('exec-a1B2c3', [{ name: executableName }]);
  const staleEmpty = stagingDirectory('exec-d4E5f6', []);
  const recent = stagingDirectory('exec-g7H8i9', [{ name: executableName }], false);
  const unknownExtra = stagingDirectory('exec-j0K1l2', [
    { name: executableName },
    { name: 'keep.txt', body: 'unknown user entry' },
  ]);
  const nested = stagingDirectory('exec-m3N4o5', [{ name: executableName, directory: true }]);
  const active = stagingDirectory(`exec-${process.pid}-P6q7R8`, []);
  const unrelated = stagingDirectory('exec-not-a-launcher-directory', [
    { name: 'keep.txt', body: 'unrelated' },
  ]);

  const result = cleanupStaleRunDirectories(canonicalRoot, {
    nowMs,
    platform: process.platform,
  });

  assert.equal(result.removed, 2);
  assert.equal(fs.existsSync(staleBinary), false);
  assert.equal(fs.existsSync(staleEmpty), false);
  for (const retained of [recent, unknownExtra, nested, active, unrelated]) {
    assert.equal(fs.existsSync(retained), true, `${retained} should have been retained`);
  }
  assert.equal(fs.readFileSync(path.join(unknownExtra, 'keep.txt'), 'utf8'), 'unknown user entry');
});

test('stale execution cleanup has a hard per-run deletion budget', () => {
  const root = temporaryDirectory();
  const runRoot = path.join(root, 'run');
  const canonicalRoot = ensurePrivateCachePath(root, runRoot);
  const executableName = process.platform === 'win32' ? 'pigeonpost.exe' : 'pigeonpost';
  const nowMs = Date.now();
  const oldDate = new Date(nowMs - STALE_RUN_DIRECTORY_AGE_MS - 60_000);

  for (let index = 0; index < MAX_STALE_RUN_DIRECTORIES_REMOVED + 2; index += 1) {
    const name = `exec-${index.toString(36).padStart(6, '0')}`;
    const directory = path.join(runRoot, name);
    fs.mkdirSync(directory, { mode: 0o700 });
    const executable = path.join(directory, executableName);
    fs.writeFileSync(executable, 'staged binary', { mode: 0o500 });
    fs.utimesSync(executable, oldDate, oldDate);
    fs.utimesSync(directory, oldDate, oldDate);
  }

  const result = cleanupStaleRunDirectories(canonicalRoot, {
    nowMs,
    platform: process.platform,
  });
  assert.equal(result.removed, MAX_STALE_RUN_DIRECTORIES_REMOVED);
  assert.equal(fs.readdirSync(runRoot).length, 2);
});

test('concurrent Windows cache publication verifies EEXIST and EPERM winners', async () => {
  for (const code of ['EEXIST', 'EPERM']) {
    const root = temporaryDirectory();
    const parent = path.join(root, code.toLowerCase());
    const canonicalRoot = ensurePrivateCachePath(root, parent);
    const destination = path.join(parent, 'pigeonpost.exe');
    const temp = path.join(parent, 'download.tmp');
    const bytes = Buffer.from(`verified ${code} release bytes`);
    const digest = crypto.createHash('sha256').update(bytes).digest('hex');
    fs.writeFileSync(destination, bytes, { mode: 0o500 });
    fs.writeFileSync(temp, bytes, { mode: 0o500 });
    let renameCalls = 0;

    const result = await publishCachedBinary(temp, destination, digest, {
      cacheRoot: canonicalRoot,
      platform: 'win32',
      renameSync() {
        renameCalls += 1;
        const error = new Error(`simulated ${code} publication race`);
        error.code = code;
        throw error;
      },
    });

    assert.deepEqual(result, { usedExisting: true });
    assert.equal(renameCalls, 1);
    assert.deepEqual(fs.readFileSync(destination), bytes);
    assert.equal(fs.existsSync(temp), true);
  }
});

test('a bad Windows publication winner is removed and the verified temp wins the retry', async () => {
  const root = temporaryDirectory();
  const parent = path.join(root, 'cache');
  const canonicalRoot = ensurePrivateCachePath(root, parent);
  const destination = path.join(parent, 'pigeonpost.exe');
  const temp = path.join(parent, 'download.tmp');
  const bytes = Buffer.from('verified release bytes');
  const digest = crypto.createHash('sha256').update(bytes).digest('hex');
  fs.writeFileSync(destination, 'tampered winner', { mode: 0o500 });
  fs.writeFileSync(temp, bytes, { mode: 0o500 });
  let renameCalls = 0;

  const result = await publishCachedBinary(temp, destination, digest, {
    cacheRoot: canonicalRoot,
    platform: 'win32',
    renameSync(source, target) {
      renameCalls += 1;
      if (renameCalls === 1) {
        const error = new Error('simulated Windows destination race');
        error.code = 'EPERM';
        throw error;
      }
      fs.renameSync(source, target);
    },
  });

  assert.deepEqual(result, { usedExisting: false });
  assert.equal(renameCalls, 2);
  assert.deepEqual(fs.readFileSync(destination), bytes);
  assert.equal(fs.existsSync(temp), false);
});

test('the native installer receives a stable npm launcher instead of the versioned cache path', () => {
  const modulePath = '/npm/current/package/bin/pigeonpost.js';
  const stableEntry = '/npm/current/bin/pigeonpost';
  const nativeCache = '/cache/pigeonpost/bin/0.2.0/linux-x64/pigeonpost';
  const realpath = (value) => {
    if (value === stableEntry || value === modulePath) return modulePath;
    return value;
  };

  const env = serviceHandoffEnvironment({
    env: {
      PIGEONPOST_NPM_LAUNCHER_ENTRY: nativeCache,
      PIGEONPOST_NPM_LAUNCHER_NODE: '/attacker/node',
      PIGEONPOST_NPM_LAUNCHER_PROTOCOL: 'spoofed',
    },
    invokedPath: stableEntry,
    modulePath,
    nodePath: '/runtime/node',
    args: ['install'],
    realpath,
  });

  assert.equal(env.PIGEONPOST_NPM_LAUNCHER_PROTOCOL, 'npm-v1');
  assert.equal(env.PIGEONPOST_NPM_LAUNCHER_NODE, '/runtime/node');
  assert.equal(env.PIGEONPOST_NPM_LAUNCHER_ENTRY, stableEntry);
  assert.notEqual(env.PIGEONPOST_NPM_LAUNCHER_ENTRY, nativeCache);
});

test('an unrelated or vanished invocation path is not persisted into a service', () => {
  const modulePath = '/npm/current/package/bin/pigeonpost.js';
  const env = serviceHandoffEnvironment({
    env: {},
    invokedPath: '/temporary/npx/pigeonpost',
    modulePath,
    nodePath: '/runtime/node',
    args: ['install'],
    realpath(value) {
      if (value === modulePath) return modulePath;
      throw new Error('missing');
    },
  });

  assert.equal(env.PIGEONPOST_NPM_LAUNCHER_ENTRY, modulePath);
});

test('service installation rejects disposable npm-exec and npx launchers', () => {
  const unixEntry =
    '/home/agent/.npm/_npx/deadbeef/node_modules/@bekirdag/pigeonpost/bin/pigeonpost.js';
  const windowsEntry =
    'C:\\Users\\agent\\AppData\\Local\\npm-cache\\_NPX\\deadbeef\\node_modules\\@bekirdag\\pigeonpost\\bin\\pigeonpost.js';

  for (const modulePath of [unixEntry, windowsEntry]) {
    assert.throws(
      () =>
        serviceHandoffEnvironment({
          env: {},
          invokedPath: modulePath,
          modulePath,
          nodePath: '/runtime/node',
          args: ['--home', '/srv/pigeonpost-agent', '--json', 'install'],
          realpath: (value) => value,
        }),
      /Install globally with `npm i -g @bekirdag\/pigeonpost@0\.2\.0`/
    );
  }

  assert.equal(isNpmExecCachePath(unixEntry), true);
  assert.equal(isNpmExecCachePath(windowsEntry), true);
  assert.equal(isNpmExecCachePath('/opt/npm/_npx-tools/pigeonpost.js'), false);
});

test('a disposable launcher remains usable without service installation', () => {
  const modulePath =
    '/home/agent/.npm/_npx/deadbeef/node_modules/@bekirdag/pigeonpost/bin/pigeonpost.js';
  const common = {
    env: {},
    invokedPath: modulePath,
    modulePath,
    nodePath: '/runtime/node',
    realpath: (value) => value,
  };

  assert.doesNotThrow(() => serviceHandoffEnvironment({ ...common, args: ['--version'] }));
  assert.doesNotThrow(() =>
    serviceHandoffEnvironment({ ...common, args: ['install', '--no-service'] })
  );
});

test('service installation rejects an ephemeral fallback module path', () => {
  const modulePath =
    '/home/agent/.npm/_npx/deadbeef/node_modules/@bekirdag/pigeonpost/bin/pigeonpost.js';

  assert.throws(
    () =>
      serviceHandoffEnvironment({
        env: {},
        invokedPath: '/tmp/vanished/pigeonpost',
        modulePath,
        nodePath: '/runtime/node',
        args: ['install'],
        realpath(value) {
          if (value === modulePath) return modulePath;
          throw new Error('missing');
        },
      }),
    /disposable npm-exec\/npx cache launcher/
  );
});

test('a tampered cache entry is deleted instead of executed', async () => {
  const file = path.join(temporaryDirectory(), 'pigeonpost');
  fs.writeFileSync(file, 'tampered');
  const expected = crypto.createHash('sha256').update('original').digest('hex');

  assert.equal(await verifyCachedBinary(file, expected), false);
  assert.equal(fs.existsSync(file), false);
});

test('a directory at the cache path is never recursively removed', async () => {
  const directory = path.join(temporaryDirectory(), 'pigeonpost');
  fs.mkdirSync(directory);
  const expected = crypto.createHash('sha256').update('original').digest('hex');

  await assert.rejects(
    verifyCachedBinary(directory, expected),
    /cache entry is a directory and will not be removed/
  );
  assert.equal(fs.statSync(directory).isDirectory(), true);
});

test('release mirrors must use HTTPS', () => {
  assert.throws(
    () => releaseLocation({ PIGEONPOST_RELEASE_BASE: 'http://mirror.example/releases' }),
    /must use HTTPS/
  );
});

test('a declared oversized response is rejected before it is buffered', async () => {
  const fetchImpl = async () =>
    fakeResponse({ headers: { 'content-length': '11' }, body: Buffer.alloc(11) });

  await assert.rejects(
    fetchAsset('https://mirror.example/pigeonpost', { fetchImpl, maxBytes: 10 }),
    /exceeds the 10-byte download limit/
  );
});

test('a streamed oversized response is rejected without Content-Length', async () => {
  const fetchImpl = async () => fakeResponse({ body: Buffer.alloc(11) });

  await assert.rejects(
    fetchAsset('https://mirror.example/pigeonpost', { fetchImpl, maxBytes: 10 }),
    /exceeds the 10-byte download limit/
  );
});

test('an invalid Content-Length is rejected', async () => {
  const fetchImpl = async () =>
    fakeResponse({ headers: { 'content-length': 'not-a-number' }, body: Buffer.from('binary') });

  await assert.rejects(
    fetchAsset('https://mirror.example/pigeonpost', { fetchImpl }),
    /Invalid release asset Content-Length/
  );
});

test('a stalled request is aborted at the download deadline', async () => {
  const fetchImpl = async (_url, { signal }) =>
    new Promise((_resolve, reject) => {
      signal.addEventListener(
        'abort',
        () => {
          const error = new Error('aborted');
          error.name = 'AbortError';
          reject(error);
        },
        { once: true }
      );
    });

  await assert.rejects(
    fetchAsset('https://mirror.example/pigeonpost', { fetchImpl, timeoutMs: 20 }),
    /timed out after 20 ms/
  );
});

test('a mirror cannot redirect downloads to another origin', async () => {
  const fetchImpl = async () =>
    fakeResponse({ status: 302, headers: { location: 'https://other.example/pigeonpost' } });

  await assert.rejects(
    fetchAsset('https://mirror.example/pigeonpost', { fetchImpl }),
    /Refusing release asset redirect/
  );
});

test('same-origin redirect loops stop at the fixed redirect limit', async () => {
  let calls = 0;
  const fetchImpl = async () => {
    calls += 1;
    return fakeResponse({ status: 302, headers: { location: '/again' } });
  };

  await assert.rejects(
    fetchAsset('https://mirror.example/pigeonpost', { fetchImpl }),
    /exceeded the 3-redirect limit/
  );
  assert.equal(calls, 4);
});

test('the official source accepts only its explicit HTTPS asset origin', async () => {
  const calls = [];
  const fetchImpl = async (url) => {
    calls.push(url.href);
    if (calls.length === 1) {
      return fakeResponse({
        status: 302,
        headers: { location: 'https://release-assets.githubusercontent.com/signed-asset' },
      });
    }
    return fakeResponse({ body: Buffer.from('binary') });
  };

  const bytes = await fetchAsset('https://github.com/release', {
    fetchImpl,
    official: true,
  });
  assert.equal(bytes.toString(), 'binary');
  assert.equal(calls.length, 2);
});

test('even the official source refuses a redirect downgrade', async () => {
  const fetchImpl = async () =>
    fakeResponse({ status: 302, headers: { location: 'http://release-assets.githubusercontent.com/x' } });

  await assert.rejects(
    fetchAsset('https://github.com/release', { fetchImpl, official: true }),
    /Refusing release asset redirect/
  );
});
