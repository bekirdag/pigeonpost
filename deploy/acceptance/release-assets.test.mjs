import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const verifier = path.join(repoRoot, "deploy/acceptance/verify-release-assets.mjs");
// Read from the package rather than pinning a literal: this suite is about the *shape* of a
// release — which assets exist, how SBOMs bind to them, what the tarball may contain — none of
// which is version-specific. Hardcoding the number only guarantees the suite breaks on the next
// bump, which is exactly what it did.
const releaseVersion = JSON.parse(
  fs.readFileSync(path.join(repoRoot, "npm/package.json"), "utf8"),
).version;
const onlineAssets = [
  "pigeonpost-darwin-arm64",
  "pigeonpost-darwin-x64",
  "pigeonpost-linux-arm64",
  "pigeonpost-linux-x64",
  "pigeonpost-win32-arm64.exe",
  "pigeonpost-win32-x64.exe",
].sort();
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
const otherAssets = [
  "pigeonpost-container.txt",
  "ppcompliance-darwin-arm64",
  "ppcompliance-darwin-x64",
  "ppcompliance-linux-arm64",
  "ppcompliance-linux-x64",
  ...nativeSboms.keys(),
];

const hash = (file) =>
  crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");

function spdxFixture(artifact, digest) {
  const packageId = `SPDXRef-DocumentRoot-File-${artifact}`;
  const cargoRoot = artifact.startsWith("ppcompliance-")
    ? "pigeonpost-compliance"
    : "pigeonpost-cli";
  const cargoId = `SPDXRef-Cargo-${cargoRoot}`;
  return {
    spdxVersion: "SPDX-2.3",
    dataLicense: "CC0-1.0",
    SPDXID: "SPDXRef-DOCUMENT",
    name: artifact,
    documentNamespace: `https://pigeonpost.dev/sbom/v${releaseVersion}/${artifact}`,
    creationInfo: {
      creators: ["Organization: Anchore, Inc", "Tool: syft-1.50.0"],
      created: "2026-01-01T00:00:00Z",
    },
    packages: [
      {
        name: artifact,
        SPDXID: packageId,
        versionInfo: `sha256:${digest}`,
        supplier: "NOASSERTION",
        downloadLocation: "NOASSERTION",
        filesAnalyzed: false,
        checksums: [{ algorithm: "SHA256", checksumValue: digest }],
        licenseConcluded: "NOASSERTION",
        licenseDeclared: "NOASSERTION",
        copyrightText: "NOASSERTION",
        primaryPackagePurpose: "FILE",
      },
      {
        name: cargoRoot,
        SPDXID: cargoId,
        versionInfo: releaseVersion,
        supplier: "NOASSERTION",
        downloadLocation: "NOASSERTION",
        filesAnalyzed: false,
        licenseConcluded: "NOASSERTION",
        licenseDeclared: "MIT",
        copyrightText: "NOASSERTION",
        primaryPackagePurpose: "APPLICATION",
        externalRefs: [
          {
            referenceCategory: "PACKAGE-MANAGER",
            referenceType: "purl",
            referenceLocator: `pkg:cargo/${cargoRoot}@${releaseVersion}`,
          },
        ],
      },
    ],
    relationships: [
      {
        spdxElementId: "SPDXRef-DOCUMENT",
        relatedSpdxElement: packageId,
        relationshipType: "DESCRIBES",
      },
      {
        spdxElementId: packageId,
        relatedSpdxElement: cargoId,
        relationshipType: "GENERATED_FROM",
      },
    ],
  };
}

function runNpm(root, args) {
  const cache = path.join(root, "npm-cache");
  const home = path.join(root, "home");
  fs.mkdirSync(cache, { recursive: true });
  fs.mkdirSync(home, { recursive: true });
  return spawnSync(process.platform === "win32" ? "npm.cmd" : "npm", args, {
    cwd: root,
    encoding: "utf8",
    env: {
      ...process.env,
      HOME: home,
      npm_config_cache: cache,
      npm_config_globalconfig: path.join(root, "global.npmrc"),
      npm_config_userconfig: path.join(root, "user.npmrc"),
    },
  });
}

function releaseFixture(context, variant = "valid") {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "pigeonpost-release-assets."));
  context.after(() => fs.rmSync(root, { force: true, recursive: true }));
  const dist = path.join(root, "dist");
  const packageRoot = path.join(root, "npm");
  const serverSource = path.join(root, "server.json");
  fs.mkdirSync(dist);
  fs.cpSync(path.join(repoRoot, "npm"), packageRoot, { recursive: true });
  fs.rmSync(path.join(packageRoot, "checksums.json"), { force: true });

  for (const name of [
    ...onlineAssets,
    ...otherAssets.filter((asset) => !nativeSboms.has(asset)),
  ]) {
    const file = path.join(dist, name);
    if (name === "pigeonpost-container.txt") {
      fs.writeFileSync(
        file,
        `ghcr.io/bekirdag/pigeonpost@sha256:${"c".repeat(64)}\n`,
      );
    } else {
      fs.writeFileSync(file, `release fixture for ${name}\n`);
    }
  }
  for (const [sbomName, artifact] of nativeSboms) {
    fs.writeFileSync(
      path.join(dist, sbomName),
      `${JSON.stringify(spdxFixture(artifact, hash(path.join(dist, artifact))))}\n`,
    );
  }

  const targetSbomPath = path.join(dist, "pigeonpost-win32-x64.spdx.json");
  const targetSbom = JSON.parse(fs.readFileSync(targetSbomPath, "utf8"));
  if (variant === "mismatched-sbom") {
    targetSbom.documentNamespace =
      `https://pigeonpost.dev/sbom/v${releaseVersion}/pigeonpost-linux-x64`;
  } else if (variant === "empty-sbom") {
    targetSbom.packages = [];
  } else if (variant === "unrelated-sbom") {
    targetSbom.name = "unrelated-native";
    targetSbom.packages[0].name = "unrelated-native";
  } else if (variant === "wrong-digest-sbom") {
    const wrongDigest = "d".repeat(64);
    targetSbom.packages[0].versionInfo = `sha256:${wrongDigest}`;
    targetSbom.packages[0].checksums[0].checksumValue = wrongDigest;
  } else if (variant === "disconnected-package") {
    targetSbom.packages.push({
      name: "unrelated-library",
      SPDXID: "SPDXRef-Package-unrelated-library",
      versionInfo: "1.0.0",
      downloadLocation: "NOASSERTION",
      filesAnalyzed: false,
      licenseConcluded: "NOASSERTION",
      licenseDeclared: "NOASSERTION",
      copyrightText: "NOASSERTION",
    });
  } else if (variant === "missing-cargo-root") {
    targetSbom.packages.splice(1, 1);
    targetSbom.relationships.splice(1, 1);
  }
  if (
    variant.endsWith("sbom") ||
    variant === "disconnected-package" ||
    variant === "missing-cargo-root"
  ) {
    fs.writeFileSync(targetSbomPath, `${JSON.stringify(targetSbom)}\n`);
  }

  const checksums = Object.fromEntries(
    onlineAssets.map((name) => [name, hash(path.join(dist, name))]),
  );
  fs.writeFileSync(
    path.join(packageRoot, "checksums.json"),
    `${JSON.stringify(checksums, null, 2)}\n`,
    { mode: 0o644 },
  );

  const packageJsonPath = path.join(packageRoot, "package.json");
  const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, "utf8"));
  if (variant === "extra-member") {
    packageJson.files.push("unexpected.txt");
    fs.writeFileSync(path.join(packageRoot, "unexpected.txt"), "not part of the release\n");
  } else if (variant === "non-executable-launcher") {
    delete packageJson.bin;
    fs.chmodSync(path.join(packageRoot, "bin/pigeonpost.js"), 0o644);
  }
  fs.writeFileSync(packageJsonPath, `${JSON.stringify(packageJson, null, 2)}\n`);

  fs.copyFileSync(path.join(repoRoot, "server.json"), serverSource);
  fs.copyFileSync(serverSource, path.join(dist, "server.json"));

  const packed = runNpm(root, [
    "--silent",
    "pack",
    packageRoot,
    "--pack-destination",
    dist,
    "--json",
  ]);
  assert.equal(packed.status, 0, `${packed.stdout}\n${packed.stderr}`);
  assert.equal(
    fs.existsSync(path.join(dist, `bekirdag-pigeonpost-${releaseVersion}.tgz`)),
    true,
  );

  const manifest = fs
    .readdirSync(dist)
    .sort()
    .map((name) => `${hash(path.join(dist, name))}  ${name}`);
  fs.writeFileSync(path.join(dist, "SHA256SUMS"), `${manifest.join("\n")}\n`);
  return { dist, root, serverSource };
}

function runVerifier({ dist, root, serverSource }) {
  return spawnSync(
    process.execPath,
    [verifier, dist, releaseVersion, "bekirdag/pigeonpost", serverSource],
    {
      cwd: root,
      encoding: "utf8",
      env: { ...process.env, LC_ALL: "C" },
    },
  );
}

test("release verification accepts the exact npm package and useful per-target SBOMs", (context) => {
  const result = runVerifier(releaseFixture(context));
  assert.equal(result.status, 0, result.stderr);
  assert.equal(
    result.stdout,
    `ghcr.io/bekirdag/pigeonpost@sha256:${"c".repeat(64)}\n`,
  );
});

test("release verification rejects an extra npm tar member", (context) => {
  const result = runVerifier(releaseFixture(context, "extra-member"));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /unexpected npm package members/);
});

test("release verification rejects a non-executable npm launcher", (context) => {
  const result = runVerifier(releaseFixture(context, "non-executable-launcher"));
  assert.notEqual(result.status, 0);
  assert.match(
    result.stderr,
    /package\/bin\/pigeonpost\.js must be a regular file with mode 0755/,
  );
});

test("release verification rejects an SBOM bound to another native asset", (context) => {
  const result = runVerifier(releaseFixture(context, "mismatched-sbom"));
  assert.notEqual(result.status, 0);
  assert.match(
    result.stderr,
    /pigeonpost-win32-x64\.spdx\.json is not the canonical SBOM/,
  );
});

test("release verification rejects an empty SBOM package inventory", (context) => {
  const result = runVerifier(releaseFixture(context, "empty-sbom"));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /must contain the artifact and its SPDX dependency inventory/);
});

test("release verification rejects an SBOM without its Cargo root", (context) => {
  const result = runVerifier(releaseFixture(context, "missing-cargo-root"));
  assert.notEqual(result.status, 0);
  assert.match(
    result.stderr,
    /must contain the artifact and its SPDX dependency inventory|must contain exactly one pigeonpost-cli Cargo root/,
  );
});

test("release verification rejects an SBOM for an unrelated artifact", (context) => {
  const result = runVerifier(releaseFixture(context, "unrelated-sbom"));
  assert.notEqual(result.status, 0);
  assert.match(
    result.stderr,
    /pigeonpost-win32-x64\.spdx\.json is not the canonical SBOM/,
  );
});

test("release verification rejects an SBOM with the wrong artifact digest", (context) => {
  const result = runVerifier(releaseFixture(context, "wrong-digest-sbom"));
  assert.notEqual(result.status, 0);
  assert.match(
    result.stderr,
    /root package digest does not match SHA256SUMS for pigeonpost-win32-x64\.exe/,
  );
});

test("release verification rejects disconnected package inventory", (context) => {
  const result = runVerifier(releaseFixture(context, "disconnected-package"));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /contains package inventory unrelated to/);
});
