// The naming rule every client shares: a handle becomes a person's name, and their face's letters
// come from that name. The same table is asserted in the iOS/Mac, Android, Windows and Linux tests.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";

function load() {
  const window = {};
  new Function("window", fs.readFileSync(new URL("../names.js", import.meta.url), "utf8"))(window);
  return window.PigeonpostNames;
}

const N = load();

test("handles read as names", () => {
  const table = [
    ["/bekir/main", "Bekir"],
    ["/bekir", "Bekir"],
    ["/wodo/hr", "Wodo HR"],
    ["/acme/john-doe", "Acme John Doe"],
    ["/acme/john_doe", "Acme John Doe"],
    ["/acme/qa/bot", "Acme QA Bot"],
    ["/bekir/agent1", "Bekir Agent1"],
    ["/wodo/ceo", "Wodo CEO"],
    ["/wodo/home", "Wodo Home"],
    ["/main", "Main"],
  ];
  for (const [handle, name] of table) assert.equal(N.displayName(handle), name, handle);
});

test("shapes that are not names keep their display", () => {
  assert.equal(N.displayName("/k/aaaa1111bbbb2222cccc3333dd"), "/k/aaaa1111b…");
  assert.equal(N.displayName("/github/alex"), "alex");
  assert.equal(N.displayName("/alex@example.com"), "/alex@example.com");
  assert.equal(N.displayName("/pp/alex@example.com"), "alex@example.com");
  assert.equal(N.displayName("Team inbox"), "Team inbox");
  assert.equal(N.displayName(""), "unknown");
});

test("the acronym list is defined once and lower-case", () => {
  assert.deepEqual(N.ACRONYMS, ["hr", "it", "qa", "ui", "ux", "pr", "ai", "ceo", "cto", "cfo", "ops", "dev"]);
});

test("initials come from the derived name", () => {
  assert.equal(N.initials(N.displayName("/bekir/main")), "BE");
  assert.equal(N.initials(N.displayName("/wodo/hr")), "WH");
  assert.equal(N.initials(N.displayName("/acme/john-doe")), "AD");
  assert.equal(N.initials("Mum"), "MU");
  assert.equal(N.initials("Ada Lovelace"), "AL");
  assert.equal(N.initials(N.displayName("/k/aaaa1111bbbb2222")), "AA");
  assert.equal(N.initials(""), "··");
});
