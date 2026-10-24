// The invitation page: the fragment token is taken out of the URL, nothing is accepted without a
// click, and a mismatch shows only the masked address.
import assert from "node:assert/strict";
import test from "node:test";
import { load, reply, tick, jwt } from "./helpers.mjs";

const TOKEN = "A".repeat(43);
const preview = { org: { name: "Acme", namespace: "acme" }, handle: "/acme/john", role_names: [], inviter: "Bekir",
  contact_masked: "j***@acme.test", expires_at: 1900000000, rooms: ["General"], requires_mfa: false };

test("the fragment token is removed from the address bar and only POSTed", async (t) => {
  const { window, calls, $ } = load(t, "invite.html", { url: `https://enterprise.pigeonpost.dev/invite#${TOKEN}`,
    fetch: (c) => c.url.endsWith("/v1/org-invites/preview") ? reply(200, preview) : reply(404, {}) });
  await tick();
  assert.equal(window.location.hash, "");
  assert.equal(window.location.href, "https://enterprise.pigeonpost.dev/invite");
  assert.equal(window.sessionStorage.getItem("ppe_invite_token"), TOKEN);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].method, "POST");
  assert.deepEqual(calls[0].body, { token: TOKEN });
  assert.ok(!calls[0].url.includes(TOKEN));
  assert.equal(calls[0].headers.authorization, undefined, "preview is anonymous");
  const text = $("#invite").textContent;
  assert.match(text, /Acme invites you/);
  assert.match(text, /\/acme\/john/);
  assert.match(text, /admins will see your company address, status and last activity, not your messages/);
  assert.match(text, /Joining adds you to: General/);
  assert.match($("#invite button").textContent, /Sign in to accept/);
});

test("a mismatched account sees the masked address and a switch-account action", { timeout: 5_000 }, async (t) => {
  let releaseDigest;
  const digestGate = new Promise((resolve) => { releaseDigest = resolve; });
  const { $, calls, nav, navigation, window } = load(t, "invite.html", { url: `https://enterprise.pigeonpost.dev/invite#${TOKEN}`, session: jwt({ preferred_username: "someone" }),
    before: (w) => {
      const crypto = w.crypto;
      Object.defineProperty(w, "crypto", { configurable: true, value: {
        getRandomValues: crypto.getRandomValues.bind(crypto),
        subtle: { digest: async (...args) => { await digestGate; return crypto.subtle.digest(...args); } },
      } });
    },
    fetch: (c) => c.url.endsWith("/preview") ? reply(200, preview) : reply(403, { error: "invite_address_mismatch", contact_masked: "j***@acme.test" }) });
  await tick();
  assert.equal(calls.filter((c) => c.url.endsWith("/accept")).length, 0, "opening the page never accepts");
  [...window.document.querySelectorAll("#invite button")].find((b) => /Accept invitation/.test(b.textContent)).click();
  await tick();
  const accept = calls.find((c) => c.url.endsWith("/v1/org-invites/accept"));
  assert.equal(accept.headers.authorization.startsWith("Bearer "), true);
  assert.match($("#invite").textContent, /sent to j\*\*\*@acme\.test/);
  assert.match($("#invite").textContent, /signed in as someone/);
  [...window.document.querySelectorAll("#invite button")].find((b) => /Switch account/.test(b.textContent)).click();
  try {
    await tick();
    assert.equal(nav.length, 0, "sign-in waits for the native PKCE digest");
  } finally {
    releaseDigest();
  }
  assert.equal(new URL(await navigation).searchParams.get("prompt"), "login");
  assert.equal(window.sessionStorage.getItem("ppe_session"), null);
  assert.equal(window.sessionStorage.getItem("ppe_invite_token"), TOKEN, "the token survives the account switch");
});

test("any preview failure is one generic answer", async (t) => {
  const { $, window } = load(t, "invite.html", { url: `https://enterprise.pigeonpost.dev/invite#${TOKEN}`, fetch: () => reply(404, { error: "invite_invalid" }) });
  await tick();
  assert.match($("#invite").textContent, /This invitation link isn't valid/);
  assert.equal(window.sessionStorage.getItem("ppe_invite_token"), null);
  const none = load(t, "invite.html", { url: "https://enterprise.pigeonpost.dev/invite" });
  await tick();
  assert.match(none.$("#invite").textContent, /isn't valid/);
  assert.equal(none.calls.length, 0);
});

test("acceptance welcomes the member with their address", async (t) => {
  const { $, window } = load(t, "invite.html", { url: `https://enterprise.pigeonpost.dev/invite#${TOKEN}`, session: jwt(),
    fetch: (c) => c.url.endsWith("/preview") ? reply(200, preview) : reply(200, { org: { name: "Acme" }, mailbox: { address: "/k/x", handle: "/acme/john" } }) });
  await tick();
  [...window.document.querySelectorAll("#invite button")].find((b) => /Accept/.test(b.textContent)).click();
  await tick();
  assert.match($("#invite").textContent, /Welcome to Acme/);
  assert.ok([...window.document.querySelectorAll("#invite a")].some((a) => a.href === "https://inbox.pigeonpost.dev/" && /Open your inbox/.test(a.textContent)));
  assert.equal(window.sessionStorage.getItem("ppe_invite_token"), null);
});

test("a new invitation fragment clears every previous acceptance decision", async (t) => {
  for (const fragment of ["", "invalid", "B".repeat(43)]) {
    await t.test(fragment ? "replacement " + fragment.slice(0, 7) : "empty fragment", async (t) => {
      const valid = fragment.length === 43;
      const { window, calls, $ } = load(t, "invite.html", {
        url: `https://enterprise.pigeonpost.dev/invite#${fragment}`,
        session: jwt(),
        storage: {
          ppe_invite_token: TOKEN,
          ppe_resume: { name: "invite.accept", at: Date.now() },
        },
        fetch: (c) => c.url.endsWith("/preview") ? reply(200, preview) : reply(200, { org: { name: "Acme" } }),
      });
      await tick();
      assert.equal(calls.filter((c) => c.url.endsWith("/accept")).length, 0, "a previous decision cannot accept a different link");
      assert.equal(window.sessionStorage.getItem("ppe_resume"), null);
      assert.equal(window.sessionStorage.getItem("ppe_invite_token"), valid ? fragment : null);
      assert.equal(window.location.href, "https://enterprise.pigeonpost.dev/invite");
      assert.match($("#invite").textContent, valid ? /Acme invites you/ : /isn't valid/);
      assert.deepEqual(calls.map((c) => c.body), valid ? [{ token: fragment }] : []);
    });
  }
});

test("the fragment-less sign-in return preserves the original acceptance", async (t) => {
  const { window, calls, $ } = load(t, "invite.html", {
    url: "https://enterprise.pigeonpost.dev/invite",
    session: jwt(),
    storage: { ppe_invite_token: TOKEN, ppe_resume: { name: "invite.accept", at: Date.now() } },
    fetch: () => reply(200, { org: { name: "Acme" }, mailbox: { handle: "/acme/john" } }),
  });
  await tick();
  assert.equal(calls.length, 1);
  assert.ok(calls[0].url.endsWith("/accept"));
  assert.deepEqual(calls[0].body, { token: TOKEN });
  assert.match($("#invite").textContent, /Welcome to Acme/);
  assert.equal(window.sessionStorage.getItem("ppe_invite_token"), null);
});
