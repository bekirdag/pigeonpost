// Phase 3B guest page (m.html + m.js), driven in jsdom: the invitation token leaves the address bar
// at once and is kept in memory only; a preselected answer waits for Confirm; confirming it's you
// signs exactly the proof docs/calendar.md §6 specifies (checked here with node:crypto); joining
// with only the link sends {token, occurrence_id}; unsubscribe; and a bare meeting link reveals
// nothing about the event.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { webcrypto, createPublicKey, verify } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";

const read = (name) => readFileSync(new URL("../" + name, import.meta.url), "utf8");
const tick = (ms = 20) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(predicate, what = "page did not settle") {
  for (let n = 0; n < 300; n++) { if (predicate()) return; await tick(10); }
  assert.fail(what);
}
const NOW = Math.floor(Date.now() / 1000);
const TOKEN = "tok_SECRET_invitation_0123456789";

function preview(over = {}) {
  return {
    event: { title: "Design review", all_day: false, start: "2026-10-22T09:00", end: "2026-10-22T10:00", start_date: null, end_date: null, tzid: "America/New_York",
      starts_at: NOW - 300, ends_at: NOW + 1800, location: "Room 2", organizer_name: "Bekir", recurring: false, status: "confirmed",
      meeting: { code: "ABCDEFGHJK", url: "https://inbox.pigeonpost.dev/m/ABCDEFGHJK", opens_at: NOW - 900, closes_at: NOW + 3600 } },
    occurrences: [{ occurrence_id: "occ_now", starts_at: NOW - 300, ends_at: NOW + 1800, status: "confirmed" }],
    attendee: { name: "Guest Person", rsvp: "needs_action", proof: null },
    guests: null,
    verification_available: true,
    ...over,
  };
}

function page(t, { path = "/m/ABCDEFGHJK", hash = "", previewBody = preview(), member = null, meeting = { state: "open", can_knock: false } } = {}) {
  const calls = [], joins = [];
  const json = (body, status = 200) => ({ ok: status < 300, status, json: async () => structuredClone(body), text: async () => "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n" });
  const fetch = async (url, opts = {}) => {
    const u = new URL(url, "https://inbox.pigeonpost.dev");
    const body = typeof opts.body === "string" ? JSON.parse(opts.body) : null;
    calls.push({ url: String(url), path: u.pathname, method: opts.method || "GET", body, headers: opts.headers || {} });
    const g = u.pathname.replace("/v1/calendar/guest/", "");
    if (u.pathname === "/v1/calendar/guest/preview") return json(previewBody);
    if (g === "rsvp") return json({ rsvp: body.status, proof: "link" });
    if (g === "ics") return json({});
    if (g === "verify/start") return json({ challenge_id: "chl_1", expires_at: NOW + 600 });
    if (g === "verify/complete") return json({ guest_session: "gs_1", session_id: "gss_1", expires_at: NOW + 86400 });
    if (g === "join") return json({ ticket: "tkt_1", code: "ABCDEFGHJK", occurrence_id: body.occurrence_id, admission: body.token ? "knock" : "direct", expires_at: NOW + 120 });
    if (g === "unsubscribe") return json({ ok: true });
    if (u.pathname === "/calls/v1/meetings/scheduled/ABCDEFGHJK") return json(opts.headers && opts.headers.authorization ? member : meeting);
    if (u.pathname === "/v1/identities") return json({ identities: [{ address: "/k/me", handle: "/me/main" }] });
    return json({ error: "not_found" }, 404);
  };
  const errors = [], vconsole = new VirtualConsole();
  vconsole.on("jsdomError", (e) => errors.push(e.message));
  const dom = new JSDOM(read("m.html"), { url: "https://inbox.pigeonpost.dev" + path + hash, runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: vconsole });
  const w = dom.window, $ = (id) => w.document.getElementById(id);
  Object.defineProperty(w, "crypto", { value: webcrypto });
  if (!w.TextEncoder) w.TextEncoder = TextEncoder;
  w.fetch = fetch;
  w.URL.createObjectURL = () => "blob:fixture";
  w.URL.revokeObjectURL = () => {};
  w.HTMLAnchorElement.prototype.click = function () { calls.push({ download: this.getAttribute("download") }); };
  if (member) w.localStorage.setItem("ppi_token", "member_token");
  w.PigeonpostCalls = { joinScheduled: async (code, opts) => { joins.push({ code, opts: JSON.parse(JSON.stringify(opts)) }); return { state: "waiting" }; } };
  w.eval(read("config.js"));
  w.eval(read("m.js"));
  t.after(async () => { await tick(60); w.close(); assert.deepEqual(errors, [], "No uncaught script errors"); });
  return { w, $, calls, joins, doc: w.document };
}

function stored(w) {
  const out = [];
  for (const s of [w.localStorage, w.sessionStorage]) for (let i = 0; i < s.length; i++) out.push(s.key(i), s.getItem(s.key(i)));
  return out.join("\n");
}
function checkProof(publicKey, proof, message) {
  const raw = Buffer.from(publicKey, "base64url");
  assert.equal(raw.length, 65);
  assert.equal(raw[0], 4, "uncompressed point");
  const key = createPublicKey({ key: { kty: "EC", crv: "P-256", x: raw.subarray(1, 33).toString("base64url"), y: raw.subarray(33).toString("base64url") }, format: "jwk" });
  const sig = Buffer.from(proof, "base64url");
  assert.equal(sig.length, 64, "raw r||s");
  assert.ok(verify("sha256", Buffer.from(message, "utf8"), { key, dsaEncoding: "ieee-p1363" }, sig), "signature verifies over " + JSON.stringify(message));
}

test("the fragment leaves the address bar at once and the token is never stored or put in a URL", async (t) => {
  const a = page(t, { hash: "#t=" + TOKEN + "&r=accepted" });
  assert.equal(a.w.location.href, "https://inbox.pigeonpost.dev/m/ABCDEFGHJK", "removed before anything else");
  await until(() => a.$("m-when"));
  assert.equal(a.$("m-title").textContent, "Design review");
  assert.equal(a.doc.querySelector('meta[name="referrer"]').content, "no-referrer");
  assert.match(a.doc.querySelector('meta[http-equiv="Content-Security-Policy"]').content, /script-src 'self'/);
  const pv = a.calls.find((c) => c.path === "/v1/calendar/guest/preview");
  assert.equal(pv.method, "POST");
  assert.deepEqual(pv.body, { token: TOKEN });
  for (const c of a.calls) assert.ok(!String(c.url || "").includes(TOKEN), "never in a URL");
  assert.ok(!stored(a.w).includes(TOKEN), "never in storage");
  assert.ok(!a.w.location.href.includes(TOKEN));
  assert.ok(!a.doc.documentElement.outerHTML.includes(TOKEN), "never in the page");
  assert.match(a.$("m-organizer-zone")?.textContent || "New York", /New York/);
});

test("a preselected answer is highlighted and only sent after Confirm", async (t) => {
  const a = page(t, { hash: "#t=" + TOKEN + "&r=accepted" });
  await until(() => a.$("m-confirm"));
  await tick(60);
  assert.equal(a.calls.filter((c) => c.path === "/v1/calendar/guest/rsvp").length, 0, "opening the link changes nothing");
  assert.equal(a.$("m-rsvp-accepted").getAttribute("aria-pressed"), "true");
  assert.ok(a.$("m-rsvp-accepted").classList.contains("is-preselected"));
  assert.match(a.$("m-confirm-note").textContent, /not sent yet/);
  a.$("m-rsvp-tentative").click();
  await tick();
  assert.equal(a.calls.filter((c) => c.path === "/v1/calendar/guest/rsvp").length, 0, "changing the choice still sends nothing");
  assert.equal(a.$("m-confirm").textContent, "Confirm: Maybe");
  a.$("m-confirm").click();
  await until(() => a.calls.some((c) => c.path === "/v1/calendar/guest/rsvp"));
  assert.deepEqual(a.calls.find((c) => c.path === "/v1/calendar/guest/rsvp").body, { token: TOKEN, status: "tentative", scope: "all" });
  await until(() => a.$("m-current"));
  assert.equal(a.$("m-current").textContent, "Your answer: Maybe");
  assert.equal(a.$("m-confirm"), null);
  a.$("m-ics").click();
  await until(() => a.calls.some((c) => c.download));
  assert.deepEqual(a.calls.find((c) => c.path === "/v1/calendar/guest/ics").body, { token: TOKEN });
});

test("confirming it's you signs the exact proof messages, then joins with the ticket", async (t) => {
  const a = page(t, { hash: "#t=" + TOKEN });
  await until(() => a.$("m-verify-start"));
  a.$("m-verify-start").click();
  await until(() => a.$("m-code"));
  assert.deepEqual(a.calls.find((c) => c.path === "/v1/calendar/guest/verify/start").body, { token: TOKEN });
  a.$("m-code").value = "123456";
  a.$("m-verify-complete").click();
  await until(() => a.$("m-join-btn"));
  const done = a.calls.find((c) => c.path === "/v1/calendar/guest/verify/complete").body;
  assert.deepEqual(Object.keys(done).sort(), ["challenge_id", "code", "device_proof", "device_public_key", "ts"]);
  assert.equal(done.challenge_id, "chl_1");
  assert.equal(done.code, "123456");
  assert.ok(Math.abs(done.ts - Date.now() / 1000) < 120);
  checkProof(done.device_public_key, done.device_proof, `pigeonpost-guest-proof-v1\nverify\nchl_1\n${done.ts}`);
  a.$("m-join-btn").click();
  await until(() => a.joins.length);
  const join = a.calls.find((c) => c.path === "/v1/calendar/guest/join").body;
  assert.deepEqual(Object.keys(join).sort(), ["device_proof", "guest_session", "occurrence_id", "ts"]);
  assert.equal(join.guest_session, "gs_1");
  assert.equal(join.occurrence_id, "occ_now");
  checkProof(done.device_public_key, join.device_proof, `pigeonpost-guest-proof-v1\njoin\ngss_1\nocc_now\n${join.ts}`);
  assert.deepEqual(a.joins[0], { code: "ABCDEFGHJK", opts: { ticket: "tkt_1", occurrenceId: "occ_now", guest: true, name: "Guest Person", title: "Design review" } });
  assert.ok(!stored(a.w).includes("gs_1"), "the guest session stays in memory");
});

test("without verification the guest asks to join with the link: {token, occurrence_id}", async (t) => {
  const a = page(t, { hash: "#t=" + TOKEN, previewBody: preview({ verification_available: false }) });
  await until(() => a.$("m-ask"));
  a.$("m-ask").click();
  await until(() => a.joins.length);
  assert.deepEqual(a.calls.find((c) => c.path === "/v1/calendar/guest/join").body, { token: TOKEN, occurrence_id: "occ_now" });
  assert.equal(a.joins[0].opts.ticket, "tkt_1");
  assert.equal(a.joins[0].opts.guest, true);
});

test("unsubscribe mode offers this organizer or anyone", async (t) => {
  const a = page(t, { path: "/m/u", hash: "#u=tok_unsub_secret" });
  assert.equal(a.w.location.href, "https://inbox.pigeonpost.dev/m/u");
  await until(() => a.$("m-unsub-all"));
  assert.ok(a.$("m-unsub-organizer"));
  assert.equal(a.calls.length, 0, "nothing is sent on load");
  a.$("m-unsub-all").click();
  await until(() => a.$("m-unsub-done"));
  assert.deepEqual(a.calls.find((c) => c.path === "/v1/calendar/guest/unsubscribe").body, { token: "tok_unsub_secret", scope: "all" });
  assert.ok(!stored(a.w).includes("tok_unsub_secret"));
});

test("a bare meeting link shows nothing about the event", async (t) => {
  const anon = page(t, { meeting: { state: "open", can_knock: false } });
  await until(() => anon.$("m-not-open"));
  assert.equal(anon.$("m-not-open").textContent, "This meeting isn’t open.");
  assert.ok(anon.calls.every((c) => !c.path.startsWith("/v1/")), "no postbox requests");
  const knock = page(t, { meeting: { state: "open", can_knock: true } });
  await until(() => knock.$("m-knock"));
  knock.$("m-name").value = "Visitor";
  knock.$("m-knock").click();
  await until(() => knock.joins.length);
  assert.deepEqual(knock.joins[0], { code: "ABCDEFGHJK", opts: { name: "Visitor", guest: true } });
  const member = page(t, { member: { state: "open", role: "attendee", admission: "direct", occurrences: [], room: null } });
  await until(() => member.$("m-member-join"));
  member.$("m-member-join").click();
  await until(() => member.joins.length);
  assert.deepEqual(member.joins[0], { code: "ABCDEFGHJK", opts: { as: "/k/me" } });
});
