// The handle grammar, checked against the cases crates/pigeonpost-core/src/address.rs asserts.
//
// The web must agree with the Rust exactly: a name the site accepts and the postbox refuses is a
// green tick followed by an error, and for a paid handle that error arrives after payment.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const inboxCopy = path.join(here, "..", "handle.js");
const siteCopy = path.join(here, "..", "..", "site", "handle.js");

function load(file) {
  const window = {};
  new Function("window", fs.readFileSync(file, "utf8"))(window);
  return window.PIGEONPOST_HANDLE;
}

test("the two copies of the grammar are byte-identical", () => {
  assert.equal(
    fs.readFileSync(inboxCopy, "utf8"),
    fs.readFileSync(siteCopy, "utf8"),
    "site/handle.js and site-inbox/handle.js have drifted — edit site/handle.js and copy it across",
  );
});

const H = load(siteCopy);

test("the four shapes a handle can take are accepted", () => {
  for (const [input, want] of [
    ["/github/alex/agent1", "/github/alex/agent1"],
    ["/bekir/docdex/scratch", "/bekir/docdex/scratch"],
    ["/pp/alex@example.com", "/pp/alex@example.com"],
    ["/alex@gmail.com", "/alex@gmail.com"],
    ["/alex@gmail.com/agent2", "/alex@gmail.com/agent2"],
    ["/ALEX@GMAIL.COM", "/alex@gmail.com"],
    ["/github/alice_one", "/github/alice_one"],
  ]) {
    const r = H.canonical(input);
    assert.ok(r.ok, `${input} should be accepted: ${r.reason}`);
    assert.equal(r.handle, want);
  }
});

test("the shapes the protocol refuses are refused here too", () => {
  for (const input of [
    "/bekir",                    // a namespace is not a person
    "/gh/someone",               // legacy namespace
    "/github/", "/github/-leading",
    "/github/alex/agent1/extra", // four segments is a typo, not a deeper fleet
    "/pp/@alex", "/pp/alex@", "/pp/a@b@c",
    "/alex@localhost",           // one label reaches nobody
    "/alex@.com", "/alex@example..com", "/alex@-example.com",
    "/alex?x@gmail.com",         // ? opens a loft hint
    "/alex#x@gmail.com",         // # opens a capability token
    "/alex%41@gmail.com",        // % can decode into a different name
    "/has space", "/-leading", "/trailing-", "/",
  ]) {
    assert.equal(H.canonical(input).ok, false, `${input} should be refused`);
  }
});

test("a contact covers the parent, never the provider", () => {
  assert.equal(H.coverageWildcard("/bekir/docdex"), "/bekir/*");
  assert.equal(H.coverageWildcard("/github/alex/agent1"), "/github/alex/*");
  assert.equal(H.coverageWildcard("/alex@gmail.com/agent2"), "/alex@gmail.com/*");
  // Nothing sits above an address, and /k/ is not a namespace anybody owns.
  assert.equal(H.coverageWildcard("/alex@gmail.com"), null);
  assert.equal(H.coverageWildcard("/k/abc"), null);
  assert.equal(H.coverageWildcard("/bekir"), null);
});

test("your own agents live under your namespace, which depends on its shape", () => {
  assert.equal(H.namespaceOf("/bekir/main"), "/bekir");            // siblings under a name you own
  assert.equal(H.namespaceOf("/github/alex"), "/github/alex");     // children, /github is everybody's
  assert.equal(H.namespaceOf("/alex@gmail.com"), "/alex@gmail.com");
  assert.equal(H.namespaceOf("/pp/alex"), "/pp/alex");
  assert.equal(H.fleetWildcard("/bekir/main"), "/bekir/*");
  assert.equal(H.fleetWildcard("/github/alex"), "/github/alex/*");
});
