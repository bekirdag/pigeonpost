#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";

const expectedNpmMembers = new Map([
  ["package/LICENSE", { mode: "0644", permissions: "-rw-r--r--" }],
  ["package/README.md", { mode: "0644", permissions: "-rw-r--r--" }],
  ["package/bin/pigeonpost.js", { mode: "0755", permissions: "-rwxr-xr-x" }],
  ["package/checksums.json", { mode: "0644", permissions: "-rw-r--r--" }],
  ["package/package.json", { mode: "0644", permissions: "-rw-r--r--" }],
]);
const nativeSboms = new Map([
  ["pigeonpost-darwin-arm64.spdx.json", "pigeonpost-darwin-arm64"],
  ["pigeonpost-darwin-x64.spdx.json", "pigeonpost-darwin-x64"],
  ["pigeonpost-linux-arm64.spdx.json", "pigeonpost-linux-arm64"],
  ["pigeonpost-linux-x64.spdx.json", "pigeonpost-linux-x64"],
  ["pigeonpost-win32-arm64.spdx.json", "pigeonpost-win32-arm64.exe"],
  ["pigeonpost-win32-x64.spdx.json", "pigeonpost-win32-x64.exe"],
  ["ppcompliance-darwin-arm64.spdx.json", "ppcompliance-darwin-arm64"],
  ["ppcompliance-darwin-x64.spdx.json", "ppcompliance-darwin-x64"],
  ["ppcompliance-linux-arm64.spdx.json", "ppcompliance-linux-arm64"],
  ["ppcompliance-linux-x64.spdx.json", "ppcompliance-linux-x64"],
]);
const packageRelationshipTypes = new Set([
  "ANCESTOR_OF",
  "BUILD_DEPENDENCY_OF",
  "BUILD_TOOL_OF",
  "CONTAINED_BY",
  "CONTAINS",
  "COPY_OF",
  "DATA_FILE_OF",
  "DEPENDENCY_OF",
  "DEPENDS_ON",
  "DESCENDANT_OF",
  "DEV_DEPENDENCY_OF",
  "DEV_TOOL_OF",
  "DISTRIBUTION_ARTIFACT",
  "DYNAMIC_LINK",
  "EXAMPLE_OF",
  "GENERATED_FROM",
  "GENERATES",
  "HAS_PREREQUISITE",
  "OPTIONAL_COMPONENT_OF",
  "OPTIONAL_DEPENDENCY_OF",
  "PACKAGE_OF",
  "PREREQUISITE_FOR",
  "PROVIDED_DEPENDENCY_OF",
  "RUNTIME_DEPENDENCY_OF",
  "STATIC_LINK",
  "TEST_DEPENDENCY_OF",
  "TEST_OF",
  "TEST_TOOL_OF",
  "VARIANT_OF",
]);

const hash = (file) =>
  crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const isRecord = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const isUsefulSpdxValue = (value) =>
  typeof value === "string" &&
  value.trim().length > 0 &&
  !["NONE", "NOASSERTION"].includes(value.trim().toUpperCase());

function verifyNativeSbom({ artifact, artifactDigest, sbom, sbomName, version }) {
  const expectedNamespace = `https://pigeonpost.dev/sbom/v${version}/${artifact}`;
  if (
    !isRecord(sbom) ||
    sbom.spdxVersion !== "SPDX-2.3" ||
    sbom.dataLicense !== "CC0-1.0" ||
    sbom.SPDXID !== "SPDXRef-DOCUMENT" ||
    sbom.name !== artifact ||
    sbom.documentNamespace !== expectedNamespace
  ) {
    throw new Error(`${sbomName} is not the canonical SBOM for ${artifact}`);
  }
  if (!Array.isArray(sbom.packages) || sbom.packages.length < 2) {
    throw new Error(`${sbomName} must contain the artifact and its SPDX dependency inventory`);
  }
  if (!Array.isArray(sbom.relationships) || sbom.relationships.length === 0) {
    throw new Error(`${sbomName} must contain useful SPDX relationships`);
  }

  const packagesById = new Map();
  for (const pkg of sbom.packages) {
    if (
      !isRecord(pkg) ||
      typeof pkg.SPDXID !== "string" ||
      !/^SPDXRef-[A-Za-z0-9.-]+$/.test(pkg.SPDXID) ||
      pkg.SPDXID === "SPDXRef-DOCUMENT" ||
      !isUsefulSpdxValue(pkg.name) ||
      packagesById.has(pkg.SPDXID)
    ) {
      throw new Error(`${sbomName} contains an invalid or duplicate SPDX package`);
    }
    const hasUsefulChecksum =
      Array.isArray(pkg.checksums) &&
      pkg.checksums.some(
        (checksum) =>
          isRecord(checksum) &&
          isUsefulSpdxValue(checksum.algorithm) &&
          isUsefulSpdxValue(checksum.checksumValue),
      );
    const hasUsefulExternalReference =
      Array.isArray(pkg.externalRefs) &&
      pkg.externalRefs.some(
        (reference) =>
          isRecord(reference) && isUsefulSpdxValue(reference.referenceLocator),
      );
    if (
      !isUsefulSpdxValue(pkg.versionInfo) &&
      !hasUsefulChecksum &&
      !hasUsefulExternalReference
    ) {
      throw new Error(`${sbomName} contains a package without useful identity data`);
    }
    packagesById.set(pkg.SPDXID, pkg);
  }

  const described = sbom.relationships.filter(
    (relationship) =>
      isRecord(relationship) &&
      relationship.spdxElementId === "SPDXRef-DOCUMENT" &&
      relationship.relationshipType === "DESCRIBES",
  );
  if (described.length !== 1) {
    throw new Error(`${sbomName} must describe exactly one root package`);
  }
  const rootPackage = packagesById.get(described[0].relatedSpdxElement);
  if (
    !rootPackage ||
    rootPackage.name !== artifact ||
    rootPackage.primaryPackagePurpose !== "FILE"
  ) {
    throw new Error(`${sbomName} does not describe ${artifact} as its root package`);
  }
  const rootSha256 = Array.isArray(rootPackage.checksums)
    ? rootPackage.checksums.filter(
        (checksum) =>
          isRecord(checksum) &&
          typeof checksum.algorithm === "string" &&
          checksum.algorithm.toUpperCase() === "SHA256",
      )
    : [];
  if (
    rootSha256.length !== 1 ||
    !/^[a-f0-9]{64}$/.test(rootSha256[0].checksumValue) ||
    rootSha256[0].checksumValue !== artifactDigest ||
    rootPackage.versionInfo !== `sha256:${artifactDigest}`
  ) {
    throw new Error(
      `${sbomName} root package digest does not match SHA256SUMS for ${artifact}`,
    );
  }

  const expectedCargoRoot = artifact.startsWith("ppcompliance-")
    ? "pigeonpost-compliance"
    : "pigeonpost-cli";
  const expectedCargoPurl = `pkg:cargo/${expectedCargoRoot}@${version}`;
  const cargoRoots = [...packagesById.values()].filter(
    (pkg) =>
      pkg.name === expectedCargoRoot &&
      pkg.versionInfo === version &&
      pkg.primaryPackagePurpose === "APPLICATION" &&
      Array.isArray(pkg.externalRefs) &&
      pkg.externalRefs.some(
        (reference) =>
          isRecord(reference) &&
          reference.referenceCategory === "PACKAGE-MANAGER" &&
          reference.referenceType === "purl" &&
          reference.referenceLocator === expectedCargoPurl,
      ),
  );
  if (cargoRoots.length !== 1) {
    throw new Error(
      `${sbomName} must contain exactly one ${expectedCargoRoot} Cargo root`,
    );
  }
  const generatedFromCargoRoot = sbom.relationships.filter(
    (relationship) =>
      isRecord(relationship) &&
      relationship.spdxElementId === rootPackage.SPDXID &&
      relationship.relatedSpdxElement === cargoRoots[0].SPDXID &&
      relationship.relationshipType === "GENERATED_FROM",
  );
  if (generatedFromCargoRoot.length !== 1) {
    throw new Error(
      `${sbomName} must bind ${artifact} to its exact Cargo root`,
    );
  }

  const relatedPackages = new Map(
    [...packagesById.keys()].map((packageId) => [packageId, new Set()]),
  );
  for (const relationship of sbom.relationships) {
    if (
      !isRecord(relationship) ||
      !isUsefulSpdxValue(relationship.spdxElementId) ||
      !isUsefulSpdxValue(relationship.relatedSpdxElement) ||
      !isUsefulSpdxValue(relationship.relationshipType)
    ) {
      throw new Error(`${sbomName} contains an invalid SPDX relationship`);
    }
    if (
      packagesById.has(relationship.spdxElementId) &&
      packagesById.has(relationship.relatedSpdxElement) &&
      packageRelationshipTypes.has(relationship.relationshipType)
    ) {
      relatedPackages
        .get(relationship.spdxElementId)
        .add(relationship.relatedSpdxElement);
      relatedPackages
        .get(relationship.relatedSpdxElement)
        .add(relationship.spdxElementId);
    }
  }
  const reachable = new Set([rootPackage.SPDXID]);
  const pending = [rootPackage.SPDXID];
  while (pending.length > 0) {
    for (const packageId of relatedPackages.get(pending.pop())) {
      if (!reachable.has(packageId)) {
        reachable.add(packageId);
        pending.push(packageId);
      }
    }
  }
  const unrelated = [...packagesById.keys()].filter(
    (packageId) => !reachable.has(packageId),
  );
  if (unrelated.length > 0) {
    throw new Error(
      `${sbomName} contains package inventory unrelated to ${artifact}: ${unrelated.join(", ")}`,
    );
  }
}

function tarListing(tarball, verbose = false) {
  return execFileSync("tar", [verbose ? "-tvzf" : "-tzf", tarball], {
    encoding: "utf8",
    env: { ...process.env, LC_ALL: "C" },
    maxBuffer: 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
  })
    .split("\n")
    .map((line) => line.replace(/\r$/, ""))
    .filter((line) => line.length > 0);
}

function verifyNpmTarball(tarball) {
  const members = tarListing(tarball);
  const expectedNames = [...expectedNpmMembers.keys()].sort();
  if (JSON.stringify([...members].sort()) !== JSON.stringify(expectedNames)) {
    throw new Error(`unexpected npm package members: ${members.join(", ")}`);
  }

  const verbose = tarListing(tarball, true);
  if (verbose.length !== members.length) {
    throw new Error("npm package metadata does not cover the exact member set");
  }
  for (const [member, expected] of expectedNpmMembers) {
    const matches = verbose.filter((line) => line.endsWith(` ${member}`));
    if (matches.length !== 1) {
      throw new Error(`npm package metadata does not name ${member} exactly once`);
    }
    const permissions = matches[0].trimStart().split(/\s+/, 1)[0];
    if (permissions !== expected.permissions) {
      throw new Error(
        `npm package member ${member} must be a regular file with mode ${expected.mode}; ` +
          `got ${permissions}`,
      );
    }
  }
}

const [distArg, version, repository, serverSourceArg] = process.argv.slice(2);
if (!distArg || !version || !repository || !serverSourceArg) {
  throw new Error(
    "usage: verify-release-assets.mjs <dist-dir> <version> <owner/repo> <server.json>",
  );
}
if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) {
  throw new Error(`invalid stable SemVer: ${version}`);
}
if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) {
  throw new Error(`invalid repository identity: ${repository}`);
}

const dist = path.resolve(distArg);
const serverSource = path.resolve(serverSourceArg);
const npmTarball = `bekirdag-pigeonpost-${version}.tgz`;
const expected = [
  "SHA256SUMS",
  npmTarball,
  "pigeonpost-container.txt",
  "pigeonpost-darwin-arm64",
  "pigeonpost-darwin-x64",
  "pigeonpost-linux-arm64",
  "pigeonpost-linux-x64",
  "pigeonpost-win32-arm64.exe",
  "pigeonpost-win32-x64.exe",
  ...nativeSboms.keys(),
  "ppcompliance-darwin-arm64",
  "ppcompliance-darwin-x64",
  "ppcompliance-linux-arm64",
  "ppcompliance-linux-x64",
  "server.json",
].sort();
const actual = fs.readdirSync(dist).sort();
if (JSON.stringify(actual) !== JSON.stringify(expected)) {
  throw new Error(`unexpected final release assets: ${actual.join(", ")}`);
}

for (const name of actual) {
  const stat = fs.lstatSync(path.join(dist, name));
  if (!stat.isFile() || stat.size === 0 || stat.size > 64 * 1024 * 1024) {
    throw new Error(`${name} is not a non-empty regular file within the 64 MiB limit`);
  }
}
if (!fs.readFileSync(path.join(dist, "server.json")).equals(fs.readFileSync(serverSource))) {
  throw new Error("released server.json differs from the tagged source");
}
const manifest = fs
  .readFileSync(path.join(dist, "SHA256SUMS"), "utf8")
  .trim()
  .split("\n");
const checksummed = [];
const manifestDigests = new Map();
for (const line of manifest) {
  const match = /^([a-f0-9]{64})  ([^/]+)$/.exec(line);
  if (!match) throw new Error(`invalid SHA256SUMS line: ${line}`);
  const [, expectedDigest, name] = match;
  if (!actual.includes(name) || name === "SHA256SUMS") {
    throw new Error(`SHA256SUMS names an invalid asset: ${name}`);
  }
  checksummed.push(name);
  manifestDigests.set(name, expectedDigest);
  if (hash(path.join(dist, name)) !== expectedDigest) {
    throw new Error(`SHA256SUMS mismatch: ${name}`);
  }
}
const expectedChecksummed = actual.filter((name) => name !== "SHA256SUMS").sort();
if (JSON.stringify(checksummed.sort()) !== JSON.stringify(expectedChecksummed)) {
  throw new Error("SHA256SUMS does not cover the exact release asset set");
}
for (const [sbomName, artifact] of nativeSboms) {
  let sbom;
  try {
    sbom = JSON.parse(fs.readFileSync(path.join(dist, sbomName), "utf8"));
  } catch (error) {
    throw new Error(`${sbomName} is not valid JSON`, { cause: error });
  }
  verifyNativeSbom({
    artifact,
    artifactDigest: manifestDigests.get(artifact),
    sbom,
    sbomName,
    version,
  });
}

const reference = fs.readFileSync(path.join(dist, "pigeonpost-container.txt"), "utf8");
const referenceParts = reference.trimEnd().split("@");
const expectedImage = `ghcr.io/${repository.toLowerCase()}`;
if (
  referenceParts.length !== 2 ||
  referenceParts[0] !== expectedImage ||
  !/^sha256:[a-f0-9]{64}$/.test(referenceParts[1]) ||
  reference !== `${referenceParts[0]}@${referenceParts[1]}\n`
) {
  throw new Error("released container reference is not the expected immutable digest");
}

const tarball = path.join(dist, npmTarball);
verifyNpmTarball(tarball);
const extractJson = (member) => {
  const raw = execFileSync("tar", ["-xOf", tarball, member], {
    encoding: "utf8",
    maxBuffer: 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
  });
  return JSON.parse(raw);
};
const pkg = extractJson("package/package.json");
const npmChecksums = extractJson("package/checksums.json");
const server = JSON.parse(fs.readFileSync(serverSource, "utf8"));
if (
  pkg.name !== "@bekirdag/pigeonpost" ||
  pkg.version !== version ||
  pkg.mcpName !== server.name ||
  server.version !== version
) {
  throw new Error("released npm or MCP package identity differs from the tag");
}
const onlineAssets = [
  "pigeonpost-darwin-arm64",
  "pigeonpost-darwin-x64",
  "pigeonpost-linux-arm64",
  "pigeonpost-linux-x64",
  "pigeonpost-win32-arm64.exe",
  "pigeonpost-win32-x64.exe",
].sort();
if (JSON.stringify(Object.keys(npmChecksums).sort()) !== JSON.stringify(onlineAssets)) {
  throw new Error("released npm checksums do not cover the exact online binary set");
}
for (const name of onlineAssets) {
  if (npmChecksums[name] !== hash(path.join(dist, name))) {
    throw new Error(`npm checksum mismatch: ${name}`);
  }
}

process.stdout.write(`${referenceParts[0]}@${referenceParts[1]}\n`);
