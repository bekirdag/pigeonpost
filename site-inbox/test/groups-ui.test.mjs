// Phase 3a in the web inbox, driven in jsdom: the New group control and sheet, group rows and
// invitations, the conversation view (senders, notices, device changes, announcement mode),
// disappearing timers for groups and direct messages, and the feature gate. The MLS client is a
// stand-in here; groups-mls.test.mjs drives the real one through the WASM bundle.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";
import { fixtureServer, response, OWNER } from "./fixtures.mjs";

const read = (name) => readFileSync(new URL("../" + name, import.meta.url), "utf8");
const tick = (ms = 30) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(predicate, what = "UI did not settle") {
  for (let n = 0; n < 300; n++) { if (predicate()) return; await tick(10); }
  assert.fail(what);
}
const NOW = Math.floor(Date.now() / 1000);
const JANE = "/k/jane", JOHN = "/k/john";

function group(over = {}) {
  return {
    id: "grp_aaaaaaaaaaaaaaaaaaaaaaaaaa", name: "Garden club", description: "Weekend plans", join_policy: "private", post_policy: "everyone",
    visibility: "unlisted", members_can_invite: false, members_can_change_timer: false, ephemeral_secs: null, revision: 3, epoch: 2,
    activation_state: "active", member_count: 3, max_members: 1000, created_by: OWNER, created_at: NOW - 900, last_at: NOW - 60, last_seq: 9,
    deleted_at: null, has_link: false,
    my: { role: "owner", state: "active", read_cursor: 5, unread: 2, notify: "all", muted_until: null, invited_by: null, joined_at: NOW - 900, filtered: false, revision: 1 },
    ...over,
  };
}

function fakeClient(server) {
  const histories = server.histories;
  return {
    async init() { return { deviceId: "web-testdevice00", persistent: true }; },
    async topUpKeyPackages() { return 0; },
    async processWelcomes() { return []; },
    async sync(gid) { server.syncs.push(gid); return { processed: 0 }; },
    async processPending(gid) { server.pendings.push(gid); return null; },
    async activate() { return null; },
    async createGroup(settings) {
      const res = await server.fetch("https://postbox.pigeonpost.dev/v1/groups", { method: "POST", body: JSON.stringify(settings) });
      return res.json();
    },
    async send(gid, doc) { server.sent.push({ gid, doc }); (histories[gid] ||= []).push({ kind: "message", seq: 100, sender: OWNER, text: doc.text, files: [], created_at: NOW }); return { seq: 100 }; },
    async openFile() { return new Uint8Array([1, 2, 3]); },
    async pruneExpired() { return 0; },
    async dropSeqs(gid, seqs) { server.dropped.push({ gid, seqs }); histories[gid] = (histories[gid] || []).filter((x) => !seqs.includes(x.seq)); },
    async forget() {},
    inGroup: () => true,
    history: (gid) => (histories[gid] || []).slice(),
    safetyNumber: () => "123456789012345678901234567890",
    fingerprint: () => "11111 22222 33333 44444 55555 66666",
    deviceId: () => "web-testdevice00",
    members: () => [],
    epoch: () => 2,
  };
}

async function app(t, { features = ["groups", "disappearing"], groups = [group()], histories = {}, members = null, crypto = true, configure = () => {}, live = null } = {}) {
  const server = fixtureServer();
  Object.assign(server, { features, groups, histories, syncs: [], pendings: [], sent: [], dropped: [], gcalls: [] });
  server.members = members || [
    { address: OWNER, handle: "/bekir/main", role: "owner", state: "active", revision: 1 },
    { address: JANE, handle: "/jane/main", role: "admin", state: "active", revision: 2 },
    { address: JOHN, handle: null, role: "member", state: "active", revision: 1 },
    { address: "/k/invitee", handle: null, role: "member", state: "invited", revision: 1 },
  ];
  const base = server.fetch;
  server.fetch = async (url, opts = {}) => {
    const parsed = new URL(url, "https://postbox.pigeonpost.dev");
    const path = parsed.pathname, method = opts.method || "GET";
    const body = typeof opts.body === "string" ? JSON.parse(opts.body) : null;
    if (path === "/v1/capabilities") return response({ features: server.features });
    if (path.startsWith("/v1/groups") || path.startsWith("/v1/mls") || (path.startsWith("/v1/threads/") && method === "PATCH") || path.startsWith("/calls/")) {
      server.gcalls.push({ path, method, body, query: parsed.searchParams, headers: opts.headers || {} });
      if (path === "/v1/groups" && method === "GET") return response({ groups: server.groups });
      if (path === "/v1/groups" && method === "POST") {
        const g = group({ id: "grp_newnewnewnewnewnewnewnewnew", name: body.name, activation_state: "active", my: { ...group().my, unread: 0 } });
        server.groups.push(g);
        return response({ group: g, invitations: (body.invite || []).map((address) => ({ address, status: "invited" })) }, 201);
      }
      const m = /^\/v1\/groups\/([^/]+)(\/.*)?$/.exec(path);
      if (m) {
        const rest = m[2] || "";
        const g = server.groups.find((x) => x.id === decodeURIComponent(m[1]));
        if (rest === "/members") return response({ members: server.members });
        if (rest === "/invitation") { g.my.state = body.accept ? "keying" : "left"; return response({ ok: true }); }
        if (rest === "" && method === "PATCH") { Object.assign(g, body); g.revision += 1; return response({ group: g }); }
        if (rest === "/notifications") { return response({ ok: true }); }
        if (rest === "/read") return response({ ok: true });
        if (rest.startsWith("/members/")) return response({ ok: true });
      }
      if (path.startsWith("/v1/threads/")) return response({ thread: { thread_id: decodeURIComponent(path.split("/").pop()), ephemeral_secs: body.ephemeral_secs } });
      if (path.startsWith("/calls/")) return response({ active: false });
      return response({ error: "not_found" }, 404);
    }
    return base(url, opts);
  };
  configure(server);
  const errors = [], console = new VirtualConsole();
  console.on("jsdomError", (e) => errors.push(e.message));
  const dom = new JSDOM(read("index.html"), { url: "https://inbox.pigeonpost.dev/", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: console });
  const w = dom.window, $ = (id) => w.document.getElementById(id);
  Object.defineProperty(w, "crypto", { value: webcrypto });
  w.matchMedia = (query) => ({ matches: query.includes("min-width"), addEventListener() {} });
  w.fetch = server.fetch;
  w.URL.createObjectURL = () => "blob:fixture";
  w.URL.revokeObjectURL = () => {};
  w.localStorage.setItem("ppi_token", "fixture_account");
  w.eval(read("names.js"));
  w.eval(read("config.js"));
  w.eval(read("crop.js"));
  w.eval(read("groups-core.js"));
  // The MLS client is replaced; loading the library either succeeds (stand-in) or fails.
  w.PigeonpostGroupsCore.createClient = () => fakeClient(server);
  w.PigeonpostGroupCryptoLoader = crypto ? async () => ({ fingerprintDigits: () => "00000 00000 00000 00000 00000 00000" }) : async () => { throw new Error("no wasm"); };
  if (live) w.PigeonpostLive = live; // Phase 6: live.js's surface, stubbed
  w.eval(read("groups.js"));
  w.eval(read("app.js"));
  t.after(async () => { await tick(120); w.close(); assert.deepEqual(errors, [], "No uncaught script errors"); });
  await until(() => $("identity-btn").dataset.address === "/bekir/main");
  await tick(80);
  return { w, $, server };
}

test("groups stay hidden without the flag, and without the crypto library", async (t) => {
  const off = await app(t, { features: [] });
  assert.equal(off.$("new-group-btn"), null);
  assert.equal(off.$("groups-section"), null);
  assert.equal(off.server.gcalls.filter((c) => c.path === "/v1/groups").length, 0);

  const nolib = await app(t, { crypto: false });
  await tick(80);
  assert.ok(!nolib.$("new-group-btn") || nolib.$("new-group-btn").hidden, "no New group without encryption");
  assert.equal(nolib.server.gcalls.filter((c) => c.path === "/v1/groups").length, 0, "never falls back to an unencrypted group");
});

test("New group is a labelled control beside compose and the sheet creates the group", async (t) => {
  const a = await app(t);
  await until(() => a.$("new-group-btn") && !a.$("new-group-btn").hidden);
  const btn = a.$("new-group-btn");
  assert.equal(btn.previousElementSibling.id, "new-btn", "sits beside compose");
  assert.equal(btn.textContent.trim(), "New group");
  assert.equal(btn.getAttribute("aria-label"), "New group");
  btn.click();
  assert.equal(a.$("group-create-sheet").hidden, false);
  assert.equal(a.$("gc-as-row").hidden, false, "two mailboxes: the sending-as switcher shows");
  a.$("gc-who").value = "/alp/main";
  a.$("gc-who").dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  a.$("gc-who").value = "garden/bot";
  a.$("gc-who").dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  assert.equal(a.$("gc-chips").children.length, 2);
  a.$("gc-create").click();
  await tick();
  assert.equal(a.$("gc-error").hidden, false, "a name is required");
  a.$("gc-name").value = "Book club";
  a.w.document.querySelector('input[name="gc-join"][value="public"]').checked = true;
  a.w.document.querySelector('input[name="gc-post"][value="admins"]').checked = true;
  a.$("gc-timer").value = "86400";
  a.$("gc-create").click();
  await until(() => a.server.gcalls.some((c) => c.path === "/v1/groups" && c.method === "POST"));
  const post = a.server.gcalls.find((c) => c.path === "/v1/groups" && c.method === "POST");
  assert.deepEqual(post.body.invite, ["/alp/main", "/garden/bot"]);
  assert.equal(post.body.name, "Book club");
  assert.equal(post.body.join_policy, "public");
  assert.equal(post.body.post_policy, "admins");
  assert.equal(post.body.ephemeral_secs, 86400);
  await until(() => a.$("group-create-sheet").hidden && !a.$("group-view").hidden);
  assert.equal(a.$("group-head-name").textContent, "Book club");
});

test("group rows show the decrypted preview, unread badge and mute; invitations sit on top", async (t) => {
  const gid = group().id;
  const a = await app(t, {
    groups: [group({ my: { ...group().my, muted_until: NOW + 3600 } }), group({ id: "grp_inviteinviteinviteinvite", name: "Choir", my: { role: "member", state: "invited", invited_by: JANE, unread: 0 } })],
    histories: { [gid]: [{ kind: "message", seq: 7, sender: JANE, text: "see you saturday", files: [], created_at: NOW - 60 }] },
  });
  await until(() => a.$("groups-list") && a.$("groups-list").querySelector(".group-row"));
  const list = a.$("groups-list");
  const inv = a.$("group-invitations-btn");
  assert.match(inv.textContent, /Invitations \(1\)/);
  assert.ok(list.firstElementChild.contains(inv), "invitations row is first");
  const row = list.querySelector(".group-row");
  assert.match(row.querySelector(".tr-name").textContent, /Garden club/);
  assert.match(row.querySelector(".tr-preview").textContent, /: see you saturday$/);
  assert.equal(row.querySelector(".dot").textContent, "2");
  assert.ok(row.querySelector('[title="Muted"]'));
  inv.click();
  await until(() => a.w.document.querySelector(".invitation-card"));
  const join = [...a.w.document.querySelectorAll(".invitation-card button")].find((b) => b.textContent === "Join");
  join.click();
  await until(() => a.server.gcalls.some((c) => c.path.endsWith("/invitation")));
  assert.deepEqual(a.server.gcalls.find((c) => c.path.endsWith("/invitation")).body, { accept: true });
});

test("the group view shows senders once per run, notices, device changes and warnings", async (t) => {
  const gid = group().id;
  const fp = "ab".repeat(32);
  const a = await app(t, {
    histories: {
      [gid]: [
        { kind: "notice", seq: 1, notice: "group_created", actor: OWNER, metadata: {}, created_at: NOW - 300 },
        { kind: "commit", seq: 2, epoch: 1, committer: { mailbox: OWNER, device_id: "web-a" }, added: [{ mailbox: JANE, device_id: "web-j", fingerprint: fp }], removed: [], updated: [], created_at: NOW - 290 },
        { kind: "message", seq: 3, sender: JANE, text: "hi all", files: [], created_at: NOW - 200 },
        { kind: "message", seq: 4, sender: JANE, text: "second line", files: [], created_at: NOW - 199 },
        { kind: "message", seq: 5, sender: OWNER, text: "hello", files: [], created_at: NOW - 100 },
        { kind: "commit", seq: 6, epoch: 2, committer: { mailbox: JANE, device_id: "web-j" }, added: [{ mailbox: JANE, device_id: "phone-j", fingerprint: fp }], removed: [], updated: [], created_at: NOW - 90 },
        { kind: "notice", seq: 7, notice: "settings_changed", actor: JANE, metadata: { changed: ["post_policy"], post_policy: "admins" }, created_at: NOW - 80 },
        { kind: "warning", seq: 7.5, warning: "membership_mismatch", created_at: NOW - 80 },
        { kind: "message", seq: 8, sender: JOHN, deleted: true, created_at: NOW - 70 },
      ],
    },
  });
  await until(() => a.$("groups-list")?.querySelector(".group-row"));
  a.$("groups-list").querySelector(".group-row").click();
  await until(() => a.$("group-messages").children.length > 5);
  const ol = a.$("group-messages");
  const texts = [...ol.children].map((li) => li.textContent);
  assert.ok(texts.some((x) => /created the group/.test(x)));
  assert.ok(texts.some((x) => /jane.* joined|Jane joined|main joined/i.test(x)) || texts.some((x) => / joined$/.test(x)), "verified join line");
  assert.ok(texts.some((x) => /added a new device/.test(x)), "a new device of an existing member is shown");
  assert.ok(ol.querySelector(".security .fingerprint"), "with its fingerprint");
  assert.ok(texts.some((x) => /set posting to admins only/.test(x)));
  assert.ok(ol.querySelector(".group-warning"), "membership mismatch warning");
  assert.ok(texts.some((x) => /This message was deleted/.test(x)));
  const senders = [...ol.querySelectorAll(".group-sender")].map((s) => s.textContent);
  assert.equal(senders.filter((s) => s.length).length, 2, "Jane's two messages share one name; John's tombstone has one");
  assert.equal(ol.querySelectorAll("li.mine").length, 1);
  assert.equal(a.$("pane-thread").dataset.open, "true");
  assert.ok(a.server.syncs.includes(gid), "opening a group syncs it");
});

test("announcement mode replaces the composer for members", async (t) => {
  const a = await app(t, { groups: [group({ post_policy: "admins", my: { ...group().my, role: "member" } })] });
  await until(() => a.$("groups-list")?.querySelector(".group-row"));
  a.$("groups-list").querySelector(".group-row").click();
  await until(() => !a.$("group-view").hidden);
  assert.equal(a.$("group-composer").hidden, true);
  assert.equal(a.$("group-announce").hidden, false);
  assert.equal(a.$("group-announce").textContent, "Only admins can send messages");
});

test("sending goes through the encrypted client, never the direct-message route", async (t) => {
  const a = await app(t);
  await until(() => a.$("groups-list")?.querySelector(".group-row"));
  a.$("groups-list").querySelector(".group-row").click();
  await until(() => !a.$("group-view").hidden && !a.$("group-composer").hidden);
  a.$("group-compose").value = "plaintext stays here";
  a.$("group-composer").requestSubmit();
  await until(() => a.server.sent.length === 1);
  assert.equal(a.server.sent[0].doc.text, "plaintext stays here");
  assert.equal(a.server.calls.filter((c) => c.path === "/v1/send").length, 0);
  assert.ok(!a.server.gcalls.some((c) => JSON.stringify(c.body || {}).includes("plaintext stays here")), "no plaintext to the postbox");
});

test("disappearing: group timer in header, expired items hidden, info control patches the group", async (t) => {
  const gid = group().id;
  const a = await app(t, {
    groups: [group({ ephemeral_secs: 86400 })],
    histories: { [gid]: [
      { kind: "message", seq: 3, sender: JANE, text: "gone", files: [], created_at: NOW - 90000, expires_at: NOW - 3600 },
      { kind: "message", seq: 4, sender: JANE, text: "still here", files: [], created_at: NOW - 60, expires_at: NOW + 86000 },
    ] },
  });
  await until(() => a.$("groups-list")?.querySelector(".group-row"));
  a.$("groups-list").querySelector(".group-row").click();
  await until(() => a.$("group-messages").textContent.includes("still here"));
  assert.ok(!a.$("group-messages").textContent.includes("gone"), "expired message not rendered");
  assert.match(a.$("group-head-sub").textContent, /1 day/);
  assert.ok(a.$("group-messages").querySelector("li.ephemeral .timer-icon"));
  a.$("group-info-btn").click();
  await until(() => a.$("gi-timer"));
  assert.equal(a.$("gi-timer").value, "86400");
  a.$("gi-timer").value = "3600";
  a.$("gi-timer").dispatchEvent(new a.w.Event("change"));
  await until(() => a.server.gcalls.some((c) => c.method === "PATCH" && c.path === "/v1/groups/" + gid));
  const patch = a.server.gcalls.find((c) => c.method === "PATCH" && c.path === "/v1/groups/" + gid);
  assert.deepEqual(patch.body, { expected_revision: 3, ephemeral_secs: 3600 });
  assert.ok(a.$("gi-safety").textContent.includes("12345"), "safety number shown");
});

test("message.expired drops group items; group.message triggers a sync", async (t) => {
  const gid = group().id;
  const a = await app(t, { histories: { [gid]: [{ kind: "message", seq: 3, sender: JANE, text: "short lived", files: [], created_at: NOW - 60 }] } });
  await until(() => a.$("groups-list")?.querySelector(".group-row"));
  const before = a.server.syncs.length;
  a.w.PigeonpostGroups.onEvent("group.message", { type: "group.message", data: { group_id: gid, seq: 10, sender: JANE } });
  await until(() => a.server.syncs.length > before);
  a.w.PigeonpostGroups.onEvent("message.expired", { type: "message.expired", data: { group_id: gid, seqs: [3] } });
  await until(() => a.server.dropped.length === 1);
  assert.deepEqual(a.server.dropped[0], { gid, seqs: [3] });
});

test("direct messages: the timer control sits in sender info and patches the thread", async (t) => {
  const a = await app(t, { features: ["disappearing"] });
  await until(() => a.$("threads").querySelector(".thread-row"));
  a.$("threads").querySelector(".thread-row").click();
  await tick(60);
  a.w.PigeonpostGroups.render();
  await until(() => a.$("dm-timer-section"));
  const sel = a.$("dm-timer");
  assert.equal(sel.value, "");
  sel.value = "300";
  sel.dispatchEvent(new a.w.Event("change"));
  await until(() => a.server.gcalls.some((c) => c.method === "PATCH" && c.path.startsWith("/v1/threads/")));
  const call = a.server.gcalls.find((c) => c.method === "PATCH" && c.path.startsWith("/v1/threads/"));
  assert.deepEqual(call.body, { ephemeral_secs: 300 });
  assert.equal(call.query.get("identity"), OWNER);
});

test("the events stream asks for typed events", async (t) => {
  const a = await app(t);
  await tick(60);
  const ev = a.server.calls.find((c) => c.path === "/v1/events");
  assert.ok(ev, "stream opened");
  assert.equal(ev.query.get("typed"), "1");
});

test("Phase 1e: group bubbles get the action row, ticks from member cursors, reply and edit", async (t) => {
  const gid = group().id;
  const a = await app(t, {
    features: ["groups", "receipts", "message_edit", "message_reply"],
    histories: {
      [gid]: [
        { kind: "message", seq: 3, sender: JANE, text: "Shall we meet Saturday?", files: [], created_at: NOW - 200 },
        { kind: "message", seq: 5, sender: OWNER, text: "Yes, 10am", files: [], reply_to: "3", created_at: NOW - 100 },
      ],
    },
    configure: (server) => {
      const inner = server.fetch;
      server.fetch = async (url, opts = {}) => {
        if (String(url).includes("/receipts")) {
          server.receiptCalls = (server.receiptCalls || 0) + 1;
          return response({ members: server.receiptMembers, read_receipts: true });
        }
        return inner(url, opts);
      };
      server.receiptMembers = [
        { address: JANE, handle: "/jane/main", delivered_seq: 5, seen_seq: 5 },
        { address: JOHN, handle: null, delivered_seq: 5, seen_seq: null },
      ];
    },
  });
  await until(() => a.$("groups-list")?.querySelector(".group-row"));
  a.$("groups-list").querySelector(".group-row").click();
  await until(() => a.$("group-messages").querySelector('li[data-seq="5"] .ticks-seen'));
  const li = (seq) => a.$("group-messages").querySelector(`li[data-seq="${seq}"]`);
  const kinds = (seq) => [...li(seq).querySelectorAll(".msg-actions .msg-action")].map((b) => b.classList[1]);
  assert.deepEqual(kinds(3), ["copy", "reply"]);
  assert.deepEqual(kinds(5), ["edit", "copy"]);
  assert.equal(li(5).querySelector(":scope > .msg-stack > .bubble + .msg-actions") !== null, true, "the row is outside the bubble");
  assert.ok(li(5).querySelector(".ticks").classList.contains("ticks-seen"), "every visible member has read it");
  assert.equal(li(3).querySelector(".ticks"), null, "others' messages carry no ticks");
  assert.equal(li(5).querySelector(".reply-quote .reply-line").textContent, "Shall we meet Saturday?");

  li(5).querySelector(".ticks").click();
  await until(() => !a.w.document.getElementById("group-msg-info-sheet")?.hidden);
  assert.match(a.w.document.getElementById("group-msg-info-sheet").textContent, /Seen \(\d\)/);

  li(3).querySelector(".msg-action.reply").click();
  assert.equal(a.$("group-compose-context").hidden, false);
  a.$("group-compose").value = "count me in";
  a.$("group-composer").requestSubmit();
  await until(() => a.server.sent.length === 1);
  assert.equal(a.server.sent[0].doc.reply_to, 3);

  await until(() => li(5));
  li(5).querySelector(".msg-action.edit").click();
  assert.equal(a.$("group-compose").value, "Yes, 10am");
  a.$("group-compose").value = "Yes, 11am";
  a.$("group-composer").requestSubmit();
  await until(() => a.server.sent.length === 2);
  assert.equal(a.server.sent[1].doc.edit_of, 5);
  assert.equal(a.server.sent[1].doc.reply_to, "3", "an edited reply stays a reply");
});

test("a live stream shows a LIVE banner with Watch, Go live opens its sheet, and the stream notice reads as a sentence", async (t) => {
  const gid = group().id;
  const calls = [];
  const live = {
    supported: () => true, current: () => null, show() {}, onGroupStream() {}, onGroupMember() {},
    watch: async (g, sid) => { calls.push(["watch", g, sid]); return { ok: true }; },
    goLive: async (g, opts) => { calls.push(["goLive", g, opts]); return { ok: true }; },
  };
  let streams = [{ id: "str_" + "q".repeat(26), group_id: gid, title: "Sprint demo", by: JANE, state: "live", viewers: 4, stage: [], sources: ["camera"] }];
  const a = await app(t, {
    features: ["groups", "streaming"], live,
    groups: [group({ stream_policy: "admins" })],
    histories: { [gid]: [{ kind: "notice", seq: 8, notice: "stream", actor: JANE, metadata: { stream_id: "str_x", duration: 2040, viewers: 18 }, created_at: NOW - 30 }] },
    configure: (server) => {
      const inner = server.fetch;
      server.fetch = async (url, opts = {}) => {
        const path = new URL(url, "https://postbox.pigeonpost.dev").pathname;
        if (path === `/v1/groups/${gid}/streams`) { server.gcalls.push({ path, method: opts.method || "GET" }); return response({ streams }); }
        return inner(url, opts);
      };
    },
  });
  await until(() => a.$("groups-list") && a.$("groups-list").querySelector(".group-row"));
  a.$("groups-list").querySelector(".group-row").click();
  await until(() => !a.$("group-live-banner").hidden, "the LIVE banner shows");
  assert.equal(a.$("group-live-text").textContent, "Jane is live: Sprint demo");
  assert.equal(a.$("group-live-watch").textContent, "Watch");
  assert.ok(a.$("group-live-btn").hidden, "nobody else goes live while someone is");
  a.$("group-live-banner").click();
  await until(() => calls.length === 1);
  assert.deepEqual([calls[0][0], calls[0][1].id, calls[0][2]], ["watch", gid, streams[0].id]);
  const notice = [...a.w.document.querySelectorAll(".group-notice")].map((n) => n.textContent);
  assert.ok(notice.includes("Jane streamed for 34 min · 18 viewers"), JSON.stringify(notice));
  // The stream ends: the banner goes, and the owner may go live.
  streams = [];
  a.w.PigeonpostGroups.onEvent("group.stream", { mailbox: OWNER, data: { group_id: gid, stream_id: "str_" + "q".repeat(26), state: "ended", by: JANE, title: "Sprint demo" } });
  await until(() => a.$("group-live-banner").hidden && !a.$("group-live-btn").hidden, "Go live is offered to an admin");
  a.$("group-live-btn").click();
  assert.equal(a.$("group-live-sheet").hidden, false);
  a.$("gl-start").click();
  assert.equal(a.$("gl-error").hidden, false, "a title is required");
  a.$("gl-title").value = "Town hall";
  a.$("gl-start").click();
  await until(() => calls.length === 2);
  assert.deepEqual(JSON.parse(JSON.stringify(calls[1][2])), { title: "Town hall", camera: true, screen: false });
  // Who may go live is a group setting.
  a.$("group-info-btn").click();
  await until(() => a.$("gi-live"));
  assert.equal(a.$("gi-live").value, "admins");
});

test("without the streaming flag there is no banner and no Go live", async (t) => {
  const live = { supported: () => true, current: () => null, onGroupStream() {}, onGroupMember() {} };
  const a = await app(t, { features: ["groups"], live });
  await until(() => a.$("groups-list") && a.$("groups-list").querySelector(".group-row"));
  a.$("groups-list").querySelector(".group-row").click();
  await tick(80);
  assert.ok(a.$("group-live-btn").hidden && a.$("group-live-banner").hidden);
  assert.ok(!a.server.gcalls.some((c) => c.path.endsWith("/streams")), "never asks for streams");
});
