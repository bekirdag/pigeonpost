import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const DOMAIN = 'pigeonpost-release-authorization/v1';
const SEMVER_TAG = /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const COMMIT = /^[0-9a-f]{40}$/;
const SHA256 = /^[0-9a-f]{64}$/;
const REPOSITORY = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const APP_SLUG = /^[a-z0-9](?:[a-z0-9-]{0,98}[a-z0-9])?$/;
const RULESET_SOURCE = /^(?:[A-Za-z0-9_.-]+|[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+)$/;
const RULESET_SOURCE_TYPES = new Set(['Repository', 'Organization', 'Enterprise']);

function object(value, name) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${name} is not an object`);
  }
  return value;
}

function sourceMatchesRepository(sourceType, source, repository) {
  const [owner] = repository.split('/');
  if (sourceType === 'Repository') return source.toLowerCase() === repository.toLowerCase();
  if (sourceType === 'Organization') return source.toLowerCase() === owner.toLowerCase();
  return true;
}

export function effectiveRulesetReferences(mainRules, repository) {
  if (!REPOSITORY.test(repository || '')) throw new Error('invalid release repository');
  if (!Array.isArray(mainRules) || mainRules.length === 0) {
    throw new Error('GitHub returned no effective rules for main');
  }

  const references = new Map();
  for (const [index, candidate] of mainRules.entries()) {
    const rule = object(candidate, `effective main rule ${index}`);
    if (!Number.isSafeInteger(rule.ruleset_id) || rule.ruleset_id <= 0) {
      throw new Error(`effective main rule ${index} has no authentic ruleset id`);
    }
    if (!RULESET_SOURCE_TYPES.has(rule.ruleset_source_type)
        || typeof rule.ruleset_source !== 'string'
        || !RULESET_SOURCE.test(rule.ruleset_source)
        || !sourceMatchesRepository(
          rule.ruleset_source_type,
          rule.ruleset_source,
          repository,
        )) {
      throw new Error(`effective main rule ${index} has invalid ruleset source evidence`);
    }
    const reference = {
      id: rule.ruleset_id,
      sourceType: rule.ruleset_source_type,
      source: rule.ruleset_source,
    };
    const previous = references.get(reference.id);
    if (previous && (previous.sourceType !== reference.sourceType
        || previous.source.toLowerCase() !== reference.source.toLowerCase())) {
      throw new Error(`effective ruleset ${reference.id} has conflicting source evidence`);
    }
    references.set(reference.id, previous || reference);
  }
  return [...references.values()].sort((left, right) => left.id - right.id);
}

export function verifyGitHubReleaseControls({
  environment,
  policies,
  externalRules,
  mainRules,
  effectiveRulesets,
  authorization,
  repository,
}) {
  object(environment, 'production-release environment evidence');
  object(policies, 'production-release tag-policy evidence');
  object(externalRules, 'production-release protection-rule evidence');
  object(authorization, 'release authorization configuration');

  if (environment.name !== 'production-release') {
    throw new Error('GitHub environment evidence is not for production-release');
  }
  if (environment.can_admins_bypass !== false) {
    throw new Error('production-release must disable administrator bypass');
  }
  const reviewers = Array.isArray(environment.protection_rules)
    ? environment.protection_rules.filter((rule) => rule.type === 'required_reviewers')
    : [];
  if (reviewers.length !== 1 || reviewers[0].prevent_self_review !== true
      || !Array.isArray(reviewers[0].reviewers) || reviewers[0].reviewers.length === 0) {
    throw new Error('production-release must require another reviewer and prevent self-review');
  }
  if (environment.deployment_branch_policy?.protected_branches !== false
      || environment.deployment_branch_policy?.custom_branch_policies !== true) {
    throw new Error('production-release must use an explicit tag allowlist');
  }

  const branchPolicies = Array.isArray(policies.branch_policies)
    ? policies.branch_policies
    : [];
  if (!Number.isSafeInteger(policies.total_count)
      || policies.total_count !== branchPolicies.length
      || branchPolicies.length !== 1
      || branchPolicies[0].type !== 'tag'
      || branchPolicies[0].name !== 'v*') {
    throw new Error('production-release must allow only the exact v* tag policy');
  }

  if (!APP_SLUG.test(authorization.externalProtectionAppSlug || '')) {
    throw new Error('no external release-protection app is pinned');
  }
  const protectionRules = Array.isArray(externalRules.custom_deployment_protection_rules)
    ? externalRules.custom_deployment_protection_rules
    : [];
  if (!Number.isSafeInteger(externalRules.total_count)
      || externalRules.total_count !== protectionRules.length) {
    throw new Error('GitHub returned incomplete deployment-protection evidence');
  }
  const brokerMatches = protectionRules.filter((rule) => rule.enabled === true
    && rule.app?.slug === authorization.externalProtectionAppSlug);
  if (brokerMatches.length !== 1) {
    throw new Error('production-release must enable the one pinned external release broker');
  }

  const references = effectiveRulesetReferences(mainRules, repository);
  const rulesOfType = (type) => mainRules.filter((candidate) => candidate.type === type);
  const pullRequests = rulesOfType('pull_request');
  const statusChecks = rulesOfType('required_status_checks');
  if (!pullRequests.some((rule) =>
    (rule.parameters?.required_approving_review_count || 0) >= 1)
      || !statusChecks.some((rule) =>
        Array.isArray(rule.parameters?.required_status_checks)
          && rule.parameters.required_status_checks.length > 0)
      || rulesOfType('non_fast_forward').length === 0
      || rulesOfType('deletion').length === 0) {
    throw new Error('main must require review/status checks and forbid force-push/deletion');
  }

  if (!Array.isArray(effectiveRulesets) || effectiveRulesets.length !== references.length) {
    throw new Error('GitHub returned incomplete effective-ruleset evidence');
  }
  const detailsById = new Map();
  for (const [index, candidate] of effectiveRulesets.entries()) {
    const ruleset = object(candidate, `effective ruleset detail ${index}`);
    if (!Number.isSafeInteger(ruleset.id) || ruleset.id <= 0 || detailsById.has(ruleset.id)) {
      throw new Error('GitHub returned duplicate or invalid effective-ruleset evidence');
    }
    detailsById.set(ruleset.id, ruleset);
  }
  for (const reference of references) {
    const ruleset = detailsById.get(reference.id);
    if (!ruleset
        || ruleset.source_type !== reference.sourceType
        || typeof ruleset.source !== 'string'
        || ruleset.source.toLowerCase() !== reference.source.toLowerCase()
        || ruleset.target !== 'branch'
        || ruleset.enforcement !== 'active') {
      throw new Error(`effective ruleset ${reference.id} does not match main`);
    }
    if (!Object.hasOwn(ruleset, 'bypass_actors') || !Array.isArray(ruleset.bypass_actors)) {
      throw new Error(`effective ruleset ${reference.id} has redacted bypass-actor evidence`);
    }
    if (ruleset.bypass_actors.length !== 0) {
      throw new Error(`effective ruleset ${reference.id} permits bypass actors`);
    }
  }
  return references;
}

function canonicalBase64(value, name) {
  if (typeof value !== 'string' || value.length === 0 || value.length % 4 !== 0
      || !/^[A-Za-z0-9+/]+={0,2}$/.test(value)) {
    throw new Error(`${name} is not canonical base64`);
  }
  const decoded = Buffer.from(value, 'base64');
  if (decoded.toString('base64') !== value) {
    throw new Error(`${name} is not canonical base64`);
  }
  return decoded;
}

export function authorizationPayload({ repository, tag, commit, designationDigest }) {
  if (!REPOSITORY.test(repository || '')) throw new Error('invalid release repository');
  if (!SEMVER_TAG.test(tag || '')) throw new Error('invalid release tag');
  if (!COMMIT.test(commit || '')) throw new Error('invalid release commit');
  if (!SHA256.test(designationDigest || '') || /^0+$/.test(designationDigest)) {
    throw new Error('invalid designation-evidence digest');
  }
  return `${DOMAIN}\nrepository=${repository}\ntag=${tag}\ncommit=${commit}\n`
    + `designation_sha256=${designationDigest}\n`;
}

export function verifyReleaseAuthorization({
  configPath,
  repository,
  tag,
  commit,
  designationDigest,
  signatureBase64,
}) {
  const config = JSON.parse(fs.readFileSync(configPath, 'utf8'));
  const keys = Object.keys(config).sort();
  const expectedKeys = [
    'algorithm',
    'approverPublicKeySpkiBase64',
    'externalProtectionAppSlug',
    'repository',
    'version',
  ].sort();
  if (JSON.stringify(keys) !== JSON.stringify(expectedKeys)) {
    throw new Error('release approver configuration has missing or unknown fields');
  }
  if (config.version !== 1 || config.algorithm !== 'Ed25519'
      || config.repository !== repository) {
    throw new Error('release approver configuration is invalid or for another repository');
  }
  if (config.approverPublicKeySpkiBase64 === null) {
    throw new Error('no independent release-approver public key is pinned');
  }
  if (!APP_SLUG.test(config.externalProtectionAppSlug || '')) {
    throw new Error('no external release-protection app is pinned');
  }

  const publicDer = canonicalBase64(
    config.approverPublicKeySpkiBase64,
    'release approver public key',
  );
  const signature = canonicalBase64(signatureBase64, 'release approval signature');
  if (signature.length !== 64) throw new Error('release approval signature has the wrong length');

  const publicKey = crypto.createPublicKey({ key: publicDer, format: 'der', type: 'spki' });
  if (publicKey.asymmetricKeyType !== 'ed25519') {
    throw new Error('release approver key is not Ed25519');
  }
  const payload = authorizationPayload({ repository, tag, commit, designationDigest });
  if (!crypto.verify(null, Buffer.from(payload, 'utf8'), publicKey, signature)) {
    throw new Error('release approval signature does not match this exact release');
  }
  return payload;
}

function main() {
  const scriptDir = path.dirname(fileURLToPath(import.meta.url));
  const repoRoot = path.resolve(scriptDir, '../..');
  if (process.argv[2] === '--print-payload') {
    process.stdout.write(authorizationPayload({
      repository: process.env.GITHUB_REPOSITORY,
      tag: process.env.GITHUB_REF_NAME,
      commit: process.env.GITHUB_SHA,
      designationDigest: process.env.DESIGNATION_EVIDENCE_SHA256,
    }));
    return;
  }
  if (process.argv.length > 2) {
    throw new Error('usage: verify-release-authorization.mjs [--print-payload]');
  }
  verifyReleaseAuthorization({
    configPath: path.join(repoRoot, '.github/release-authorization.json'),
    repository: process.env.GITHUB_REPOSITORY,
    tag: process.env.GITHUB_REF_NAME,
    commit: process.env.GITHUB_SHA,
    designationDigest: process.env.DESIGNATION_EVIDENCE_SHA256,
    signatureBase64: process.env.RELEASE_APPROVAL_SIGNATURE,
  });
  process.stdout.write('independent release authorization verified\n');
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
