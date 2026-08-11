import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const verifier = path.join(repoRoot, "deploy/acceptance/verify-npm-provenance.mjs");
const packageName = "@bekirdag/pigeonpost";
const version = "0.2.0";
const repository = "bekirdag/pigeonpost";
const workflow = ".github/workflows/release.yml";
const ref = "refs/tags/v0.2.0";
const sourceSha = "a".repeat(40);
const provenanceType = "https://slsa.dev/provenance/v1";

const encode = (value) => Buffer.from(JSON.stringify(value), "utf8").toString("base64");
const sha512 = (value) => crypto.createHash("sha512").update(value).digest("hex");

function statementFixture(tarballDigest) {
  return {
    _type: "https://in-toto.io/Statement/v1",
    subject: [
      {
        name: "pkg:npm/%40bekirdag/pigeonpost@0.2.0",
        digest: { sha512: tarballDigest },
      },
    ],
    predicateType: provenanceType,
    predicate: {
      buildDefinition: {
        buildType: "https://slsa-framework.github.io/github-actions-buildtypes/workflow/v1",
        externalParameters: {
          workflow: {
            ref,
            repository: "https://github.com/bekirdag/pigeonpost",
            path: workflow,
          },
        },
        internalParameters: {
          github: {
            event_name: "push",
            repository_id: "1234",
            repository_owner_id: "5678",
          },
        },
        resolvedDependencies: [
          {
            uri: "git+https://github.com/bekirdag/pigeonpost@refs/tags/v0.2.0",
            digest: { gitCommit: sourceSha },
          },
        ],
      },
      runDetails: {
        builder: { id: "https://github.com/actions/runner/github-hosted" },
        metadata: {
          invocationId: "https://github.com/bekirdag/pigeonpost/actions/runs/123/attempts/1",
        },
      },
    },
  };
}

function fixture(context, mutate = () => undefined) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "pigeonpost-npm-provenance."));
  context.after(() => fs.rmSync(root, { force: true, recursive: true }));
  const auditFile = path.join(root, "audit.json");
  const tarballFile = path.join(root, "bekirdag-pigeonpost-0.2.0.tgz");
  const tarball = Buffer.from("immutable npm release tarball fixture\n", "utf8");
  fs.writeFileSync(tarballFile, tarball);

  const statement = statementFixture(sha512(tarball));
  const provenanceBundle = {
    predicateType: provenanceType,
    bundle: {
      mediaType: "application/vnd.dev.sigstore.bundle.v0.3+json",
      verificationMaterial: { fixture: "already cryptographically verified by npm" },
      dsseEnvelope: {
        payload: "",
        payloadType: "application/vnd.in-toto+json",
        signatures: [{ keyid: "", sig: Buffer.from("verified signature").toString("base64") }],
      },
    },
  };
  const audit = {
    invalid: [],
    missing: [],
    verified: [
      {
        name: packageName,
        version,
        location: "node_modules/@bekirdag/pigeonpost",
        registry: "https://registry.npmjs.org/",
        attestations: {
          url: "https://registry.npmjs.org/-/npm/v1/attestations/@bekirdag%2fpigeonpost@0.2.0",
          provenance: { predicateType: provenanceType },
        },
        attestationBundles: [
          {
            predicateType: "https://github.com/npm/attestation/tree/main/specs/publish/v0.1",
            bundle: { fixture: "verified npm publish attestation" },
          },
          provenanceBundle,
        ],
      },
    ],
  };

  const mutationResult = mutate({ audit, provenanceBundle, statement, tarballFile });
  const payloadOverride =
    mutationResult !== null &&
    typeof mutationResult === "object" &&
    Object.hasOwn(mutationResult, "payloadOverride")
      ? mutationResult.payloadOverride
      : undefined;
  provenanceBundle.bundle.dsseEnvelope.payload =
    payloadOverride === undefined ? encode(statement) : payloadOverride;
  fs.writeFileSync(auditFile, `${JSON.stringify(audit)}\n`);
  return { auditFile, root, tarballFile };
}

function run({ auditFile, root, tarballFile }, args = {}) {
  return spawnSync(
    process.execPath,
    [
      verifier,
      auditFile,
      tarballFile,
      args.packageName ?? packageName,
      args.version ?? version,
      args.repository ?? repository,
      args.workflow ?? workflow,
      args.ref ?? ref,
      args.sourceSha ?? sourceSha,
    ],
    { cwd: root, encoding: "utf8", env: { ...process.env, LC_ALL: "C" } },
  );
}

function expectFailure(context, mutate, pattern, args) {
  const result = run(fixture(context, mutate), args);
  assert.notEqual(result.status, 0, result.stdout);
  assert.match(result.stderr, pattern);
}

test("accepts one npm-verified package with an exact GitHub release identity", (context) => {
  const result = run(fixture(context));
  assert.equal(result.status, 0, result.stderr);
  assert.equal(
    result.stdout,
    `verified npm provenance for ${packageName}@${version} from ${sourceSha}\n`,
  );
});

const auditBoundaryCases = [
  {
    name: "rejects a non-empty invalid result set",
    pattern: /contains invalid/,
    mutate: ({ audit }) => audit.invalid.push({ code: "EATTESTATIONVERIFY" }),
  },
  {
    name: "rejects a non-empty missing result set",
    pattern: /contains missing/,
    mutate: ({ audit }) => audit.missing.push({ name: packageName }),
  },
  {
    name: "rejects duplicate verified package entries",
    pattern: /exactly one verified package/,
    mutate: ({ audit }) => audit.verified.push(structuredClone(audit.verified[0])),
  },
  {
    name: "rejects the wrong verified package",
    pattern: /package, version, or registry/,
    mutate: ({ audit }) => (audit.verified[0].name = "@bekirdag/not-pigeonpost"),
  },
  {
    name: "rejects the wrong verified version",
    pattern: /package, version, or registry/,
    mutate: ({ audit }) => (audit.verified[0].version = "0.2.1"),
  },
  {
    name: "rejects a lookalike npm registry origin",
    pattern: /package, version, or registry/,
    mutate: ({ audit }) =>
      (audit.verified[0].registry = "https://registry.npmjs.org.attacker.invalid/"),
  },
  {
    name: "rejects missing attestation bundles",
    pattern: /does not contain attestation bundles/,
    mutate: ({ audit }) => delete audit.verified[0].attestationBundles,
  },
  {
    name: "rejects duplicate SLSA provenance bundles",
    pattern: /exactly one SLSA provenance v1 bundle/,
    mutate: ({ audit, provenanceBundle }) =>
      audit.verified[0].attestationBundles.push(structuredClone(provenanceBundle)),
  },
];

for (const { name, mutate, pattern } of auditBoundaryCases) {
  test(name, (context) => expectFailure(context, mutate, pattern));
}

const envelopeCases = [
  {
    name: "rejects an unsupported Sigstore bundle media type",
    pattern: /supported complete Sigstore bundle/,
    mutate: ({ provenanceBundle }) =>
      (provenanceBundle.bundle.mediaType = "application/json"),
  },
  {
    name: "rejects a non-in-toto DSSE payload type",
    pattern: /payload type is not in-toto JSON/,
    mutate: ({ provenanceBundle }) =>
      (provenanceBundle.bundle.dsseEnvelope.payloadType = "application/json"),
  },
  {
    name: "rejects a DSSE envelope without exactly one signature",
    pattern: /exactly one signature/,
    mutate: ({ provenanceBundle }) =>
      (provenanceBundle.bundle.dsseEnvelope.signatures = []),
  },
  {
    name: "rejects a malformed DSSE signature",
    pattern: /signature is not canonical base64/,
    mutate: ({ provenanceBundle }) =>
      (provenanceBundle.bundle.dsseEnvelope.signatures[0].sig = "***"),
  },
  {
    name: "rejects malformed DSSE payload base64",
    pattern: /payload is not canonical base64/,
    mutate: () => ({ payloadOverride: "***" }),
  },
  {
    name: "rejects non-canonical DSSE payload base64",
    pattern: /payload is not canonical base64/,
    mutate: ({ statement }) => ({ payloadOverride: `${encode(statement)}\n` }),
  },
  {
    name: "rejects non-UTF-8 DSSE payload bytes",
    pattern: /payload is not valid UTF-8/,
    mutate: () => ({ payloadOverride: Buffer.from([0xff]).toString("base64") }),
  },
  {
    name: "rejects a DSSE payload that is not JSON",
    pattern: /payload is not valid JSON/,
    mutate: () => ({
      payloadOverride: Buffer.from("not json", "utf8").toString("base64"),
    }),
  },
];

for (const { name, mutate, pattern } of envelopeCases) {
  test(name, (context) => expectFailure(context, mutate, pattern));
}

const identityCases = [
  {
    name: "rejects a non-v1 in-toto statement",
    pattern: /not an in-toto v1 statement/,
    mutate: ({ statement }) => (statement._type = "https://in-toto.io/Statement/v0.1"),
  },
  {
    name: "rejects an inner predicate type mismatch",
    pattern: /signed statement is not SLSA provenance v1/,
    mutate: ({ statement }) =>
      (statement.predicateType = "https://slsa.dev/provenance/v0.2"),
  },
  {
    name: "rejects duplicate signed subjects",
    pattern: /exactly one subject/,
    mutate: ({ statement }) => statement.subject.push(structuredClone(statement.subject[0])),
  },
  {
    name: "rejects a different npm purl",
    pattern: /does not bind pkg:npm/,
    mutate: ({ statement }) =>
      (statement.subject[0].name = "pkg:npm/%40bekirdag/not-pigeonpost@0.2.0"),
  },
  {
    name: "rejects a tarball digest mismatch",
    pattern: /exact npm tarball/,
    mutate: ({ statement }) => (statement.subject[0].digest.sha512 = "b".repeat(128)),
  },
  {
    name: "rejects a different GitHub workflow build type",
    pattern: /workflow\/v1 build type/,
    mutate: ({ statement }) =>
      (statement.predicate.buildDefinition.buildType = "https://github.com/npm/cli/gha/v2"),
  },
  {
    name: "rejects a different source repository",
    pattern: /workflow repository, path, or ref/,
    mutate: ({ statement }) =>
      (statement.predicate.buildDefinition.externalParameters.workflow.repository =
        "https://github.com/attacker/pigeonpost"),
  },
  {
    name: "rejects a different workflow path",
    pattern: /workflow repository, path, or ref/,
    mutate: ({ statement }) =>
      (statement.predicate.buildDefinition.externalParameters.workflow.path =
        ".github/workflows/untrusted.yml"),
  },
  {
    name: "rejects a different source ref",
    pattern: /workflow repository, path, or ref/,
    mutate: ({ statement }) =>
      (statement.predicate.buildDefinition.externalParameters.workflow.ref =
        "refs/heads/main"),
  },
  {
    name: "rejects multiple resolved source dependencies",
    pattern: /exactly one resolved source dependency/,
    mutate: ({ statement }) =>
      statement.predicate.buildDefinition.resolvedDependencies.push(
        structuredClone(statement.predicate.buildDefinition.resolvedDependencies[0]),
      ),
  },
  {
    name: "rejects a different resolved source URI",
    pattern: /resolved source URI or Git commit/,
    mutate: ({ statement }) =>
      (statement.predicate.buildDefinition.resolvedDependencies[0].uri =
        "git+https://github.com/attacker/pigeonpost@refs/tags/v0.2.0"),
  },
  {
    name: "rejects a different resolved source commit",
    pattern: /resolved source URI or Git commit/,
    mutate: ({ statement }) =>
      (statement.predicate.buildDefinition.resolvedDependencies[0].digest.gitCommit =
        "b".repeat(40)),
  },
  {
    name: "rejects a self-hosted runner",
    pattern: /not produced by a GitHub-hosted runner/,
    mutate: ({ statement }) =>
      (statement.predicate.runDetails.builder.id =
        "https://github.com/actions/runner/self-hosted"),
  },
  {
    name: "rejects an invocation scoped to another repository",
    pattern: /invocation is not scoped/,
    mutate: ({ statement }) =>
      (statement.predicate.runDetails.metadata.invocationId =
        "https://github.com/attacker/pigeonpost/actions/runs/123/attempts/1"),
  },
  {
    name: "rejects an invocation without a positive run attempt",
    pattern: /invocation is not scoped/,
    mutate: ({ statement }) =>
      (statement.predicate.runDetails.metadata.invocationId =
        "https://github.com/bekirdag/pigeonpost/actions/runs/123/attempts/0"),
  },
];

for (const { name, mutate, pattern } of identityCases) {
  test(name, (context) => expectFailure(context, mutate, pattern));
}

test("rejects when the exact tarball bytes change", (context) => {
  const release = fixture(context);
  fs.appendFileSync(release.tarballFile, "tampered");
  const result = run(release);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /exact npm tarball/);
});

test("rejects a symlink substituted for the exact tarball", (context) => {
  const release = fixture(context);
  const realTarball = `${release.tarballFile}.real`;
  fs.renameSync(release.tarballFile, realTarball);
  fs.symlinkSync(path.basename(realTarball), release.tarballFile);
  const result = run(release);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /npm tarball must be a non-empty regular file/);
});

test("rejects malformed npm audit JSON", (context) => {
  const release = fixture(context);
  fs.writeFileSync(release.auditFile, "not json\n");
  const result = run(release);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /npm audit output is not valid JSON/);
});

test("rejects a caller-selected branch ref for a release", (context) => {
  const result = run(fixture(context), { ref: "refs/heads/main" });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /release ref must be refs\/tags\/v0\.2\.0/);
});
