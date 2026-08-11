#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";

const REGISTRY = "https://registry.npmjs.org/";
const SLSA_PROVENANCE_V1 = "https://slsa.dev/provenance/v1";
const IN_TOTO_STATEMENT_V1 = "https://in-toto.io/Statement/v1";
const GITHUB_WORKFLOW_BUILD_TYPE =
  "https://slsa-framework.github.io/github-actions-buildtypes/workflow/v1";
const GITHUB_HOSTED_BUILDER = "https://github.com/actions/runner/github-hosted";
const DSSE_PAYLOAD_TYPE = "application/vnd.in-toto+json";
const SIGSTORE_BUNDLE_MEDIA_TYPES = new Set([
  "application/vnd.dev.sigstore.bundle+json;version=0.2",
  "application/vnd.dev.sigstore.bundle.v0.3+json",
]);
const MAX_AUDIT_BYTES = 8 * 1024 * 1024;
const MAX_TARBALL_BYTES = 64 * 1024 * 1024;

const isRecord = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);

function escapeRegex(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function readBoundedFile(file, label, maxBytes) {
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.size === 0 || stat.size > maxBytes) {
    throw new Error(`${label} must be a non-empty regular file no larger than ${maxBytes} bytes`);
  }
  return fs.readFileSync(file);
}

function decodeUtf8(buffer, label) {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(buffer);
  } catch {
    throw new Error(`${label} is not valid UTF-8`);
  }
}

function parseJson(buffer, label) {
  try {
    return JSON.parse(decodeUtf8(buffer, label));
  } catch (error) {
    if (error instanceof SyntaxError) {
      throw new Error(`${label} is not valid JSON`);
    }
    throw error;
  }
}

function decodeCanonicalBase64(value, label) {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    value.length % 4 !== 0 ||
    !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)
  ) {
    throw new Error(`${label} is not canonical base64`);
  }
  const decoded = Buffer.from(value, "base64");
  if (decoded.toString("base64") !== value) {
    throw new Error(`${label} is not canonical base64`);
  }
  return decoded;
}

function npmPurl(packageName, version) {
  if (packageName.startsWith("@")) {
    const [scope, name] = packageName.split("/");
    return `pkg:npm/${encodeURIComponent(scope)}/${encodeURIComponent(name)}@${version}`;
  }
  return `pkg:npm/${encodeURIComponent(packageName)}@${version}`;
}

function validateInputs({ packageName, version, repository, workflow, ref, sourceSha }) {
  if (
    !/^(?:@[a-z0-9][a-z0-9._~-]*\/[a-z0-9][a-z0-9._~-]*|[a-z0-9][a-z0-9._~-]*)$/.test(
      packageName,
    )
  ) {
    throw new Error(`invalid npm package identity: ${packageName}`);
  }
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) {
    throw new Error(`invalid stable SemVer: ${version}`);
  }
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) {
    throw new Error(`invalid GitHub repository identity: ${repository}`);
  }
  if (!/^\.github\/workflows\/[A-Za-z0-9_.-]+\.ya?ml$/.test(workflow)) {
    throw new Error(`invalid GitHub workflow path: ${workflow}`);
  }
  if (ref !== `refs/tags/v${version}`) {
    throw new Error(`release ref must be refs/tags/v${version}`);
  }
  if (!/^[a-f0-9]{40}$/.test(sourceSha)) {
    throw new Error("source SHA must be a lowercase 40-character Git commit");
  }
}

function verifySignedStatement({
  statement,
  tarballSha512,
  packageName,
  version,
  repository,
  workflow,
  ref,
  sourceSha,
}) {
  if (!isRecord(statement) || statement._type !== IN_TOTO_STATEMENT_V1) {
    throw new Error("provenance payload is not an in-toto v1 statement");
  }
  if (statement.predicateType !== SLSA_PROVENANCE_V1) {
    throw new Error("signed statement is not SLSA provenance v1");
  }

  const expectedPurl = npmPurl(packageName, version);
  if (!Array.isArray(statement.subject) || statement.subject.length !== 1) {
    throw new Error("signed statement must contain exactly one subject");
  }
  const subject = statement.subject[0];
  if (
    !isRecord(subject) ||
    subject.name !== expectedPurl ||
    !isRecord(subject.digest) ||
    Object.keys(subject.digest).length !== 1 ||
    !/^[a-f0-9]{128}$/.test(subject.digest.sha512 ?? "") ||
    subject.digest.sha512 !== tarballSha512
  ) {
    throw new Error(`signed subject does not bind ${expectedPurl} to the exact npm tarball`);
  }

  const predicate = statement.predicate;
  const build = isRecord(predicate) ? predicate.buildDefinition : undefined;
  if (!isRecord(build) || build.buildType !== GITHUB_WORKFLOW_BUILD_TYPE) {
    throw new Error("provenance does not use the GitHub Actions workflow/v1 build type");
  }

  const expectedRepositoryUrl = `https://github.com/${repository}`;
  const external = build.externalParameters;
  const workflowIdentity = isRecord(external) ? external.workflow : undefined;
  if (
    !isRecord(workflowIdentity) ||
    workflowIdentity.repository !== expectedRepositoryUrl ||
    workflowIdentity.path !== workflow ||
    workflowIdentity.ref !== ref
  ) {
    throw new Error("provenance workflow repository, path, or ref does not match the release");
  }

  if (!Array.isArray(build.resolvedDependencies) || build.resolvedDependencies.length !== 1) {
    throw new Error("provenance must contain exactly one resolved source dependency");
  }
  const source = build.resolvedDependencies[0];
  const expectedSourceUri = `git+${expectedRepositoryUrl}@${ref}`;
  if (
    !isRecord(source) ||
    source.uri !== expectedSourceUri ||
    !isRecord(source.digest) ||
    Object.keys(source.digest).length !== 1 ||
    source.digest.gitCommit !== sourceSha
  ) {
    throw new Error("provenance resolved source URI or Git commit does not match the release");
  }

  const runDetails = predicate.runDetails;
  if (
    !isRecord(runDetails) ||
    !isRecord(runDetails.builder) ||
    runDetails.builder.id !== GITHUB_HOSTED_BUILDER
  ) {
    throw new Error("provenance was not produced by a GitHub-hosted runner");
  }
  const invocationId = isRecord(runDetails.metadata)
    ? runDetails.metadata.invocationId
    : undefined;
  const invocationPattern = new RegExp(
    `^${escapeRegex(expectedRepositoryUrl)}/actions/runs/[1-9]\\d*/attempts/[1-9]\\d*$`,
  );
  if (typeof invocationId !== "string" || !invocationPattern.test(invocationId)) {
    throw new Error("provenance invocation is not scoped to the expected GitHub repository");
  }
}

function verifyNpmProvenance({
  auditFile,
  tarballFile,
  packageName,
  version,
  repository,
  workflow,
  ref,
  sourceSha,
}) {
  validateInputs({ packageName, version, repository, workflow, ref, sourceSha });

  const tarball = readBoundedFile(tarballFile, "npm tarball", MAX_TARBALL_BYTES);
  const tarballSha512 = crypto.createHash("sha512").update(tarball).digest("hex");
  const audit = parseJson(
    readBoundedFile(auditFile, "npm audit output", MAX_AUDIT_BYTES),
    "npm audit output",
  );

  if (!isRecord(audit)) {
    throw new Error("npm audit output must be a JSON object");
  }
  if (!Array.isArray(audit.invalid) || audit.invalid.length !== 0) {
    throw new Error("npm audit output contains invalid or malformed verification results");
  }
  if (!Array.isArray(audit.missing) || audit.missing.length !== 0) {
    throw new Error("npm audit output contains missing or malformed verification results");
  }
  if (!Array.isArray(audit.verified) || audit.verified.length !== 1) {
    throw new Error("npm audit output must contain exactly one verified package");
  }

  const verified = audit.verified[0];
  if (
    !isRecord(verified) ||
    verified.name !== packageName ||
    verified.version !== version ||
    verified.registry !== REGISTRY
  ) {
    throw new Error("verified npm package, version, or registry does not match the release");
  }
  if (
    !isRecord(verified.attestations) ||
    !isRecord(verified.attestations.provenance) ||
    verified.attestations.provenance.predicateType !== SLSA_PROVENANCE_V1
  ) {
    throw new Error("verified npm package does not advertise SLSA provenance v1");
  }
  if (!Array.isArray(verified.attestationBundles)) {
    throw new Error("verified npm package does not contain attestation bundles");
  }
  for (const attestation of verified.attestationBundles) {
    if (
      !isRecord(attestation) ||
      typeof attestation.predicateType !== "string" ||
      !isRecord(attestation.bundle)
    ) {
      throw new Error("verified npm package contains a malformed attestation bundle");
    }
  }
  const provenanceBundles = verified.attestationBundles.filter(
    (attestation) => attestation.predicateType === SLSA_PROVENANCE_V1,
  );
  if (provenanceBundles.length !== 1) {
    throw new Error("verified npm package must contain exactly one SLSA provenance v1 bundle");
  }

  const bundle = provenanceBundles[0].bundle;
  if (
    !SIGSTORE_BUNDLE_MEDIA_TYPES.has(bundle.mediaType) ||
    !isRecord(bundle.verificationMaterial) ||
    !isRecord(bundle.dsseEnvelope)
  ) {
    throw new Error("SLSA provenance is not a supported complete Sigstore bundle");
  }
  const envelope = bundle.dsseEnvelope;
  if (envelope.payloadType !== DSSE_PAYLOAD_TYPE) {
    throw new Error("SLSA provenance DSSE payload type is not in-toto JSON");
  }
  if (!Array.isArray(envelope.signatures) || envelope.signatures.length !== 1) {
    throw new Error("SLSA provenance DSSE envelope must contain exactly one signature");
  }
  const signature = envelope.signatures[0];
  if (!isRecord(signature)) {
    throw new Error("SLSA provenance DSSE signature is malformed");
  }
  decodeCanonicalBase64(signature.sig, "SLSA provenance DSSE signature");
  if (signature.keyid !== undefined && typeof signature.keyid !== "string") {
    throw new Error("SLSA provenance DSSE signature key ID is malformed");
  }

  const statement = parseJson(
    decodeCanonicalBase64(envelope.payload, "SLSA provenance DSSE payload"),
    "SLSA provenance DSSE payload",
  );
  verifySignedStatement({
    statement,
    tarballSha512,
    packageName,
    version,
    repository,
    workflow,
    ref,
    sourceSha,
  });

  return { packageName, version, sourceSha, tarballSha512 };
}

function main() {
  const args = process.argv.slice(2);
  if (args.length !== 8) {
    throw new Error(
      "usage: verify-npm-provenance.mjs <audit.json> <package.tgz> <package> " +
        "<version> <owner/repo> <workflow-path> <git-ref> <source-sha>",
    );
  }
  const [auditFile, tarballFile, packageName, version, repository, workflow, ref, sourceSha] =
    args;
  const result = verifyNpmProvenance({
    auditFile,
    tarballFile,
    packageName,
    version,
    repository,
    workflow,
    ref,
    sourceSha,
  });
  process.stdout.write(
    `verified npm provenance for ${result.packageName}@${result.version} from ${result.sourceSha}\n`,
  );
}

try {
  main();
} catch (error) {
  process.stderr.write(`npm provenance verification failed: ${error.message}\n`);
  process.exitCode = 1;
}
