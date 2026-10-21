// Phase 1b in the web inbox: calls page, filtered mail, privacy settings, blocking and profile
// pictures, driven in jsdom against wire shapes from the postbox contract.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import vm from "node:vm";
import { JSDOM, VirtualConsole } from "jsdom";
import { fixtureServer, response, OWNER, PEER } from "./fixtures.mjs";

const read = name => readFileSync(new URL("../" + name, import.meta.url), "utf8");
const tick = (ms = 30) => new Promise(resolve => setTimeout(resolve, ms));
async function until(predicate, what = "UI did not settle") {
  for (let n = 0; n < 200; n++) { if (predicate()) return; await tick(10); }
  assert.fail(what);
}

const ALL = ["call_history", "call_admission", "message_filtering", "avatars"];
const NOW = Math.floor(Date.now() / 1000);

const callEntry = (over = {}) => ({
  id: "clg_1", owner: OWNER, room_id: "room1", direction: "incoming", outcome: "missed", kind: "video",
  peer: "/k/peer", peer_handle: PEER, participants: [], group_id: null,
  started_at: NOW - 120, answered_at: null, ended_at: null, duration: null, seen: false, ...over,
});

async function app(t, { features = ALL, configure = () => {} } = {}) {
  const server = fixtureServer();
  server.features = features;
  server.calls_log = [callEntry(), callEntry({ id: "clg_2", room_id: "room2", direction: "outgoing", outcome: "answered",
    kind: "audio", started_at: NOW - 3600, answered_at: NOW - 3590, ended_at: NOW - 3490, duration: 100, seen: true })];
  server.filtered_calls = [callEntry({ id: "clg_f", outcome: "filtered", peer: "/k/spammer", peer_handle: null })];
  server.unseen = 1;
  server.filtered = [{ message_id: "f1", direction: "in", from: "/k/stranger", peer: "/k/stranger", peer_handle: null,
    body: "buy cheap tokens", received_at: NOW - 50, read: false, filtered: true, sender_standing: "reported" }];
  server.policy = { accept_all: true, auto_accept_known: false, unknown_calls: "ring", unnamed_calls: "silence",
    unnamed_messages: "show", unknown_messages: "show", low_reputation: "filter", group_members_known: false, call_history_days: 180 };
  server.avatars = {};
  const base = server.fetch;
  server.fetch = async (url, opts = {}) => {
    const parsed = new URL(url, "https://postbox.pigeonpost.dev");
    const path = parsed.pathname, method = opts.method || "GET";
    const body = typeof opts.body === "string" ? JSON.parse(opts.body) : null;
    if (path === "/v1/capabilities") return response({ features: server.features });
    if (path === "/v1/calls/log" && method === "GET") {
      server.calls.push({ path, query: parsed.searchParams, method });
      const filter = parsed.searchParams.get("filter");
      const entries = filter === "filtered" ? server.filtered_calls
        : server.calls_log.filter(e => filter !== "missed" || (e.direction === "incoming" && e.outcome === "missed"));
      return response({ entries, unseen_missed: server.unseen, next_before: null });
    }
    if (path === "/v1/calls/log/seen") {
      server.calls.push({ path, method, body });
      server.unseen = 0;
      return response({ updated: 1, unseen_missed: 0 });
    }
    if (path.startsWith("/v1/calls/log") && method === "DELETE") {
      server.calls.push({ path, method, query: parsed.searchParams });
      const id = decodeURIComponent(path.split("/").pop());
      server.calls_log = server.calls_log.filter(e => e.id !== id);
      return response({ deleted: true });
    }
    if (path === "/v1/inbox" && parsed.searchParams.get("filtered") === "1") {
      server.calls.push({ path, query: parsed.searchParams, method });
      return response({ messages: server.filtered });
    }
    if (path === "/v1/filtered/accept") {
      server.calls.push({ path, method, body });
      const moved = server.filtered.filter(m => m.from === body.peer);
      server.filtered = server.filtered.filter(m => m.from !== body.peer);
      server.mailboxes[OWNER].messages.push(...moved.map(m => ({ ...m, filtered: false })));
      server.mailboxes[OWNER].contacts.push({ peer: body.peer, admission: "allow", autonomy: "review", allowed_verbs: [] });
      return response({ accepted: true, moved: moved.length });
    }
    if (path === "/v1/policy") {
      server.calls.push({ path, method, body, query: parsed.searchParams });
      if (method === "PUT") {
        if (server.policyFails) return response({ error: "unavailable" }, 503);
        const { identity, ...fields } = body;
        Object.assign(server.policy, fields);
      }
      return response(server.policy);
    }
    if (path === "/v1/report-spam") { server.calls.push({ path, method, body }); return response({ reported: true }); }
    if (path === "/v1/contacts" && method === "GET" && parsed.searchParams.get("admission") === "block") {
      server.calls.push({ path, query: parsed.searchParams, method });
      return response({ contacts: server.mailboxes[OWNER].contacts.filter(c => c.admission === "block") });
    }
    if (path === "/v1/avatars") {
      server.calls.push({ path, query: parsed.searchParams, method });
      const keys = parsed.searchParams.get("addresses").split(",");
      return response({ avatars: Object.fromEntries(keys.map(k => [k, server.avatars[k] || null])) });
    }
    if (path.startsWith("/v1/avatar-blobs/")) {
      server.calls.push({ path, method, auth: opts.headers?.authorization });
      return { ok: true, status: 200, blob: async () => ({ size: 3, type: "image/webp" }) };
    }
    if (path === "/v1/profile/avatar") {
      server.calls.push({ path, method, query: parsed.searchParams, body: opts.body, headers: opts.headers });
      if (method === "GET") return response({ avatar: server.ownAvatar || null });
      if (method === "PUT") {
        server.ownAvatar = { id: "av_new", version: 1, visibility: parsed.searchParams.get("visibility"), hash: "h-new",
          urls: { "64": "/v1/avatar-blobs/av_new/1/64.webp", "128": "/v1/avatar-blobs/av_new/1/128.webp", "256": "/v1/avatar-blobs/av_new/1/256.webp", "512": "/v1/avatar-blobs/av_new/1/512.webp", jpeg: "/v1/avatar-blobs/av_new/1/256.jpg" } };
        return response({ avatar: server.ownAvatar });
      }
      if (method === "PATCH") { server.ownAvatar = { ...server.ownAvatar, visibility: JSON.parse(opts.body).visibility, version: 2 }; return response({ avatar: server.ownAvatar }); }
      if (method === "DELETE") { server.ownAvatar = null; return response({ removed: true }); }
    }
    return base(url, opts);
  };
  configure(server);
  const errors = [], console = new VirtualConsole();
  console.on("jsdomError", e => errors.push(e.message));
  const dom = new JSDOM(read("index.html"), { url: "https://inbox.pigeonpost.dev/", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: console });
  const w = dom.window, $ = id => w.document.getElementById(id);
  Object.defineProperty(w, "crypto", { value: webcrypto });
  w.matchMedia = query => ({ matches: query.includes("min-width"), addEventListener() {} });
  w.fetch = server.fetch;
  let objects = 0;
  w.URL.createObjectURL = () => "blob:fixture-" + (++objects);
  w.URL.revokeObjectURL = () => {};
  w.localStorage.setItem("ppi_token", "fixture_account");
  w.eval(read("names.js"));
  w.eval(read("config.js"));
  w.eval(read("crop.js"));
  w.eval(read("app.js"));
  t.after(async () => { await tick(120); w.close(); assert.deepEqual(errors, [], "No uncaught script errors"); });
  await until(() => $("identity-btn").dataset.address === "/bekir/main");
  await tick(60);
  const clickPeer = async () => { await until(() => $("threads").querySelector(".thread-row")); $("threads").querySelector(".thread-row").click(); await tick(); };
  const called = (path, method = "GET") => server.calls.filter(c => c.path === path && c.method === method);
  return { w, $, server, clickPeer, called };
}

test("everything new stays hidden until the postbox switches it on", async t => {
  const a = await app(t, { features: [] });
  assert.equal(a.$("side-nav").hidden, true);
  assert.equal(a.$("filtered-entry").hidden, true);
  assert.equal(a.called("/v1/calls/log").length, 0, "no call history requests");
  assert.equal(a.server.calls.filter(c => c.query?.get?.("filtered") === "1").length, 0, "no filtered requests");
  assert.equal(a.called("/v1/avatars").length, 0, "no avatar requests");
  a.$("settings-btn").click();
  assert.equal(a.$("settings-nav-profile").hidden, true);
  assert.equal(a.$("settings-nav-privacy").hidden, true);
  assert.equal(a.$("settings-nav-blocked").hidden, false, "blocking needs no flag");
});

test("calls page lists calls, shows the missed badge and marks them seen", async t => {
  const a = await app(t);
  assert.equal(a.$("side-nav").hidden, false);
  assert.equal(a.$("calls-badge").hidden, false);
  assert.equal(a.$("calls-badge").textContent, "1");
  a.$("calls-btn").click();
  await until(() => a.$("call-log").children.length === 2);
  assert.equal(a.$("calls-view").hidden, false);
  assert.equal(a.$("threads").hidden, true);
  const rows = [...a.$("call-log").children];
  assert.ok(rows[0].classList.contains("call-missed"), "missed call is marked");
  assert.match(rows[0].querySelector(".call-sub").textContent, /Missed · Video/);
  assert.match(rows[1].querySelector(".call-sub").textContent, /Outgoing · Voice · 1:40/);
  await until(() => a.called("/v1/calls/log/seen", "POST").length === 1);
  assert.equal(a.called("/v1/calls/log/seen", "POST")[0].body.up_to, NOW - 120);
  await until(() => a.$("calls-badge").hidden);

  // Row actions: report spam and delete from history.
  rows[0].querySelector(".call-more").click();
  const menu = rows[0].querySelector(".call-menu");
  assert.equal(menu.hidden, false);
  [...menu.querySelectorAll("button")].find(b => b.textContent === "Report spam").click();
  await until(() => a.called("/v1/report-spam", "POST").length === 1);
  assert.equal(a.called("/v1/report-spam", "POST")[0].body.call_id, "clg_1");
  rows[0].querySelector(".call-more").click();
  [...menu.querySelectorAll("button")].find(b => b.textContent === "Delete from history").click();
  await until(() => a.$("call-log").children.length === 1);
  assert.equal(a.server.calls.find(c => c.method === "DELETE" && c.path === "/v1/calls/log/clg_1").query.get("identity"), OWNER);

  // The filtered section loads when opened.
  a.$("calls-filtered-box").open = true;
  a.$("calls-filtered-box").dispatchEvent(new a.w.Event("toggle"));
  await until(() => a.$("call-log-filtered").children.length === 1);
  assert.equal(a.$("calls-filtered-count").textContent, "(1)");

  // Missed filter asks the server for missed calls only.
  const missed = a.$("calls-filter").querySelector('input[value="missed"]');
  missed.checked = true; missed.dispatchEvent(new a.w.Event("change"));
  await until(() => a.called("/v1/calls/log").some(c => c.query.get("filter") === "missed"));
  a.$("calls-exit").click();
  assert.equal(a.$("threads").hidden, false);
});

test("a missed call appears as a system line in the 1:1 conversation", async t => {
  const a = await app(t);
  await a.clickPeer();
  await until(() => a.$("messages").querySelector(".system-line"));
  assert.match(a.$("messages").querySelector(".system-line").textContent, /^Missed video call · \d/);
  assert.equal(a.$("messages").querySelectorAll(".system-line").length, 1, "answered calls make no line");
});

test("filtered mail sits at the bottom of the list and can be accepted", async t => {
  const a = await app(t);
  await until(() => !a.$("filtered-entry").hidden);
  assert.equal(a.$("filtered-count").textContent, " (1)");
  assert.equal(a.$("threads").textContent.includes("buy cheap tokens"), false, "filtered mail is not in the inbox");
  a.$("filtered-entry").click();
  assert.equal(a.$("filtered-view").hidden, false);
  a.$("filtered-list").querySelector(".thread-row").click();
  assert.equal(a.$("filtered-sheet").hidden, false);
  assert.match(a.$("filtered-messages").textContent, /buy cheap tokens/);
  a.$("filtered-accept").click();
  await until(() => a.called("/v1/filtered/accept", "POST").length === 1);
  assert.deepEqual(a.called("/v1/filtered/accept", "POST")[0].body, { peer: "/k/stranger", identity: OWNER });
  await until(() => a.$("filtered-sheet").hidden && a.$("threads").hidden === false);
  await until(() => a.$("threads").textContent.includes("/k/stranger".slice(0, 12)));
  assert.equal(a.$("filtered-entry").hidden, true);
});

test("blocking from Filtered reports as spam by default", async t => {
  const a = await app(t);
  await until(() => !a.$("filtered-entry").hidden);
  a.$("filtered-entry").click();
  a.$("filtered-list").querySelector(".thread-row").click();
  a.$("filtered-block").click();
  assert.equal(a.$("peer-block-sheet").hidden, false);
  assert.equal(a.$("peer-block-report").checked, true);
  assert.equal(a.$("peer-block-wide-row").hidden, true, "a /k/ address has no namespace");
  a.$("peer-block-confirm").click();
  await until(() => a.called("/v1/report-spam", "POST").length === 1);
  const put = a.server.calls.find(c => c.path === "/v1/contacts" && c.method === "PUT");
  assert.equal(put.body.peer, "/k/stranger");
  assert.equal(put.body.admission, "block");
  assert.equal(a.called("/v1/report-spam", "POST")[0].body.message_id, "f1");
  assert.equal(a.$("peer-block-sheet").hidden, true);
});

test("block from the conversation header can report and cover the namespace", async t => {
  const a = await app(t);
  await a.clickPeer();
  a.$("peer-name-btn").click();
  a.$("peer-block").click();
  assert.match(a.$("peer-block-detail").textContent, /won't be able to message or call you, and won't be told/);
  assert.equal(a.$("peer-block-report").checked, false, "report is off by default here");
  assert.equal(a.$("peer-block-wide-row").hidden, false);
  assert.equal(a.$("peer-block-wide-label").textContent, "Block everyone at /alp");
  a.$("peer-block-report").checked = true;
  a.$("peer-block-confirm").click();
  await until(() => a.called("/v1/report-spam", "POST").length === 1);
  assert.equal(a.server.mailboxes[OWNER].contacts.find(c => c.peer === PEER).admission, "block");
  assert.ok(a.called("/v1/report-spam", "POST")[0].body.message_id);
  a.$("peer-info-done").click();
  await until(() => !a.$("blocked-banner").hidden);
  assert.equal(a.$("composer").hidden, true, "a blocked conversation has no composer");
  a.$("blocked-unblock").click();
  await until(() => a.$("blocked-banner").hidden);
  assert.equal(a.$("composer").hidden, false);

  // The wide option writes the namespace rule instead.
  a.$("peer-name-btn").click();
  a.$("peer-block").click();
  a.$("peer-block-wide").checked = true;
  a.$("peer-block-confirm").click();
  await until(() => a.server.mailboxes[OWNER].contacts.some(c => c.peer === "/alp/*" && c.admission === "block"));
});

test("a message offers Block sender", async t => {
  const a = await app(t);
  await a.clickPeer();
  const button = a.$("messages").querySelector(".msg-block");
  assert.ok(button);
  assert.equal(button.getAttribute("aria-label"), "Block sender");
  button.click();
  assert.equal(a.$("peer-block-sheet").hidden, false);
  a.$("peer-block-cancel").click();
});

test("privacy settings load from and save to the policy", async t => {
  const a = await app(t);
  a.$("settings-btn").click();
  assert.equal(a.$("settings-nav-privacy").hidden, false);
  a.$("settings-nav-privacy").click();
  await until(() => a.$("policy-list").querySelector('input[name="policy-unknown_calls"]:checked'));
  assert.equal(a.called("/v1/policy")[0].query.get("identity"), OWNER);
  const groups = [...a.$("policy-list").querySelectorAll(".policy-row h4")].map(h => h.textContent);
  assert.deepEqual(groups, ["Calls from people you don’t know", "Messages from addresses without a name",
    "Messages from people you don’t know", "Calls from addresses without a name", "Low-reputation senders", "Keep call history"]);
  assert.equal(a.$("policy-list").querySelector('input[name="policy-call_history_days"]:checked').value, "180");
  const silence = a.$("policy-list").querySelector('input[name="policy-unknown_calls"][value="silence"]');
  silence.checked = true; silence.dispatchEvent(new a.w.Event("change"));
  await until(() => a.called("/v1/policy", "PUT").length === 1);
  assert.deepEqual(a.called("/v1/policy", "PUT")[0].body, { unknown_calls: "silence", identity: OWNER });
  // Numbers stay numbers; a failure puts the old value back.
  a.server.policyFails = true;
  const off = a.$("policy-list").querySelector('input[name="policy-call_history_days"][value="0"]');
  off.checked = true; off.dispatchEvent(new a.w.Event("change"));
  await until(() => !a.$("policy-error").hidden);
  assert.equal(a.called("/v1/policy", "PUT")[1].body.call_history_days, 0);
  assert.equal(a.$("policy-list").querySelector('input[name="policy-call_history_days"]:checked').value, "180");
  assert.equal(a.$("policy-list").querySelector('input[name="policy-unknown_calls"]:checked').value, "silence");
});

test("the Blocked page lists exact and wildcard blocks and unblocks them", async t => {
  const a = await app(t, { configure: s => s.mailboxes[OWNER].contacts.push(
    { peer: "/k/badbadbad", admission: "block", autonomy: "review", allowed_verbs: [] },
    { peer: "/spamco/*", admission: "block", autonomy: "review", allowed_verbs: [] }) });
  a.$("settings-btn").click();
  a.$("settings-nav-blocked").click();
  await until(() => a.$("blocked-list").querySelectorAll("li button").length === 2);
  assert.match(a.$("blocked-list").textContent, /Everyone at \/spamco/);
  const unblock = [...a.$("blocked-list").querySelectorAll("button")].find(b => b.getAttribute("aria-label") === "Unblock /spamco/*");
  unblock.click();
  await until(() => a.$("blocked-list").querySelectorAll("li button").length === 1);
  const del = a.server.calls.find(c => c.path === "/v1/contacts" && c.method === "DELETE");
  assert.equal(del.query.get("peer"), "/spamco/*");
  assert.equal(del.query.get("identity"), OWNER);
  a.$("block-input").value = "/junk/*";
  a.$("block-add").click();
  await until(() => a.server.mailboxes[OWNER].contacts.some(c => c.peer === "/junk/*" && c.admission === "block"));
  a.$("block-input").value = "not an address";
  a.$("block-add").click();
  assert.equal(a.$("blocked-error").hidden, false);
});

test("profile pictures replace initials where visible and fall back to them otherwise", async t => {
  const a = await app(t, { configure: s => {
    s.avatars[PEER] = { id: "av_1", version: 3, hash: "abc", urls: { "64": "/v1/avatar-blobs/av_1/3/64.webp", "128": "/v1/avatar-blobs/av_1/3/128.webp", "256": "/v1/avatar-blobs/av_1/3/256.webp", "512": "/v1/avatar-blobs/av_1/3/512.webp", jpeg: "/v1/avatar-blobs/av_1/3/256.jpg" } };
  } });
  const row = () => a.$("threads").querySelector(".thread-row .avatar");
  await until(() => row()?.classList.contains("has-photo"), "peer picture painted");
  assert.match(row().style.backgroundImage, /blob:fixture-/);
  const blob = a.server.calls.find(c => c.path === "/v1/avatar-blobs/av_1/3/128.webp");
  assert.equal(blob.auth, "Bearer fixture_account", "pictures are fetched with the bearer token");
  // The mailbox's own avatar had no visible picture: initials remain.
  assert.equal(a.$("me-avatar").classList.contains("has-photo"), false);
  assert.ok(a.$("me-avatar").textContent.length > 0);
  // One batched lookup for every face on screen.
  assert.ok(a.called("/v1/avatars")[0].query.get("addresses").split(",").includes(PEER));
  // Call tiles get the same hook.
  const tile = a.w.document.createElement("span");
  a.w.PigeonpostCallsHost.decorateAvatar(tile, PEER);
  assert.equal(tile.classList.contains("has-photo"), true);
});

test("profile page uploads a cropped picture and changes its visibility", async t => {
  const a = await app(t);
  a.w.createImageBitmap = async () => ({ width: 800, height: 600 });
  a.w.HTMLCanvasElement.prototype.getContext = () => null; // jsdom has no canvas; drawing is skipped
  a.$("settings-btn").click();
  assert.equal(a.$("settings-nav-profile").hidden, false);
  a.$("settings-nav-profile").click();
  await until(() => a.called("/v1/profile/avatar").length === 1);
  assert.equal(a.$("profile-remove").hidden, true);
  const file = new a.w.File([new Uint8Array([1, 2, 3])], "me.jpg", { type: "image/jpeg" });
  Object.defineProperty(a.$("profile-file"), "files", { value: [file], configurable: true });
  a.$("profile-file").dispatchEvent(new a.w.Event("change"));
  await until(() => !a.$("crop-sheet").hidden);
  a.$("profile-all").checked = true;
  a.$("crop-save").click();
  await until(() => a.called("/v1/profile/avatar", "PUT").length === 1);
  const put = a.called("/v1/profile/avatar", "PUT")[0];
  assert.equal(put.query.get("crop"), "100,0,600,600", "centre square of an 800×600 picture");
  assert.equal(put.query.get("visibility"), "known");
  assert.equal(put.query.get("all"), "1");
  assert.equal(put.headers["content-type"], "image/jpeg");
  await until(() => a.$("crop-sheet").hidden && !a.$("profile-remove").hidden);
  const everyone = a.$("profile-visibility").querySelector('input[value="everyone"]');
  everyone.checked = true; everyone.dispatchEvent(new a.w.Event("change"));
  await until(() => a.called("/v1/profile/avatar", "PATCH").length === 1);
  assert.equal(JSON.parse(a.called("/v1/profile/avatar", "PATCH")[0].body).visibility, "everyone");
  a.$("profile-remove").click();
  await until(() => a.called("/v1/profile/avatar", "DELETE").length === 1);
  await until(() => a.$("profile-remove").hidden);
});

test("crop maths: the visible square in source pixels", () => {
  const ctx = {};
  vm.runInNewContext(read("crop.js"), ctx);
  const C = ctx.PigeonpostCrop;
  const img = { imgW: 800, imgH: 600, view: 300 };
  const start = C.centred({ ...img, zoom: 1 });
  assert.deepEqual({ ...C.cropRect({ ...img, zoom: 1, ...start }) }, { x: 100, y: 0, w: 600, h: 600 });
  // Zoom 2 about the centre: half the side, still centred.
  const z = C.rezoom({ ...img, from: 1, to: 2, ...start });
  assert.deepEqual({ ...C.cropRect({ ...img, zoom: 2, ...z }) }, { x: 250, y: 150, w: 300, h: 300 });
  // Dragging past an edge is clamped: the square never leaves the picture.
  assert.deepEqual({ ...C.cropRect({ ...img, zoom: 2, x: 500, y: 500 }) }, { x: 0, y: 0, w: 300, h: 300 });
  assert.deepEqual({ ...C.cropRect({ ...img, zoom: 2, x: -5000, y: -5000 }) }, { x: 500, y: 300, w: 300, h: 300 });
  // A downscaled picture keeps the same part of it.
  assert.deepEqual({ ...C.scaleRect({ x: 1000, y: 400, w: 2000, h: 2000 }, 0.5, 2048, 1536) }, { x: 500, y: 200, w: 1000, h: 1000 });
  // Portrait pictures centre vertically.
  const tall = { imgW: 400, imgH: 1000, view: 300 };
  assert.deepEqual({ ...C.cropRect({ ...tall, zoom: 1, ...C.centred({ ...tall, zoom: 1 }) }) }, { x: 0, y: 300, w: 400, h: 400 });
});
