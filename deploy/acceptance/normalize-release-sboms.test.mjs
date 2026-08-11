import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const normalizer = path.join(
  repoRoot,
  "deploy/acceptance/normalize-release-sboms.mjs",
);
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
const created = "2026-08-08T12:34:56Z";
const rootId = "SPDXRef-Pigeonpost-Release-Artifact";
const targets = [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "aarch64-unknown-linux-musl",
  "x86_64-unknown-linux-musl",
  "aarch64-pc-windows-msvc",
  "x86_64-pc-windows-msvc",
];

function cargoMetadata() {
  const cli = "path+file:///workspace/crates/pigeonpost-cli#0.2.0";
  const compliance = "path+file:///workspace/crates/pigeonpost-compliance#0.2.0";
  const serde = "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.229";
  const cc = "registry+https://github.com/rust-lang/crates.io-index#cc@1.4.1";
  const tempfile = "registry+https://github.com/rust-lang/crates.io-index#tempfile@3.23.0";
  const cargoPackage = (id, name, version, source, checksum = null) => ({
    id,
    name,
    version,
    source,
    checksum,
    license: "MIT OR Apache-2.0",
  });
  const dependency = (pkg, kind) => ({
    name: pkg.split("#").at(-1).split("@")[0],
    pkg,
    dep_kinds: [{ kind, target: null }],
  });
  return {
    packages: [
      cargoPackage(cli, "pigeonpost-cli", "0.2.0", null),
      cargoPackage(compliance, "pigeonpost-compliance", "0.2.0", null),
      cargoPackage(serde, "serde", "1.0.229", "registry+https://github.com/rust-lang/crates.io-index", "a".repeat(64)),
      cargoPackage(cc, "cc", "1.4.1", "registry+https://github.com/rust-lang/crates.io-index", "b".repeat(64)),
      cargoPackage(tempfile, "tempfile", "3.23.0", "registry+https://github.com/rust-lang/crates.io-index", "c".repeat(64)),
    ],
    resolve: {
      nodes: [
        {
          id: cli,
          deps: [
            dependency(serde, null),
            dependency(cc, "build"),
            dependency(tempfile, "dev"),
          ],
        },
        {
          id: compliance,
          deps: [
            dependency(serde, null),
            dependency(cc, "build"),
            dependency(tempfile, "dev"),
          ],
        },
        { id: serde, deps: [] },
        { id: cc, deps: [] },
        { id: tempfile, deps: [] },
      ],
    },
  };
}

function fixture(context, variant = "valid") {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "pigeonpost-sbom-normalize."));
  context.after(() => fs.rmSync(root, { force: true, recursive: true }));
  const metadataDir = path.join(root, "cargo-metadata");
  fs.mkdirSync(metadataDir);
  for (const target of targets) {
    const metadata = cargoMetadata();
    if (variant === "no-production-dependencies" && target === "x86_64-pc-windows-msvc") {
      metadata.resolve.nodes.find((node) => node.id.includes("pigeonpost-cli")).deps = [
        metadata.resolve.nodes
          .find((node) => node.id.includes("pigeonpost-cli"))
          .deps.find((dependency) => dependency.dep_kinds[0].kind === "dev"),
      ];
    }
    fs.writeFileSync(
      path.join(metadataDir, `${target}.json`),
      `${JSON.stringify(metadata)}\n`,
    );
  }
  for (const [sbomName, artifact] of nativeSboms) {
    fs.writeFileSync(path.join(root, artifact), `exact bytes for ${artifact}\n`);
    const componentId = `SPDXRef-Package-${artifact.replace(/[^A-Za-z0-9.-]/g, "-")}`;
    const packages = [
      {
        name: "pigeonpost-component",
        SPDXID: componentId,
        versionInfo: "0.2.0",
        downloadLocation: "NOASSERTION",
        filesAnalyzed: false,
        licenseConcluded: "NOASSERTION",
        licenseDeclared: "NOASSERTION",
        copyrightText: "NOASSERTION",
      },
    ];
    if (variant === "empty" && artifact === "pigeonpost-win32-x64.exe") {
      packages.length = 0;
    }
    if (variant === "reserved" && artifact === "pigeonpost-win32-x64.exe") {
      packages[0].SPDXID = rootId;
    }
    fs.writeFileSync(
      path.join(root, sbomName),
      `${JSON.stringify({
        spdxVersion: "SPDX-2.3",
        dataLicense: "CC0-1.0",
        SPDXID: "SPDXRef-DOCUMENT",
        name: "unbound-syft-output",
        documentNamespace: "https://example.invalid/unbound",
        creationInfo: {
          creators: ["Tool: syft-1.50.0"],
          created: "2020-01-01T00:00:00Z",
        },
        packages,
        relationships:
          packages.length === 0
            ? []
            : [
                {
                  spdxElementId: "SPDXRef-DOCUMENT",
                  relatedSpdxElement: packages[0].SPDXID,
                  relationshipType: "DESCRIBES",
                },
              ],
      })}\n`,
    );
  }
  return root;
}

function run(root, version = "0.2.0") {
  return spawnSync(
    process.execPath,
    [normalizer, root, version, created, path.join(root, "cargo-metadata")],
    { encoding: "utf8" },
  );
}

test("normalizer binds every Syft inventory to its exact release artifact", (context) => {
  const root = fixture(context);
  const result = run(root);
  assert.equal(result.status, 0, result.stderr);
  for (const [sbomName, artifact] of nativeSboms) {
    const sbom = JSON.parse(fs.readFileSync(path.join(root, sbomName), "utf8"));
    const digest = crypto
      .createHash("sha256")
      .update(fs.readFileSync(path.join(root, artifact)))
      .digest("hex");
    assert.equal(sbom.name, artifact);
    assert.equal(sbom.documentNamespace, `https://pigeonpost.dev/sbom/v0.2.0/${artifact}`);
    assert.equal(sbom.creationInfo.created, created);
    assert.equal(sbom.packages.length, 5);
    assert.deepEqual(sbom.packages[0], {
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
    });
    assert.deepEqual(
      sbom.relationships.filter(
        (relationship) =>
          relationship.spdxElementId === "SPDXRef-DOCUMENT" &&
          relationship.relationshipType === "DESCRIBES",
      ),
      [
        {
          spdxElementId: "SPDXRef-DOCUMENT",
          relatedSpdxElement: rootId,
          relationshipType: "DESCRIBES",
        },
      ],
    );
    assert.equal(
      sbom.relationships.some(
        (relationship) =>
          relationship.spdxElementId === rootId &&
          relationship.relatedSpdxElement === sbom.packages[1].SPDXID &&
          relationship.relationshipType === "CONTAINS",
      ),
      true,
    );
    const packageNames = sbom.packages.map((pkg) => pkg.name);
    assert.equal(packageNames.includes("serde"), true);
    assert.equal(packageNames.includes("cc"), true);
    assert.equal(packageNames.includes("tempfile"), false);
    assert.equal(
      packageNames.includes(
        artifact.startsWith("ppcompliance")
          ? "pigeonpost-compliance"
          : "pigeonpost-cli",
      ),
      true,
    );
    assert.equal(
      packageNames.includes(
        artifact.startsWith("ppcompliance")
          ? "pigeonpost-cli"
          : "pigeonpost-compliance",
      ),
      false,
    );
    assert.equal(
      sbom.relationships.some(
        (relationship) => relationship.relationshipType === "BUILD_DEPENDENCY_OF",
      ),
      true,
    );
  }
});

test("normalizer rejects an empty generated inventory", (context) => {
  const result = run(fixture(context, "empty"));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /has no Syft package inventory to bind/);
});

test("normalizer rejects a generated package that collides with the reserved root", (context) => {
  const result = run(fixture(context, "reserved"));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /invalid, duplicate, or reserved package id/);
});

test("normalizer rejects Cargo metadata without a production dependency graph", (context) => {
  const result = run(fixture(context, "no-production-dependencies"));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Cargo inventory has no production dependency graph/);
});

test("normalizer rejects a non-stable release version", (context) => {
  const result = run(fixture(context), "0.2.0-rc.1");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /invalid stable SemVer/);
});
