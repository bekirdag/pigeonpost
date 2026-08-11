import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import {
  authorizationPayload,
  effectiveRulesetReferences,
  verifyGitHubReleaseControls,
  verifyReleaseAuthorization,
} from './verify-release-authorization.mjs';

const RELEASE = Object.freeze({
  repository: 'bekirdag/pigeonpost',
  tag: 'v0.2.0',
  commit: 'a'.repeat(40),
  designationDigest: 'b'.repeat(64),
});

function fixture() {
  const { publicKey, privateKey } = crypto.generateKeyPairSync('ed25519');
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'pigeonpost-release-auth.'));
  const configPath = path.join(directory, 'authorization.json');
  fs.writeFileSync(configPath, `${JSON.stringify({
    version: 1,
    repository: RELEASE.repository,
    algorithm: 'Ed25519',
    approverPublicKeySpkiBase64: publicKey.export({ format: 'der', type: 'spki' }).toString('base64'),
    externalProtectionAppSlug: 'pigeonpost-release-broker',
  })}\n`, { mode: 0o600 });
  const signatureBase64 = crypto.sign(
    null,
    Buffer.from(authorizationPayload(RELEASE), 'utf8'),
    privateKey,
  ).toString('base64');
  return { configPath, directory, signatureBase64 };
}

function githubControls() {
  const repositoryRuleset = 41;
  const organizationRuleset = 73;
  const ruleEvidence = (type, rulesetId, sourceType, source, parameters) => ({
    type,
    ruleset_id: rulesetId,
    ruleset_source_type: sourceType,
    ruleset_source: source,
    ...(parameters ? { parameters } : {}),
  });
  return {
    repository: RELEASE.repository,
    authorization: { externalProtectionAppSlug: 'pigeonpost-release-broker' },
    environment: {
      name: 'production-release',
      can_admins_bypass: false,
      protection_rules: [{
        type: 'required_reviewers',
        prevent_self_review: true,
        reviewers: [{ type: 'User', reviewer: { login: 'independent-reviewer' } }],
      }],
      deployment_branch_policy: {
        protected_branches: false,
        custom_branch_policies: true,
      },
    },
    policies: {
      total_count: 1,
      branch_policies: [{ id: 1, type: 'tag', name: 'v*' }],
    },
    externalRules: {
      total_count: 1,
      custom_deployment_protection_rules: [{
        id: 2,
        enabled: true,
        app: { slug: 'pigeonpost-release-broker' },
      }],
    },
    mainRules: [
      ruleEvidence('pull_request', repositoryRuleset, 'Repository', RELEASE.repository, {
        required_approving_review_count: 1,
      }),
      ruleEvidence('required_status_checks', repositoryRuleset, 'Repository', RELEASE.repository, {
        required_status_checks: [{ context: 'CI / test' }],
      }),
      ruleEvidence('non_fast_forward', organizationRuleset, 'Organization', 'bekirdag'),
      ruleEvidence('deletion', organizationRuleset, 'Organization', 'bekirdag'),
    ],
    effectiveRulesets: [
      {
        id: repositoryRuleset,
        target: 'branch',
        source_type: 'Repository',
        source: RELEASE.repository,
        enforcement: 'active',
        bypass_actors: [],
      },
      {
        id: organizationRuleset,
        target: 'branch',
        source_type: 'Organization',
        source: 'bekirdag',
        enforcement: 'active',
        bypass_actors: [],
      },
    ],
  };
}

test('authorization is bound to repository, tag, commit, and designation evidence', (context) => {
  const current = fixture();
  context.after(() => fs.rmSync(current.directory, { recursive: true, force: true }));
  assert.match(
    verifyReleaseAuthorization({ ...RELEASE, ...current }),
    /^pigeonpost-release-authorization\/v1\n/,
  );
  for (const mutation of [
    { tag: 'v0.2.1' },
    { commit: 'c'.repeat(40) },
    { designationDigest: 'd'.repeat(64) },
    { repository: 'attacker/pigeonpost' },
  ]) {
    assert.throws(
      () => verifyReleaseAuthorization({ ...RELEASE, ...current, ...mutation }),
      /signature|configuration/,
    );
  }
});

test('missing production key and malformed signatures fail closed', (context) => {
  const current = fixture();
  context.after(() => fs.rmSync(current.directory, { recursive: true, force: true }));
  assert.throws(
    () => verifyReleaseAuthorization({ ...RELEASE, ...current, signatureBase64: 'AAAA' }),
    /wrong length/,
  );
  fs.writeFileSync(current.configPath, `${JSON.stringify({
    version: 1,
    repository: RELEASE.repository,
    algorithm: 'Ed25519',
    approverPublicKeySpkiBase64: null,
    externalProtectionAppSlug: null,
  })}\n`);
  assert.throws(
    () => verifyReleaseAuthorization({ ...RELEASE, ...current }),
    /no independent release-approver public key/,
  );
});

test('external deployment-protection boundary is mandatory', (context) => {
  const current = fixture();
  context.after(() => fs.rmSync(current.directory, { recursive: true, force: true }));
  const config = JSON.parse(fs.readFileSync(current.configPath, 'utf8'));
  config.externalProtectionAppSlug = null;
  fs.writeFileSync(current.configPath, `${JSON.stringify(config)}\n`);
  assert.throws(
    () => verifyReleaseAuthorization({ ...RELEASE, ...current }),
    /no external release-protection app is pinned/,
  );
});

test('canonical payload has one exact domain-separated representation', () => {
  assert.equal(
    authorizationPayload(RELEASE),
    `pigeonpost-release-authorization/v1\nrepository=bekirdag/pigeonpost\n`
      + `tag=v0.2.0\ncommit=${'a'.repeat(40)}\n`
      + `designation_sha256=${'b'.repeat(64)}\n`,
  );
  assert.throws(
    () => authorizationPayload({ ...RELEASE, tag: 'v00.2.0' }),
    /invalid release tag/,
  );
});

test('GitHub control evidence binds every effective main rule to a non-bypassable ruleset', () => {
  const controls = githubControls();
  assert.deepEqual(verifyGitHubReleaseControls(controls), [
    { id: 41, sourceType: 'Repository', source: RELEASE.repository },
    { id: 73, sourceType: 'Organization', source: 'bekirdag' },
  ]);
  assert.deepEqual(effectiveRulesetReferences(controls.mainRules, RELEASE.repository), [
    { id: 41, sourceType: 'Repository', source: RELEASE.repository },
    { id: 73, sourceType: 'Organization', source: 'bekirdag' },
  ]);
});

test('environment administrator bypass and incomplete tag-policy evidence fail closed', () => {
  for (const adminBypass of [true, undefined, null]) {
    const controls = githubControls();
    controls.environment.can_admins_bypass = adminBypass;
    assert.throws(
      () => verifyGitHubReleaseControls(controls),
      /disable administrator bypass/,
    );
  }

  const controls = githubControls();
  controls.policies.total_count = 2;
  assert.throws(
    () => verifyGitHubReleaseControls(controls),
    /allow only the exact v\* tag policy/,
  );
});

test('ruleset bypass actors and redacted bypass evidence fail closed', () => {
  const bypassable = githubControls();
  bypassable.effectiveRulesets[1].bypass_actors = [{
    actor_id: null,
    actor_type: 'OrganizationAdmin',
    bypass_mode: 'always',
  }];
  assert.throws(
    () => verifyGitHubReleaseControls(bypassable),
    /permits bypass actors/,
  );

  const redacted = githubControls();
  delete redacted.effectiveRulesets[0].bypass_actors;
  assert.throws(
    () => verifyGitHubReleaseControls(redacted),
    /redacted bypass-actor evidence/,
  );
});

test('ruleset details must be complete and match authentic main-rule source evidence', () => {
  const missing = githubControls();
  missing.effectiveRulesets.pop();
  assert.throws(
    () => verifyGitHubReleaseControls(missing),
    /incomplete effective-ruleset evidence/,
  );

  const mismatched = githubControls();
  mismatched.effectiveRulesets[1].source = 'attacker';
  assert.throws(
    () => verifyGitHubReleaseControls(mismatched),
    /does not match main/,
  );

  const malformed = githubControls();
  malformed.mainRules[0].ruleset_id = null;
  assert.throws(
    () => verifyGitHubReleaseControls(malformed),
    /no authentic ruleset id/,
  );
});
