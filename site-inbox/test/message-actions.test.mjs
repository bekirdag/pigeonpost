// Phase 1e in the web inbox: the action row under each bubble, ticks inside it, reply, edit,
// message info and the read-receipts setting, driven in jsdom against docs/message-actions.md.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";
import { fixtureServer, response, OWNER, PEER, SUBJECT } from "./fixtures.mjs";

const read = name => readFileSync(new URL("../" + name, import.meta.url), "utf8");
const tick = (ms = 30) => new Promise(resolve => setTimeout(resolve, ms));
async function until(predicate, what = "UI did not settle") {
  for (let n = 0; n < 200; n++) { if (predicate()) return; await tick(10); }
  assert.fail(what);
}

const ALL = ["message_edit", "message_reply", "receipts", "agent_status", "message_filtering"];
const NOW = Math.floor(Date.now() / 1000);
const REF = (c) => "mref_" + c.repeat(26);

function conversation() {
  return [
    { message_id: "in-1", msg_ref: REF("a"), direction: "in", from: "/k/peer", peer: PEER, peer_handle: PEER, thread_id: SUBJECT,
      body: "## Plan\nhello — are you the maintainer of pigeonpost?", received_at: NOW - 600, read: true, autonomy: "review" },
    { message_id: "out-1", msg_ref: REF("b"), direction: "out", from: OWNER, to: "/k/peer", peer: PEER, peer_handle: PEER, thread_id: SUBJECT,
      body: "Hi! Yes, that's me", sent_at: NOW - 500, received_at: NOW - 500, read: true,
      receipt: { state: "seen", delivered_at: NOW - 490, seen_at: NOW - 480 },
      reply_to: { msg_ref: REF("a"), message_id: "in-1", from: "/k/peer", direction: "in", excerpt: "Plan", quote: "Plan", verb: null, args: null } },
    { message_id: "out-2", msg_ref: REF("c"), direction: "out", from: OWNER, to: "/k/peer", peer: PEER, peer_handle: PEER, thread_id: SUBJECT,
      body: "Sorry I missed your call", sent_at: NOW - 400, received_at: NOW - 400, read: true,
      receipt: { state: "delivered", delivered_at: NOW - 390, seen_at: null }, edited_at: NOW - 100, edit_count: 1 },
    { message_id: "out-3", msg_ref: REF("d"), direction: "out", from: OWNER, to: "/k/peer", peer: PEER, peer_handle: PEER, thread_id: SUBJECT,
      body: JSON.stringify({ v: 1, verb: "full_access", args: { task: "deploy it" }, note: "deploy it" }), sent_at: NOW - 300, received_at: NOW - 300, read: true,
      receipt: { state: "seen", delivered_at: NOW - 290, seen_at: NOW - 280 }, agent_status: { state: "working", at: NOW - 270 },
      attachments: [{ id: "att1", filename: "report.pdf", media_type: "application/pdf", bytes: 1200 }] },
    { message_id: "out-old", msg_ref: REF("e"), direction: "out", from: OWNER, to: "/k/peer", peer: PEER, peer_handle: PEER, thread_id: SUBJECT,
      body: "old one", sent_at: NOW - 200, received_at: NOW - 200, read: true, receipt: { state: "sent", delivered_at: null, seen_at: null },
      agent_status: { state: "failed", at: NOW - 150 } },
  ];
}

async function app(t, { features = ALL, messages = conversation() } = {}) {
  const server = fixtureServer();
  server.mailboxes[OWNER].messages = messages;
  server.policy = { accept_all: true, auto_accept_known: false, unknown_messages: "show", unnamed_messages: "show", low_reputation: "filter", read_receipts: true };
  const base = server.fetch;
  server.fetch = async (url, opts = {}) => {
    const parsed = new URL(url, "https://postbox.pigeonpost.dev");
    const path = parsed.pathname, method = opts.method || "GET";
    const body = typeof opts.body === "string" ? JSON.parse(opts.body) : null;
    if (path === "/v1/capabilities") return response({ features });
    if (path === "/v1/inbox" && parsed.searchParams.get("filtered") === "1") return response({ messages: [] });
    if (path === "/v1/policy") {
      server.calls.push({ path, method, body });
      if (method === "PUT") { const { identity, ...fields } = body; Object.assign(server.policy, fields); }
      return response(server.policy);
    }
    if (path.startsWith("/v1/messages/") && path.endsWith("/info")) {
      server.calls.push({ path, method });
      return response({ message_id: "out-2", msg_ref: REF("c"), direction: "out", sent_at: NOW - 400,
        versions: [{ message_id: "out-2-0", body: "Sorry I missd your call", at: NOW - 400 }, { message_id: "out-2", body: "Sorry I missed your call", at: NOW - 100 }],
        receipt: { state: "delivered", delivered_at: NOW - 390, seen_at: null }, agent_status: null, reply_to: null });
    }
    return base(url, opts);
  };
  const errors = [], vc = new VirtualConsole();
  vc.on("jsdomError", e => errors.push(e.message));
  const dom = new JSDOM(read("index.html"), { url: "https://inbox.pigeonpost.dev/", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: vc });
  const w = dom.window, $ = id => w.document.getElementById(id);
  Object.defineProperty(w, "crypto", { value: webcrypto });
  w.matchMedia = query => ({ matches: query.includes("min-width"), addEventListener() {} });
  w.fetch = server.fetch;
  const clipboard = [];
  Object.defineProperty(w.navigator, "clipboard", { value: { writeText: async (s) => { clipboard.push(s); } } });
  w.localStorage.setItem("ppi_token", "fixture_account");
  w.eval(read("names.js"));
  w.eval(read("config.js"));
  w.eval(read("crop.js"));
  w.eval(read("app.js"));
  t.after(async () => { await tick(120); w.close(); assert.deepEqual(errors, [], "No uncaught script errors"); });
  await until(() => $("identity-btn").dataset.address === "/bekir/main");
  await tick(60);
  const open = async () => {
    await until(() => $("threads").querySelector(".thread-row"));
    $("threads").querySelector(".thread-row").click();
    await until(() => $("messages").querySelector("[data-message-id]"));
    await tick();
  };
  const row = id => $("messages").querySelector(`[data-message-id="${id}"]`);
  const called = (path, method = "GET") => server.calls.filter(c => c.path === path && c.method === method);
  return { w, $, server, open, row, called, clipboard };
}

// ---- pure rules ------------------------------------------------------------------------------

test("pure helpers: msg_ref, ticks, editable and quotes", async t => {
  const a = await app(t);
  const UI = a.w.PigeonpostMessageActions;
  for (let i = 0; i < 50; i++) assert.match(UI.newMsgRef(), /^mref_[a-z2-7]{26}$/);
  assert.notEqual(UI.newMsgRef(), UI.newMsgRef());
  assert.equal(UI.isMsgRef("mref_abc"), false);

  const flags = { edit: true, reply: true, receipts: true, agentStatus: true };
  const out = (over) => ({ kind: "out", status: "sent", msg_ref: REF("a"), at: NOW - 10, ...over });
  assert.equal(UI.tickState({ kind: "in" }, flags), null, "received mail has no ticks");
  assert.equal(UI.tickState(out(), { ...flags, receipts: false }), null, "no receipts flag, no ticks");
  assert.equal(UI.tickState(out({ status: "sending" }), flags), "pending");
  assert.equal(UI.tickState(out({ status: "failed" }), flags), null);
  assert.equal(UI.tickState(out(), flags), "sent");
  assert.equal(UI.tickState(out({ receipt: { state: "delivered" } }), flags), "delivered");
  assert.equal(UI.tickState(out({ receipt: { state: "seen" } }), flags), "seen");
  assert.equal(UI.tickState(out({ receipt: { state: "seen" }, agent_status: { state: "working" } }), flags), "working");
  assert.equal(UI.tickState(out({ receipt: { state: "seen" }, agent_status: { state: "done" } }), flags), "seen", "done reverts to the receipt");
  assert.equal(UI.tickState(out({ receipt: { state: "delivered" }, agent_status: { state: "failed" } }), flags), "agent-failed");
  assert.equal(UI.tickState(out({ agent_status: { state: "working" } }), { ...flags, agentStatus: false }), "sent");

  const now = NOW;
  assert.equal(UI.editable(out(), now, flags), true);
  assert.equal(UI.editable(out(), now, { ...flags, edit: false }), false, "flag off");
  assert.equal(UI.editable(out({ kind: "in" }), now, flags), false, "not yours");
  assert.equal(UI.editable(out({ status: "sending" }), now, flags), false);
  assert.equal(UI.editable(out({ msg_ref: null }), now, flags), false, "an old message has no reference");
  assert.equal(UI.editable(out({ at: now - 24 * 3600 - 1 }), now, flags), false, "older than 24 hours");
  assert.equal(UI.editable(out({ at: now - 24 * 3600 + 5 }), now, flags), true);
  assert.equal(UI.editable(out({ expires_at: now - 1 }), now, flags), false, "expired");

  assert.equal(UI.quoteExcerpt("\n## Heading\nsecond"), "Heading");
  assert.equal(UI.quoteExcerpt("- **bold** `code`"), "bold code");
  assert.equal(UI.quoteExcerpt("x".repeat(500)).length, 200);
  assert.equal(UI.quoteExcerpt("", [{ filename: "a.png" }]), "📎 a.png");
  assert.equal(UI.quoteExcerpt(""), "");

  const fields = UI.actionFields({ msg_ref: REF("z"), receipt: { state: "seen" }, edited_at: 5, edit_count: 2 });
  assert.deepEqual(JSON.parse(JSON.stringify(fields)), { msg_ref: REF("z"), reply_to: null, edited_at: 5, edit_count: 2, receipt: { state: "seen" }, agent_status: null });
});

// ---- the page --------------------------------------------------------------------------------

test("the inbox asks for the folded actions view only when the postbox has it", async t => {
  const on = await app(t);
  await until(() => on.server.calls.some(c => c.path === "/v1/inbox"));
  const q = on.server.calls.find(c => c.path === "/v1/inbox" && c.query.get("include_sent") === "true").query;
  assert.equal(q.get("actions"), "1");
  assert.equal(q.get("fold"), "1");

  const off = await app(t, { features: [] });
  await until(() => off.server.calls.some(c => c.path === "/v1/inbox"));
  const legacy = off.server.calls.find(c => c.path === "/v1/inbox").query;
  assert.equal(legacy.has("actions"), false);
  await off.open();
  assert.equal(off.$("messages").querySelectorAll(".ticks").length, 0, "no ticks without receipts");
  assert.equal(off.$("messages").querySelectorAll(".msg-action.reply, .msg-action.edit").length, 0, "no reply or edit without flags");
  assert.ok(off.$("messages").querySelector(".msg-actions .copy"), "copy is always there");
});

test("icons sit under the bubble, outside it: copy+reply received, edit+copy sent", async t => {
  const a = await app(t);
  await a.open();
  for (const id of ["in-1", "out-1", "out-2", "out-3", "out-old"]) {
    const li = a.row(id);
    const stack = li.querySelector(":scope > .msg-stack");
    assert.ok(stack, id + " has a stack");
    assert.equal(stack.children[0].className, "bubble", id + ": bubble first");
    assert.equal(stack.children[1].className, "msg-actions", id + ": then the action row");
    assert.equal(li.querySelector(".bubble .msg-action, .bubble .copy"), null, id + ": no action inside the bubble");
  }
  const kinds = id => [...a.row(id).querySelectorAll(".msg-actions .msg-action")].map(b => b.classList[1]);
  assert.deepEqual(kinds("in-1"), ["copy", "reply"]);
  assert.deepEqual(kinds("out-1"), ["edit", "copy"]);
  assert.deepEqual(kinds("out-3"), ["edit", "copy"], "a request with an attachment too");
  for (const b of a.$("messages").querySelectorAll(".msg-action")) assert.ok(b.getAttribute("aria-label"));

  a.row("in-1").querySelector(".msg-action.copy").click();
  await until(() => a.clipboard.length === 1);
  assert.equal(a.clipboard[0], "## Plan\nhello — are you the maintainer of pigeonpost?");
  a.row("out-3").querySelector(".msg-action.copy").click();
  await until(() => a.clipboard.length === 2);
  assert.equal(a.clipboard[1], "deploy it", "a request copies its words, not its JSON");
});

test("ticks, the gear and the edited label sit inside the bubble", async t => {
  const a = await app(t);
  await a.open();
  const tick = id => a.row(id).querySelector(".bubble .meta .ticks");
  assert.equal(a.row("in-1").querySelector(".ticks"), null, "received mail has none");
  assert.ok(tick("out-1").classList.contains("ticks-seen"));
  assert.match(tick("out-1").getAttribute("aria-label"), /^Seen \d/);
  assert.ok(tick("out-2").classList.contains("ticks-delivered"));
  assert.equal(tick("out-2").getAttribute("aria-label"), "Delivered");
  assert.ok(tick("out-3").classList.contains("ticks-working"));
  assert.equal(tick("out-3").getAttribute("aria-label"), "Agent working on this");
  assert.equal(tick("out-3").querySelector(".ticks-word").textContent, "Working…");
  assert.ok(tick("out-old").classList.contains("ticks-agent-failed"));
  assert.equal(a.row("out-2").querySelector(".bubble .meta .edited").textContent, "edited");
  assert.equal(a.row("out-1").querySelector(".edited"), null);
  // The meta row ends with the time and then the ticks.
  const meta = [...a.row("out-1").querySelector(".bubble .meta").children].map(c => c.className.split(" ")[0]);
  assert.deepEqual(meta.slice(-2), ["time", "ticks"]);
});

test("reply: quote above the composer, sent with reply_to, quote and msg_ref; the bubble quote jumps", async t => {
  const a = await app(t);
  await a.open();
  a.row("in-1").querySelector(".msg-action.reply").click();
  assert.equal(a.$("compose-context").hidden, false);
  assert.match(a.$("compose-context-title").textContent, /^Replying to /);
  assert.equal(a.$("compose-context-line").textContent, "Plan");

  // Esc cancels.
  a.$("compose").dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  assert.equal(a.$("compose-context").hidden, true);

  a.row("in-1").querySelector(".msg-action.reply").click();
  a.$("compose").value = "yes I am";
  a.$("compose").dispatchEvent(new a.w.Event("input"));
  a.$("composer").dispatchEvent(new a.w.Event("submit", { cancelable: true, bubbles: true }));
  await until(() => a.called("/v1/send", "POST").length === 1);
  const sent = a.called("/v1/send", "POST")[0].body;
  assert.equal(sent.reply_to, REF("a"));
  assert.equal(sent.quote, "Plan");
  assert.match(sent.msg_ref, /^mref_[a-z2-7]{26}$/);
  assert.equal(sent.edit_of, undefined);
  assert.equal(a.$("compose-context").hidden, true, "the bar goes once sent");

  const quote = a.row("out-1").querySelector(".bubble > .reply-quote");
  assert.ok(quote, "the reply shows its parent");
  assert.equal(quote.querySelector(".reply-line").textContent, "Plan");
  quote.click();
  assert.ok(a.row("in-1").classList.contains("flash"), "the parent is highlighted");
});

test("edit: composer in edit mode, Esc restores the draft, saving sends edit_of", async t => {
  const a = await app(t);
  await a.open();
  a.$("compose").value = "half-written";
  a.row("out-1").querySelector(".msg-action.edit").click();
  assert.equal(a.$("compose-context-title").textContent, "Editing message");
  assert.equal(a.$("compose").value, "Hi! Yes, that's me");
  a.$("compose").dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  assert.equal(a.$("compose-context").hidden, true);
  assert.equal(a.$("compose").value, "half-written", "the draft comes back");

  a.row("out-1").querySelector(".msg-action.edit").click();
  a.$("compose").value = "Hi! Yes, that's me 👋";
  a.$("compose").dispatchEvent(new a.w.Event("input"));
  a.$("composer").dispatchEvent(new a.w.Event("submit", { cancelable: true, bubbles: true }));
  await until(() => a.called("/v1/send", "POST").length === 1);
  const sent = a.called("/v1/send", "POST")[0].body;
  assert.equal(sent.edit_of, REF("b"));
  assert.equal(sent.to, PEER);
  assert.deepEqual(JSON.parse(sent.body), { v: 1, verb: "full_access", args: { task: "Hi! Yes, that's me 👋" }, note: "Hi! Yes, that's me 👋" });
  await until(() => a.$("compose").value === "half-written");
});

test("an edit the postbox refuses says why", async t => {
  const a = await app(t);
  a.server.intercept = (call) => call.path === "/v1/send" ? response({ error: "edit_window_closed", message: "too late" }, 403) : null;
  await a.open();
  a.row("out-1").querySelector(".msg-action.edit").click();
  a.$("compose").value = "changed";
  a.$("composer").dispatchEvent(new a.w.Event("submit", { cancelable: true, bubbles: true }));
  await until(() => !a.$("toast").hidden);
  assert.equal(a.$("toast").textContent, "Messages can only be edited for 24 hours.");
});

test("message info shows receipts and the version history", async t => {
  const a = await app(t);
  await a.open();
  a.row("out-2").querySelector(".ticks").click();
  await until(() => a.$("msg-info-body").querySelector(".info-versions"));
  assert.equal(a.$("msg-info-sheet").hidden, false);
  const text = a.$("msg-info-body").textContent;
  assert.match(text, /Delivered/);
  assert.match(text, /Seen—/);
  const versions = [...a.$("msg-info-body").querySelectorAll(".info-versions li")];
  assert.equal(versions.length, 2);
  assert.match(versions[0].textContent, /^Original/);
  assert.match(versions[1].textContent, /Sorry I missed your call$/);
  a.$("msg-info-done").click();
  assert.equal(a.$("msg-info-sheet").hidden, true);
});

test("right-click opens the full list", async t => {
  const a = await app(t);
  await a.open();
  a.row("in-1").querySelector(".bubble").dispatchEvent(new a.w.MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
  assert.equal(a.$("msg-menu").hidden, false);
  assert.deepEqual([...a.$("msg-menu").querySelectorAll("button")].map(b => b.textContent), ["Reply", "Copy", "Info", "Block sender"]);
  [...a.$("msg-menu").querySelectorAll("button")][0].click();
  assert.equal(a.$("msg-menu").hidden, true);
  assert.equal(a.$("compose-context").hidden, false);
  a.row("out-1").querySelector(".bubble").dispatchEvent(new a.w.MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
  assert.deepEqual([...a.$("msg-menu").querySelectorAll("button")].map(b => b.textContent), ["Reply", "Copy", "Edit", "Info"]);
});

test("acks carry with_edits; the read-receipts setting saves", async t => {
  const a = await app(t, { messages: [{ ...conversation()[0], read: false }] });
  await a.open();
  await until(() => a.called("/v1/ack", "POST").length === 1);
  assert.equal(a.called("/v1/ack", "POST")[0].body.with_edits, true);

  a.$("settings-btn").click();
  assert.equal(a.$("settings-nav-privacy").hidden, false);
  a.$("settings-nav-privacy").click();
  await until(() => a.$("policy-list").querySelector('input[name="policy-read_receipts"]:checked'));
  assert.equal(a.$("policy-list").querySelector('input[name="policy-read_receipts"]:checked').value, "true");
  assert.match(a.$("policy-list").textContent, /Send read receipts/);
  const off = a.$("policy-list").querySelector('input[name="policy-read_receipts"][value="false"]');
  off.checked = true;
  off.dispatchEvent(new a.w.Event("change"));
  await until(() => a.called("/v1/policy", "PUT").length === 1);
  assert.equal(a.called("/v1/policy", "PUT")[0].body.read_receipts, false);
});

test("the stylesheet keeps the gear still under reduced motion and shows the row on touch", () => {
  const css = read("app.css");
  assert.match(css, /@media \(prefers-reduced-motion: reduce\) \{\s*\.ticks-working svg \{ animation: none; \}\s*\.ticks-word \{ display: inline; \}/);
  assert.match(css, /@media \(hover: none\) \{\s*\.msg-actions \{ opacity: 1;/);
  assert.match(css, /\.messages > li:hover \.msg-actions,\s*\.messages > li:focus-within \.msg-actions \{ opacity: 1; \}/);
});
