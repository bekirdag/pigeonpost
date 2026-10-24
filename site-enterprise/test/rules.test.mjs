// The rule sentence editor's model: form <-> rule round trips and the guardrails (SDS §9).
import assert from "node:assert/strict";
import test from "node:test";
import { load, plain } from "./helpers.mjs";

test("rules round-trip through the editor form unchanged", (t) => {
  const { window } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/" });
  const R = window.PPE.rules;
  const cases = [
    { owners: "/acme/*", peers: "/acme/*", admission: "allow", known: true, autonomy: null, verbs: null, locked: false, priority: 100 },
    { owners: "agents", peers: "*", admission: "block", known: false, autonomy: null, verbs: null, locked: true, priority: 50 },
    { owners: "/acme/agent1", peers: "/acme/john", admission: "allow", known: true, autonomy: "auto", verbs: ["report_status", "run_tests"], locked: false, priority: 80, description: "John directs agent1" },
    { owners: "/acme/build-*", peers: "members", admission: null, known: false, autonomy: "review", verbs: null, locked: false, priority: 1 },
  ];
  for (const rule of cases) {
    const back = R.fromForm(R.toForm(rule, "acme"), "acme");
    assert.deepEqual(JSON.parse(JSON.stringify(back)), JSON.parse(JSON.stringify(rule)));
  }
});

test("the editor refuses rules the server would refuse", (t) => {
  const { window } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/" });
  const R = window.PPE.rules;
  const base = R.toForm({ owners: "/acme/agent1", peers: "/acme/john", admission: "allow", known: true, autonomy: "auto", verbs: ["report_status"], priority: 100 }, "acme");
  assert.match(R.fromForm({ ...base, verbs: "deploy, report_status" }, "acme").error, /always need a person: deploy/);
  assert.match(R.fromForm({ ...base, ownersCustom: "/other/agent1" }, "acme").error, /inside \/acme/);
  assert.match(R.fromForm({ ...base, ownersKind: "members" }, "acme").error, /Only agents/);
  assert.match(R.fromForm({ ...base, ownersKind: "anyone" }, "acme").error, /inside \/acme/);
  assert.match(R.fromForm({ ...base, priority: 0 }, "acme").error, /1 to 1000/);
  assert.match(R.fromForm({ ...base, admission: "", known: false, autonomy: "" }, "acme").error, /doesn't change/);
  assert.equal(R.validPattern("/acme/a/b", "acme", "peers"), false);
  assert.equal(R.validPattern("/acme/team-*", "acme", "owners"), true);
});

test("a rule card reads back what the pickers show", (t) => {
  const { window } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/" });
  const app = window.PPE.app;
  app.org = { name: "Acme", namespace: "acme" };
  const card = app.ruleCard({ id: "r", owners: "agents", peers: "/acme/*", admission: "allow", known: false, autonomy: null, verbs: null, locked: true, priority: 50 }, () => {});
  const form = card._read();
  assert.equal(form.ownersKind, "agents");
  assert.equal(form.peersKind, "everyone");
  assert.equal(form.locked, true);
  assert.deepEqual(plain(window.PPE.rules.fromForm(form, "acme")), { owners: "agents", peers: "/acme/*", admission: "allow", known: false, autonomy: null, verbs: null, locked: true, priority: 50 });
});
