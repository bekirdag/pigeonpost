#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const nativeSboms = new Map([
  [
    "pigeonpost-darwin-arm64.spdx.json",
    {
      artifact: "pigeonpost-darwin-arm64",
      cargoRoot: "pigeonpost-cli",
      target: "aarch64-apple-darwin",
    },
  ],
  [
    "pigeonpost-darwin-x64.spdx.json",
    {
      artifact: "pigeonpost-darwin-x64",
      cargoRoot: "pigeonpost-cli",
      target: "x86_64-apple-darwin",
    },
  ],
  [
    "pigeonpost-linux-arm64.spdx.json",
    {
      artifact: "pigeonpost-linux-arm64",
      cargoRoot: "pigeonpost-cli",
      target: "aarch64-unknown-linux-musl",
    },
  ],
  [
    "pigeonpost-linux-x64.spdx.json",
    {
      artifact: "pigeonpost-linux-x64",
      cargoRoot: "pigeonpost-cli",
      target: "x86_64-unknown-linux-musl",
    },
  ],
  [
    "pigeonpost-win32-arm64.spdx.json",
    {
      artifact: "pigeonpost-win32-arm64.exe",
      cargoRoot: "pigeonpost-cli",
      target: "aarch64-pc-windows-msvc",
    },
  ],
  [
    "pigeonpost-win32-x64.spdx.json",
    {
      artifact: "pigeonpost-win32-x64.exe",
      cargoRoot: "pigeonpost-cli",
      target: "x86_64-pc-windows-msvc",
    },
  ],
  [
    "ppcompliance-darwin-arm64.spdx.json",
    {
      artifact: "ppcompliance-darwin-arm64",
      cargoRoot: "pigeonpost-compliance",
      target: "aarch64-apple-darwin",
    },
  ],
  [
    "ppcompliance-darwin-x64.spdx.json",
    {
      artifact: "ppcompliance-darwin-x64",
      cargoRoot: "pigeonpost-compliance",
      target: "x86_64-apple-darwin",
    },
  ],
  [
    "ppcompliance-linux-arm64.spdx.json",
    {
      artifact: "ppcompliance-linux-arm64",
      cargoRoot: "pigeonpost-compliance",
      target: "aarch64-unknown-linux-musl",
    },
  ],
  [
    "ppcompliance-linux-x64.spdx.json",
    {
      artifact: "ppcompliance-linux-x64",
      cargoRoot: "pigeonpost-compliance",
      target: "x86_64-unknown-linux-musl",
    },
  ],
]);

const rootId = "SPDXRef-Pigeonpost-Release-Artifact";
const validSpdxId = /^SPDXRef-[A-Za-z0-9.-]+$/;
const stableSemver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

const isRecord = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);

function regularFile(file, label) {
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.size === 0 || stat.size > 64 * 1024 * 1024) {
    throw new Error(`${label} must be a non-empty regular file within 64 MiB`);
  }
}

function sha256(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function cargoPackageKey(pkg) {
  return pkg.source
    ? `${pkg.source}#${pkg.name}@${pkg.version}`
    : `workspace:${pkg.name}@${pkg.version}`;
}

function cargoSpdxId(pkg) {
  const digest = crypto
    .createHash("sha256")
    .update(cargoPackageKey(pkg))
    .digest("hex")
    .slice(0, 32);
  return `SPDXRef-Cargo-${digest}`;
}

function cargoInventory(metadata, cargoRoot, sbomName) {
  if (
    !isRecord(metadata) ||
    !Array.isArray(metadata.packages) ||
    !isRecord(metadata.resolve) ||
    !Array.isArray(metadata.resolve.nodes)
  ) {
    throw new Error(`${sbomName} Cargo metadata is incomplete`);
  }
  const packagesById = new Map();
  for (const pkg of metadata.packages) {
    if (
      !isRecord(pkg) ||
      typeof pkg.id !== "string" ||
      typeof pkg.name !== "string" ||
      typeof pkg.version !== "string" ||
      packagesById.has(pkg.id)
    ) {
      throw new Error(`${sbomName} Cargo metadata contains an invalid package`);
    }
    packagesById.set(pkg.id, pkg);
  }
  const nodesById = new Map();
  for (const node of metadata.resolve.nodes) {
    if (
      !isRecord(node) ||
      typeof node.id !== "string" ||
      !Array.isArray(node.deps) ||
      nodesById.has(node.id)
    ) {
      throw new Error(`${sbomName} Cargo metadata contains an invalid resolve node`);
    }
    nodesById.set(node.id, node);
  }
  const roots = metadata.packages.filter(
    (pkg) => pkg.name === cargoRoot && pkg.source === null,
  );
  if (roots.length !== 1) {
    throw new Error(`${sbomName} does not resolve exactly one workspace ${cargoRoot} root`);
  }

  const reachable = new Set();
  const pending = [roots[0].id];
  const dependencies = new Map();
  while (pending.length > 0) {
    const packageId = pending.pop();
    if (reachable.has(packageId)) continue;
    const pkg = packagesById.get(packageId);
    const node = nodesById.get(packageId);
    if (!pkg || !node) {
      throw new Error(`${sbomName} Cargo resolve graph references an unknown package`);
    }
    reachable.add(packageId);
    for (const dependency of node.deps) {
      if (
        !isRecord(dependency) ||
        typeof dependency.pkg !== "string" ||
        !Array.isArray(dependency.dep_kinds)
      ) {
        throw new Error(`${sbomName} Cargo resolve graph contains an invalid dependency`);
      }
      const productionKinds = dependency.dep_kinds.filter(
        (kind) => isRecord(kind) && kind.kind !== "dev",
      );
      if (productionKinds.length === 0) continue;
      if (!packagesById.has(dependency.pkg) || !nodesById.has(dependency.pkg)) {
        throw new Error(`${sbomName} Cargo resolve graph references an unknown dependency`);
      }
      const edge = `${packageId}\u0000${dependency.pkg}`;
      dependencies.set(edge, {
        dependency: dependency.pkg,
        package: packageId,
        runtime: productionKinds.some((kind) => kind.kind === null),
      });
      pending.push(dependency.pkg);
    }
  }
  if (reachable.size < 2) {
    throw new Error(`${sbomName} Cargo inventory has no production dependency graph`);
  }

  const spdxIds = new Map();
  const packages = [...reachable]
    .map((id) => packagesById.get(id))
    .sort((left, right) => cargoPackageKey(left).localeCompare(cargoPackageKey(right)))
    .map((pkg) => {
      const SPDXID = cargoSpdxId(pkg);
      if ([...spdxIds.values()].includes(SPDXID)) {
        throw new Error(`${sbomName} Cargo package id collision`);
      }
      spdxIds.set(pkg.id, SPDXID);
      const result = {
        name: pkg.name,
        SPDXID,
        versionInfo: pkg.version,
        supplier: "NOASSERTION",
        downloadLocation: "NOASSERTION",
        filesAnalyzed: false,
        licenseConcluded: "NOASSERTION",
        licenseDeclared:
          typeof pkg.license === "string" && pkg.license.trim().length > 0
            ? pkg.license
            : "NOASSERTION",
        copyrightText: "NOASSERTION",
        primaryPackagePurpose: pkg.id === roots[0].id ? "APPLICATION" : "LIBRARY",
        externalRefs: [
          {
            referenceCategory: "PACKAGE-MANAGER",
            referenceType: "purl",
            referenceLocator: `pkg:cargo/${encodeURIComponent(pkg.name)}@${encodeURIComponent(pkg.version)}`,
          },
        ],
      };
      if (typeof pkg.checksum === "string" && /^[a-f0-9]{64}$/.test(pkg.checksum)) {
        result.checksums = [{ algorithm: "SHA256", checksumValue: pkg.checksum }];
      }
      return result;
    });
  const relationships = [...dependencies.values()]
    .filter(
      (dependency) =>
        reachable.has(dependency.package) && reachable.has(dependency.dependency),
    )
    .sort((left, right) => {
      const leftKey = `${spdxIds.get(left.package)}\u0000${spdxIds.get(left.dependency)}`;
      const rightKey = `${spdxIds.get(right.package)}\u0000${spdxIds.get(right.dependency)}`;
      return leftKey.localeCompare(rightKey);
    })
    .map((dependency) =>
      dependency.runtime
        ? {
            spdxElementId: spdxIds.get(dependency.package),
            relatedSpdxElement: spdxIds.get(dependency.dependency),
            relationshipType: "DEPENDS_ON",
          }
        : {
            spdxElementId: spdxIds.get(dependency.dependency),
            relatedSpdxElement: spdxIds.get(dependency.package),
            relationshipType: "BUILD_DEPENDENCY_OF",
          },
    );
  return {
    packages,
    relationships,
    rootPackageId: spdxIds.get(roots[0].id),
  };
}

function normalizeOne({ artifact, cargoMetadata, cargoRoot, created, dist, sbomName, version }) {
  const artifactFile = path.join(dist, artifact);
  const sbomFile = path.join(dist, sbomName);
  regularFile(artifactFile, artifact);
  regularFile(sbomFile, sbomName);

  const sbom = JSON.parse(fs.readFileSync(sbomFile, "utf8"));
  if (
    !isRecord(sbom) ||
    sbom.spdxVersion !== "SPDX-2.3" ||
    sbom.dataLicense !== "CC0-1.0" ||
    sbom.SPDXID !== "SPDXRef-DOCUMENT" ||
    !isRecord(sbom.creationInfo) ||
    !Array.isArray(sbom.creationInfo.creators) ||
    sbom.creationInfo.creators.length === 0 ||
    !sbom.creationInfo.creators.every(
      (creator) => typeof creator === "string" && creator.trim().length > 0,
    )
  ) {
    throw new Error(`${sbomName} is not an SPDX 2.3 document with creator evidence`);
  }
  if (!Array.isArray(sbom.packages) || sbom.packages.length === 0) {
    throw new Error(`${sbomName} has no Syft package inventory to bind`);
  }
  if (!Array.isArray(sbom.relationships)) {
    throw new Error(`${sbomName} has no SPDX relationship array`);
  }
  const cargo = cargoInventory(cargoMetadata, cargoRoot, sbomName);

  const packageIds = new Set();
  for (const component of sbom.packages) {
    if (
      !isRecord(component) ||
      typeof component.SPDXID !== "string" ||
      !validSpdxId.test(component.SPDXID) ||
      component.SPDXID === "SPDXRef-DOCUMENT" ||
      component.SPDXID === rootId ||
      packageIds.has(component.SPDXID)
    ) {
      throw new Error(`${sbomName} contains an invalid, duplicate, or reserved package id`);
    }
    packageIds.add(component.SPDXID);
  }
  for (const relationship of sbom.relationships) {
    if (
      !isRecord(relationship) ||
      typeof relationship.spdxElementId !== "string" ||
      typeof relationship.relatedSpdxElement !== "string" ||
      typeof relationship.relationshipType !== "string" ||
      relationship.spdxElementId === rootId ||
      relationship.relatedSpdxElement === rootId
    ) {
      throw new Error(`${sbomName} contains an invalid or reserved relationship`);
    }
  }

  const digest = sha256(artifactFile);
  const rootPackage = {
    name: artifact,
    SPDXID: rootId,
    versionInfo: `sha256:${digest}`,
    supplier: "NOASSERTION",
    downloadLocation: "NOASSERTION",
    filesAnalyzed: false,
    checksums: [{ algorithm: "SHA256", checksumValue: digest }],
    licenseConcluded: "NOASSERTION",
    licenseDeclared: "NOASSERTION",
    copyrightText: "NOASSERTION",
    primaryPackagePurpose: "FILE",
  };

  sbom.name = artifact;
  sbom.documentNamespace = `https://pigeonpost.dev/sbom/v${version}/${artifact}`;
  sbom.creationInfo.created = created;
  const syftPackageIds = [...packageIds].sort();
  for (const pkg of cargo.packages) {
    if (packageIds.has(pkg.SPDXID)) {
      throw new Error(`${sbomName} Cargo and Syft package ids collide`);
    }
    packageIds.add(pkg.SPDXID);
  }
  sbom.packages = [rootPackage, ...sbom.packages, ...cargo.packages];
  sbom.relationships = [
    ...sbom.relationships.filter(
      (relationship) =>
        !(
          relationship.spdxElementId === "SPDXRef-DOCUMENT" &&
          relationship.relationshipType === "DESCRIBES"
        ),
    ),
    {
      spdxElementId: "SPDXRef-DOCUMENT",
      relatedSpdxElement: rootId,
      relationshipType: "DESCRIBES",
    },
    ...syftPackageIds.map((packageId) => ({
      spdxElementId: rootId,
      relatedSpdxElement: packageId,
      relationshipType: "CONTAINS",
    })),
    {
      spdxElementId: rootId,
      relatedSpdxElement: cargo.rootPackageId,
      relationshipType: "GENERATED_FROM",
    },
    ...cargo.relationships,
  ];
  fs.writeFileSync(sbomFile, `${JSON.stringify(sbom, null, 2)}\n`);
}

const [distArg, version, created, metadataArg] = process.argv.slice(2);
if (!distArg || !version || !created || !metadataArg) {
  throw new Error(
    "usage: normalize-release-sboms.mjs <dist-dir> <stable-version> <created-at> <cargo-metadata-dir>",
  );
}
if (!stableSemver.test(version)) {
  throw new Error(`invalid stable SemVer: ${version}`);
}
if (Number.isNaN(Date.parse(created))) {
  throw new Error(`invalid creation timestamp: ${created}`);
}

const dist = path.resolve(distArg);
const metadataDir = path.resolve(metadataArg);
if (!fs.lstatSync(dist).isDirectory()) {
  throw new Error(`${dist} is not a directory`);
}
if (!fs.lstatSync(metadataDir).isDirectory()) {
  throw new Error(`${metadataDir} is not a directory`);
}
const metadataByTarget = new Map();
for (const [sbomName, { artifact, cargoRoot, target }] of nativeSboms) {
  if (!metadataByTarget.has(target)) {
    const metadataFile = path.join(metadataDir, `${target}.json`);
    regularFile(metadataFile, `${target} Cargo metadata`);
    metadataByTarget.set(target, JSON.parse(fs.readFileSync(metadataFile, "utf8")));
  }
  normalizeOne({
    artifact,
    cargoMetadata: metadataByTarget.get(target),
    cargoRoot,
    created,
    dist,
    sbomName,
    version,
  });
}
