import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";
import { fixtureServer, response, OWNER, SECOND, PEER, SUBJECT } from "./fixtures.mjs";

const read = name => readFileSync(new URL("../" + name, import.meta.url), "utf8");
const tick = (ms = 30) => new Promise(resolve => setTimeout(resolve, ms));
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };
async function until(predicate) {
  for (let n = 0; n < 200; n++) { if (predicate()) return; await tick(10); }
  assert.fail("UI did not settle");
}

async function app(t, configure = () => {}) {
  const server = fixtureServer();
  configure(server);
  const errors = [], console = new VirtualConsole();
  console.on("jsdomError", e => errors.push(e.message));
  const dom = new JSDOM(read("index.html"), { url: "https://inbox.pigeonpost.dev/", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: console });
  const w = dom.window, $ = id => w.document.getElementById(id);
  Object.defineProperty(w, "crypto", { value: webcrypto });
  w.matchMedia = query => ({ matches: query.includes("min-width"), addEventListener() {} });
  w.fetch = server.fetch;
  w.localStorage.setItem("ppi_token", "fixture_account");
  w.eval(read("config.js"));
  w.eval(read("app.js"));
  t.after(() => { w.close(); assert.deepEqual(errors, [], "No uncaught script errors"); });
  await until(() => $("identity-btn").dataset.address === "/bekir/main");
  const clickPeer = async () => { await until(() => $("threads").querySelector(".thread-row")); $("threads").querySelector(".thread-row").click(); await tick(); };
  const switchTo = async address => {
    $("identity-btn").click();
    const button = [...$("identity-menu").querySelectorAll("li")].find(li => li.querySelector(".copy-address").dataset.address === address)?.querySelector(".identity-option");
    assert.ok(button); button.click(); await tick();
  };
  const refresh = async () => { w.dispatchEvent(new w.Event("online")); await tick(); };
  const input = (id, value) => { $(id).value = value; $(id).dispatchEvent(new w.Event("input", { bubbles: true })); };
  return { w, $, server, clickPeer, switchTo, refresh, input };
}

test("default inbox labels distinguish owners and picker groups roots before children and raw keys", async t => {
  const a = await app(t, server => server.identities.push(
    { address: "/k/z", handle: "/garden/worker" }, { address: "/k/anonymous", label: "Unnamed" },
    { address: "/k/a", handle: "/apple/main" }, { address: "/k/b", handle: "/apple/bot" }));
  assert.equal(a.$("me-name").textContent, "/bekir");
  assert.equal(a.$("me-sub"), null);
  assert.equal(a.$("copy-my-address"), null);
  a.$("identity-btn").click();
  assert.deepEqual([...a.$("identity-menu").querySelectorAll(".copy-address")].map(x => x.dataset.address),
    ["/bekir/main", "/apple/main", "/apple/bot", "/garden/main", "/garden/worker", "/k/anonymous"]);
  await a.clickPeer();
  assert.equal(a.$("peer-name").textContent, "/alp");
});

test("rapid A→B→A switching rejects delayed inbox, contact, archive and subject responses", async t => {
  const pending = [];
  const a = await app(t, server => { server.intercept = call => {
    if (call.query.get("identity") !== SECOND || call.query.has("wait")) return;
    const shapes = { "/v1/inbox": { messages: [{ ...server.mailboxes[SECOND].messages[0], peer_handle: "/wrong/main" }] },
      "/v1/contacts": { contacts: [{ peer: "/wrong/main", alias: "Wrong account" }] },
      "/v1/threads": { threads: [{ thread_id: "wrong", peer: PEER, title: "Wrong thread" }] },
      "/v1/archive": { archived: [PEER] } };
    if (!shapes[call.path]) return;
    const d = deferred(); pending.push(() => d.resolve(response(shapes[call.path]))); return d.promise;
  }; });
  await a.switchTo("/garden/main");
  assert.equal(a.$("inbox-status").hidden, false);
  assert.equal(a.$("identity-btn").disabled, false);
  await a.switchTo("/bekir/main");
  pending.forEach(resolve => resolve());
  await a.clickPeer();
  assert.equal(a.$("me-name").textContent, "/bekir");
  assert.doesNotMatch(a.$("app").textContent, /Wrong account|Wrong thread|\/wrong/);
  assert.equal(a.$("inbox-status").hidden, true);
  assert.ok(a.$("messages").textContent.includes(OWNER));
});

test("old requests cannot resurrect inbox state after sign-out", async t => {
  const a = await app(t);
  const late = deferred();
  a.server.intercept = call => call.path === "/v1/inbox" && !call.query.has("wait") ? late.promise : undefined;
  await a.refresh();
  a.$("signout-btn").click();
  late.resolve(response({ messages: a.server.mailboxes[OWNER].messages }));
  await tick();
  assert.equal(a.$("app").hidden, true);
  assert.equal(a.$("signin").hidden, false);
  assert.equal(a.w.localStorage.getItem("ppi_token"), null);
});

test("history starts at ten and search finds older messages in a bounded window", async t => {
  const a = await app(t, s => {
    s.mailboxes[OWNER].messages[1].body = "The **needle** appears in early history";
    s.mailboxes[OWNER].messages[20].body = "Another needle nearby";
  });
  await a.clickPeer();
  const rows = () => [...a.$("messages").querySelectorAll("[data-message-id]")];
  assert.equal(rows().length, 10);
  assert.equal(rows()[0].dataset.messageId, `${OWNER}-26`);
  a.$("load-older").click();
  assert.equal(rows().length, 20);
  a.$("find-btn").click();
  a.input("find-input", "needle");
  await tick(220);
  assert.equal(a.$("find-count").textContent, "1 of 2");
  assert.ok(rows().length <= 10);
  assert.ok(a.$("messages").textContent.includes("early history"));
  assert.equal(a.$("messages").querySelector("mark").textContent, "needle");
  a.$("find-next").click();
  assert.equal(a.$("find-count").textContent, "2 of 2");
  a.$("find-next").click();
  assert.equal(a.$("find-count").textContent, "1 of 2");
  a.$("jump-latest").click();
  assert.equal(rows().length, 10);
  assert.equal(rows().at(-1).dataset.messageId, `${OWNER}-35`);
});

test("an unchanged refresh keeps the rendered message nodes and inline attachments", async t => {
  const a = await app(t, s => { s.mailboxes[OWNER].messages.at(-1).attachments = [{ id: "attachment", filename: "release.pdf", bytes: 256 }]; });
  await a.clickPeer();
  const last = a.$("messages").lastElementChild;
  assert.equal(last.querySelector(".file").textContent, "release.pdf");
  await a.refresh();
  assert.equal(a.$("messages").lastElementChild, last);
});

test("sent requests do not display an invented held verdict", async t => {
  const a = await app(t, server => {
    server.mailboxes[OWNER].messages.at(-1).body = JSON.stringify({ v: 1, verb: "full_access", args: { task: "My request" }, note: "My request" });
  });
  await a.clickPeer();
  const sent = a.$("messages").lastElementChild;
  assert.match(sent.textContent, /My request/);
  assert.equal(sent.querySelector(".decision"), null);
});

test("pasted addresses get one slash, preserve selection, and require a first message", async t => {
  const a = await app(t);
  a.$("new-btn").click();
  const field = a.$("new-peer");
  assert.equal(field.value, "/");
  field.value = "alp/main"; field.setSelectionRange(3, 3);
  field.dispatchEvent(new a.w.Event("input"));
  assert.equal(field.value, "/alp/main");
  assert.equal(field.selectionStart, 4);
  a.input("new-peer", "  ///alp/main");
  assert.equal(field.value, "/alp/main");
  a.$("new-send").click(); await tick();
  assert.equal(a.server.calls.filter(c => c.path === "/v1/send").length, 0);
  a.input("new-body", "Hello"); a.$("new-send").click(); await tick();
  const send = a.server.calls.find(c => c.path === "/v1/send");
  assert.equal(send.body.to, PEER);
  assert.equal(send.body.from, OWNER);
});

test("an upload started in A cannot send from B and its draft is kept for A", async t => {
  const upload = deferred();
  const a = await app(t);
  a.server.intercept = call => call.path === "/v1/attachments" ? upload.promise : undefined;
  await a.clickPeer();
  a.input("compose", "Keep this with the original inbox");
  Object.defineProperty(a.$("file-input"), "files", { value: [new a.w.File(["data"], "notes.txt")] });
  a.$("file-input").dispatchEvent(new a.w.Event("change"));
  a.$("composer").dispatchEvent(new a.w.Event("submit", { cancelable: true }));
  await until(() => a.server.calls.some(c => c.path === "/v1/attachments"));
  await a.switchTo("/garden/main");
  upload.resolve(response({ id: "old-file" }, 201));
  await tick();
  assert.equal(a.server.calls.filter(c => c.path === "/v1/send").length, 0);
  assert.equal(a.server.calls.find(c => c.path === "/v1/attachments").opts.headers["x-pigeonpost-identity"], OWNER);
  await a.switchTo("/bekir/main"); await a.clickPeer();
  assert.equal(a.$("compose").value, "Keep this with the original inbox");
  assert.equal(a.$("pending-files").children.length, 1);
});

test("starting with a namespace opens the canonical peer returned in the sent copy", async t => {
  const a = await app(t);
  a.server.intercept = call => {
    if (call.path !== "/v1/send") return;
    a.server.mailboxes[OWNER].messages.push({ message_id: "resolved-copy", direction: "out", peer: "/k/peer",
      peer_handle: PEER, thread_id: SUBJECT, body: call.body.body, received_at: 1789800000, read: true });
    return response({ message_id: "delivered", sent_copy_id: "resolved-copy" }, 201);
  };
  a.$("new-btn").click(); a.input("new-peer", "alp"); a.input("new-body", "Hello at your main address");
  a.$("new-send").click();
  await until(() => !a.$("thread-head").hidden);
  assert.equal(a.$("peer-name").textContent, "/alp");
  assert.match(a.$("messages").textContent, /Hello at your main address/);
});

test("drafts stay with their subject instead of following another conversation", async t => {
  const a = await app(t); await a.clickPeer();
  a.input("compose", "Draft for development");
  [...a.$("subs").querySelectorAll(".sub-row")].find(row => row.textContent.includes("Release notes")).click();
  assert.equal(a.$("compose").value, "");
  a.input("compose", "Release draft");
  [...a.$("subs").querySelectorAll(".sub-row")].find(row => row.textContent.includes("Development ideas")).click();
  assert.equal(a.$("compose").value, "Draft for development");
});

test("thread deletion supports cancel and retry and only deletes the selected subject", async t => {
  const a = await app(t); await a.clickPeer();
  a.$("delete-thread-btn").click();
  assert.equal(a.w.document.activeElement.id, "delete-thread-cancel");
  a.$("delete-thread-cancel").click();
  assert.equal(a.server.calls.some(c => c.method === "DELETE"), false);
  a.server.intercept = call => call.method === "DELETE" ? response({}, 503) : undefined;
  a.$("delete-thread-btn").click(); a.$("delete-thread-confirm").click(); await tick();
  assert.equal(a.$("delete-thread-error").hidden, false);
  assert.equal(a.$("delete-thread-sheet").hidden, false);
  a.server.intercept = null;
  a.$("delete-thread-confirm").click(); await tick();
  assert.equal(a.$("delete-thread-sheet").hidden, true);
  assert.equal(a.server.calls.find(c => c.method === "DELETE").query.get("identity"), OWNER);
  assert.equal(a.server.mailboxes[OWNER].threads.length, 1);
  assert.equal(a.server.mailboxes[SECOND].threads.length, 2);
});

test("Escape dismisses the dialog before the conversation and IME Return does not send", async t => {
  const a = await app(t); await a.clickPeer();
  a.$("delete-thread-btn").click();
  a.w.document.dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
  assert.equal(a.$("delete-thread-sheet").hidden, true);
  assert.equal(a.$("thread-head").hidden, false);
  a.input("compose", "still composing");
  a.$("compose").dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "Enter", isComposing: true, bubbles: true, cancelable: true }));
  await tick();
  assert.equal(a.server.calls.some(c => c.path === "/v1/send"), false);
  a.w.document.dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "f", ctrlKey: true, bubbles: true, cancelable: true }));
  assert.equal(a.w.document.activeElement.id, "find-input");
});

test("column resizing has a keyboard equivalent and saves the chosen width", async t => {
  const a = await app(t), divider = a.$("list-resizer");
  const before = Number(divider.getAttribute("aria-valuenow"));
  divider.dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true, cancelable: true }));
  assert.equal(Number(divider.getAttribute("aria-valuenow")), before + 10);
  assert.equal(JSON.parse(a.w.localStorage.getItem("ppi_columns")).list, before + 10);
});

test("a delayed older snapshot cannot replace a newer snapshot of the same mailbox", async t => {
  const a = await app(t); await a.clickPeer();
  const old = deferred(); let count = 0;
  a.server.intercept = call => {
    if (call.path !== "/v1/inbox" || call.query.has("wait")) return;
    if (++count === 1) return old.promise;
  };
  await a.refresh();
  a.server.mailboxes[OWNER].messages.at(-1).body = "This is the current server snapshot";
  await a.refresh();
  old.resolve(response({ messages: [] })); await tick();
  assert.match(a.$("messages").textContent, /current server snapshot/);
  assert.equal(a.$("inbox-status").hidden, true);
});

test("read acknowledgements cover the displayed page and never change mailbox midway", async t => {
  const ack = deferred();
  const a = await app(t, server => {
    server.mailboxes[OWNER].messages.forEach(m => { m.direction = "in"; m.read = false; });
    server.intercept = call => call.path === "/v1/ack" ? ack.promise : undefined;
  });
  await a.clickPeer();
  const first = a.server.calls.find(c => c.path === "/v1/ack");
  assert.equal(first.body.message_id, `${OWNER}-26`);
  await a.switchTo("/garden/main");
  ack.resolve(response({ ok: true })); await tick();
  const acknowledgements = a.server.calls.filter(c => c.path === "/v1/ack");
  assert.equal(acknowledgements.length, 1);
  assert.equal(acknowledgements[0].body.identity, OWNER);
});

test("offline opening provides a usable retry and clears loading after recovery", async t => {
  let offline = true;
  const a = await app(t, server => { server.intercept = call => {
    if (offline && call.path === "/v1/inbox" && !call.query.has("wait")) throw new TypeError("offline");
  }; });
  await until(() => !a.$("retry-inbox").hidden);
  assert.equal(a.$("inbox-status").hidden, true);
  assert.match(a.$("offline-banner").textContent, /Could not reach/);
  offline = false;
  a.$("retry-inbox").click();
  await until(() => a.$("retry-inbox").hidden && a.$("inbox-status").hidden);
  assert.equal(a.$("retry-inbox").hidden, true);
  assert.ok(a.$("threads").querySelector(".thread-row"));
});

test("sender name and information button open desktop options with nested-dialog focus and dismissal", async t => {
  const a = await app(t);
  await a.clickPeer();
  a.$("peer-name-btn").click();
  assert.equal(a.$("peer-info-sheet").hidden, false);
  assert.equal(a.$("peer-name-btn").getAttribute("aria-expanded"), "true");
  assert.equal(a.$("peer-info-title").textContent, "/alp");
  assert.equal(a.$("peer-known").checked, true);
  assert.equal(a.$("peer-full").checked, false);
  assert.equal(a.$("peer-requests").disabled, false);
  assert.equal(a.$("peer-own-mailbox").hidden, true);
  assert.equal(a.w.document.activeElement.id, "peer-info-close");
  assert.equal(a.$("app").inert, true);
  a.$("peer-requests").click();
  assert.equal(a.$("contact-sheet").hidden, false);
  assert.equal(a.$("contact-peer").value, PEER);
  assert.equal(a.$("contact-peer").disabled, true);
  assert.equal(a.$("peer-info-sheet").inert, true);
  a.w.document.dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
  assert.equal(a.$("contact-sheet").hidden, true);
  assert.equal(a.$("peer-info-sheet").hidden, false);
  assert.equal(a.w.document.activeElement.id, "peer-requests");
  a.$("peer-info-done").click();
  assert.equal(a.w.document.activeElement.id, "peer-name-btn");
  assert.equal(a.$("app").inert, false);
  assert.equal(a.$("peer-name-btn").getAttribute("aria-expanded"), "false");
  a.$("peer-info-btn").click();
  a.$("peer-info-sheet").dispatchEvent(new a.w.MouseEvent("mousedown", { bubbles: true }));
  assert.equal(a.$("peer-info-sheet").hidden, true);
  assert.equal(a.w.document.activeElement.id, "peer-info-btn");
});

test("Full permissions uses the server vocabulary, then revocation returns to review with no grants", async t => {
  const a = await app(t);
  await a.clickPeer(); a.$("peer-name-btn").click();
  a.$("peer-full").click();
  await until(() => a.$("peer-full").checked && !a.$("peer-full").disabled);
  const put = a.server.calls.find(c => c.method === "PUT" && c.path === "/v1/contacts");
  assert.deepEqual(put.body, { peer: PEER, alias: null, admission: "allow", autonomy: "auto",
    allowed_verbs: a.server.vocabulary.grantable, identity: OWNER });
  assert.ok(put.body.allowed_verbs.every(v => !a.server.vocabulary.never_auto.includes(v)));
  a.$("peer-full").click();
  await until(() => !a.$("peer-full").disabled);
  const row = a.server.mailboxes[OWNER].contacts[0];
  assert.equal(row.autonomy, "review"); assert.deepEqual(row.allowed_verbs, []);
  assert.equal(a.$("peer-full").checked, false);
});

test("Known sender and full grants apply to one sender without rewriting their namespace rule", async t => {
  const rule = { peer: "/alp/*", alias: "Alp's fleet", admission: "allow", autonomy: "auto", allowed_verbs: ["read_file"] };
  const a = await app(t, s => { s.mailboxes[OWNER].contacts = [{ ...rule }]; });
  await a.clickPeer(); a.$("peer-name-btn").click();
  assert.equal(a.$("peer-known").checked, false);
  assert.equal(a.$("peer-requests").disabled, true);
  assert.equal(a.$("peer-full").disabled, false);
  assert.match(a.$("peer-trust-note").textContent, /Covered by \/alp\/\*/);
  a.$("peer-known").click();
  await until(() => !a.$("peer-known").disabled && a.$("peer-known").checked);
  assert.deepEqual(a.server.mailboxes[OWNER].contacts.find(c => c.peer === PEER), { ...rule, peer: PEER });
  a.$("peer-known").click();
  await until(() => !a.$("peer-known").disabled);
  assert.equal(a.$("peer-known").checked, false);
  assert.deepEqual(a.server.mailboxes[OWNER].contacts, [rule]);
  a.$("peer-full").click();
  await until(() => !a.$("peer-full").disabled);
  assert.equal(a.$("peer-known").checked, true);
  assert.deepEqual(a.server.mailboxes[OWNER].contacts.find(c => c.peer === "/alp/*"), rule);
  assert.ok(a.server.calls.filter(c => c.method !== "GET" && c.path === "/v1/contacts").every(c => c.body.peer === PEER));
});

test("block requires confirmation, reports failures, retries and unblocks without restoring automatic grants", async t => {
  const a = await app(t);
  await a.clickPeer(); a.$("peer-name-btn").click();
  a.$("peer-block").click(); a.$("peer-block-cancel").click();
  assert.equal(a.server.calls.filter(c => c.method === "PUT").length, 0);
  let fail = true;
  a.server.intercept = c => c.path === "/v1/contacts" && c.method === "PUT" && fail ? response({ error: "unavailable" }, 503) : undefined;
  a.$("peer-block").click(); a.$("peer-block-confirm").click();
  await until(() => !a.$("peer-block-error").hidden);
  assert.equal(a.$("peer-block-sheet").hidden, false);
  assert.equal(a.$("peer-block-confirm").disabled, false);
  assert.equal(a.server.mailboxes[OWNER].contacts[0].admission, "allow");
  fail = false; a.$("peer-block-confirm").click();
  await until(() => a.$("peer-block-sheet").hidden);
  assert.equal(a.w.document.activeElement.id, "peer-block");
  assert.equal(a.$("peer-block").textContent, "Unblock this sender");
  assert.equal(a.$("peer-full").disabled, true);
  assert.equal(a.server.mailboxes[OWNER].contacts[0].admission, "block");
  a.$("peer-block").click();
  await until(() => a.$("peer-block").textContent === "Block this sender");
  assert.deepEqual(a.server.mailboxes[OWNER].contacts[0], { peer: PEER, alias: null, admission: "allow", autonomy: "review", allowed_verbs: [] });
});

test("failed permission save preserves the previous policy and can be retried", async t => {
  const a = await app(t);
  await a.clickPeer(); a.$("peer-name-btn").click();
  a.server.intercept = c => c.path === "/v1/contacts" && c.method === "PUT" ? response({ error: "unavailable" }, 503) : undefined;
  a.$("peer-full").click();
  await until(() => !a.$("peer-action-error").hidden);
  assert.equal(a.$("peer-full").checked, false);
  assert.equal(a.$("peer-full").disabled, false);
  a.server.intercept = null; a.$("peer-full").click();
  await until(() => a.$("peer-full").checked && !a.$("peer-full").disabled);
  assert.equal(a.$("peer-action-error").hidden, true);
});

test("sender edits cannot adopt an old mailbox's pending response after switching away and back", async t => {
  const a = await app(t), late = deferred();
  await a.clickPeer(); a.$("peer-name-btn").click();
  a.server.intercept = c => c.path === "/v1/contacts" && c.method === "PUT" ? late.promise : undefined;
  a.$("peer-full").click(); a.$("peer-full").click();
  assert.equal(a.$("peer-action-status").hidden, false);
  assert.equal(a.server.calls.filter(c => c.method === "PUT").length, 1);
  a.$("peer-info-done").click();
  await a.switchTo("/garden/main");
  await a.switchTo("/bekir/main");
  await a.clickPeer(); a.$("peer-name-btn").click();
  late.resolve(response({ ok: true })); await tick();
  assert.equal(a.$("peer-full").checked, false);
  assert.equal(a.$("peer-full").disabled, false);
  assert.equal(a.$("peer-action-status").hidden, true);
});

test("own mailboxes retain trust actions and can be opened from sender options", async t => {
  const a = await app(t, s => {
    s.mailboxes[OWNER].messages.forEach(m => { m.peer = SECOND; m.peer_handle = "/garden/main"; });
    s.mailboxes[OWNER].threads.forEach(thread => { thread.peer = "/garden/main"; });
  });
  await a.clickPeer(); a.$("peer-name-btn").click();
  assert.equal(a.$("peer-own-mailbox").hidden, false);
  assert.equal(a.$("peer-known").disabled, false);
  assert.match(a.$("peer-actions-note").textContent, /Blocking one of your own agents/);
  a.$("peer-known").click(); await until(() => !a.$("peer-known").disabled);
  assert.equal(a.server.mailboxes[OWNER].contacts.find(c => c.peer === "/garden/main").autonomy, "review");
  a.$("peer-open-mailbox").click();
  await until(() => a.$("identity-btn").dataset.address === "/garden/main");
  assert.equal(a.$("peer-info-sheet").hidden, true);
  assert.equal(a.$("app").inert, false);
});

test("archive and restore from sender options keep messages and close the dialog", async t => {
  const a = await app(t);
  await a.clickPeer(); a.$("peer-name-btn").click();
  a.$("peer-archive").click(); await tick();
  assert.deepEqual(a.server.mailboxes[OWNER].archived, [PEER]);
  assert.equal(a.$("peer-info-sheet").hidden, true);
  a.$("settings-btn").click(); a.$("open-archive").click();
  await a.clickPeer(); a.$("peer-name-btn").click();
  assert.equal(a.$("peer-archive").textContent, "Move back to the inbox");
  a.$("peer-archive").click(); await tick();
  assert.deepEqual(a.server.mailboxes[OWNER].archived, []);
  assert.equal(a.server.mailboxes[OWNER].messages.length, 35);
});

test("unavailable sender settings can be retried before any permission change", async t => {
  const a = await app(t, s => { s.intercept = c => c.path === "/v1/contacts" ? response({ error: "unavailable" }, 503) : undefined; });
  await a.clickPeer(); a.$("peer-name-btn").click();
  assert.equal(a.$("peer-known").disabled, true);
  assert.equal(a.$("peer-retry").hidden, false);
  a.server.intercept = null; a.$("peer-retry").click();
  await until(() => a.$("peer-retry").hidden);
  assert.equal(a.$("peer-known").disabled, false);
  assert.equal(a.$("peer-known").checked, true);
});
