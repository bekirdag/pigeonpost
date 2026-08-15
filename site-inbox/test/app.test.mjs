// Drive the inbox app in jsdom against fixture data copied from the postbox's real response shapes
// (`do_inbox`, `do_list_identities`, `whoami`, `do_list_contacts`).
//
// Deliberately NOT wired into CI. Everything else in this repo tests with node builtins only and
// carries no package.json, and this app is not worth a dependency in that pipeline. Run it by hand
// when changing app.js:
//
//     npm i jsdom && node site-inbox/test/app.test.mjs
//
// It asserts the things that are easy to break and expensive to notice: which threads exist and in
// what order, that a scoped request renders as a request rather than as JSON, that opening a thread
// acknowledges its unread mail, that a send is addressed and attributed correctly, and that a
// hostile message body stays inert text.
import { JSDOM, VirtualConsole } from "jsdom";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";

const APP = new URL("..", import.meta.url).pathname;
const now = Math.floor(Date.now() / 1000);

// Shapes copied from do_inbox / do_list_identities / whoami / do_list_contacts.
const INBOX = {
  messages: [
    {
      message_id: "m1", from: "/k/aaaa1111bbbb2222cccc3333dd", body: "the build is green",
      sender_score: 3, sender_standing: "unproven", sender_tier: "handle", untrusted: true,
      sender_known: true, alias: null, matched_contact: "/bekir/*",
      sender_handle: "/bekir/agent1", autonomy: "review", verb: null,
      held_because: "not_a_request", received_at: now - 7200, read: true,
    },
    {
      message_id: "m2", from: "/k/aaaa1111bbbb2222cccc3333dd",
      body: JSON.stringify({ v: 1, verb: "run_tests", args: { suite: "unit" }, note: "before the tag" }),
      sender_score: 3, sender_standing: "unproven", sender_tier: "handle", untrusted: true,
      sender_known: true, alias: null, matched_contact: "/bekir/*",
      sender_handle: "/bekir/agent1", autonomy: "review", verb: "run_tests",
      held_because: "verb_denied", received_at: now - 600, read: false,
    },
    {
      message_id: "m3", from: "/k/eeee5555ffff6666gggg7777hh", body: "hello from a stranger",
      sender_score: 0, sender_standing: "unproven", sender_tier: "anonymous", untrusted: true,
      sender_known: false, alias: null, matched_contact: null,
      sender_handle: null, autonomy: "review", verb: null,
      held_because: "sender_not_auto", received_at: now - 60, read: false,
    },
  ],
  policy: { accept_all: true, auto_accept_known: false },
};

const CONTACTS = {
  contacts: [{ peer: "/bekir/*", alias: "my fleet", admission: "allow", autonomy: "review", allowed_verbs: [] }],
  policy: { accept_all: true, auto_accept_known: false },
};

const calls = [];
function fakeFetch(url, opts = {}) {
  const path = String(url).replace("https://postbox.pigeonpost.dev", "");
  calls.push({ path, method: opts.method || "GET", body: opts.body ? JSON.parse(opts.body) : null });
  const json = (body, ok = true, status = 200) =>
    Promise.resolve({ ok, status, json: () => Promise.resolve(body) });

  // A namespace owner's fleet: the acting mailbox, a named sub-agent, and one never named.
  if (path.startsWith("/v1/identities")) {
    return json({ identities: [
      { address: "/k/cz6900v2h90vnwefj7g7ezvbh4", label: "su_iam" },
      { address: "/k/zz1111v2h90vnwefj7g7ezvbh9", label: "docdex box" },
      { address: "/k/qq2222v2h90vnwefj7g7ezvbh7", label: "scratch" },
    ] });
  }
  if (path.startsWith("/v1/whoami")) {
    const address = decodeURIComponent(path.split("identity=")[1] || "");
    const handles = { "/k/cz": "/bekir/su_iam", "/k/zz": "/bekir/docdex" };
    return json({ address, handle: handles[address.slice(0, 5)] || null });
  }
  if (path.startsWith("/v1/inbox")) {
    if (path.includes("wait=")) return new Promise(() => {}); // long poll: never resolves in the test
    return json(INBOX);
  }
  if (path.startsWith("/v1/contacts")) return json(CONTACTS);
  if (path.startsWith("/v1/ack")) return json({ ok: true });
  if (path.startsWith("/v1/send")) return json({ message_id: "sent1" }, true, 201);
  return json({ error: "not_found" }, false, 404);
}

const virtualConsole = new VirtualConsole();
const errors = [];
virtualConsole.on("jsdomError", (e) => errors.push(e.message));
virtualConsole.on("error", (...a) => errors.push(a.join(" ")));

const dom = new JSDOM(readFileSync(`${APP}/index.html`, "utf8"), {
  url: "https://inbox.pigeonpost.dev/",
  runScripts: "outside-only",
  pretendToBeVisual: true,
  virtualConsole,
});
const { window } = dom;

Object.defineProperty(window, "crypto", { value: webcrypto, configurable: true });
window.fetch = fakeFetch;
window.matchMedia = (q) => ({
  matches: /max-width/.test(q) ? window.innerWidth <= 780 : window.innerWidth > 780,
  addEventListener() {}, removeEventListener() {},
});
window.localStorage.setItem("ppi_token", "eyJfake.token.here");

window.eval(readFileSync(`${APP}/config.js`, "utf8"));
window.eval(readFileSync(`${APP}/app.js`, "utf8"));

const settle = (ms = 60) => new Promise((r) => setTimeout(r, ms));
const $ = (id) => window.document.getElementById(id);
const text = (el) => (el ? el.textContent.trim() : "<missing>");

let failures = 0;
function check(label, actual, expected) {
  const ok = String(actual) === String(expected);
  if (!ok) failures++;
  console.log(`${ok ? "  ok  " : "  FAIL"}  ${label}${ok ? "" : `\n          expected: ${expected}\n          actual:   ${actual}`}`);
}

await settle(150);

console.log("\n— signed in —");
check("signin hidden", $("signin").hidden, true);
check("app shown", $("app").hidden, false);
check("mailbox name", text($("me-name")), "su_iam");
check("mailbox handle", text($("me-sub")), "/bekir/su_iam");
check("identity switcher enabled (2 mailboxes)", $("identity-btn").disabled, false);

console.log("\n— your agents are listed first —");
const groups = [...$("threads").children];
check("first row is the agents heading", text(groups[0]), "Your agents");
const agentRows = [];
const otherRows = [];
let bucket = agentRows;
for (const el of groups.slice(1)) {
  if (el.classList.contains("group-head")) { bucket = otherRows; continue; }
  bucket.push(el.querySelector(".thread-row"));
}
const agentNames = agentRows.map((r) => text(r.querySelector(".tr-name")));
// The acting mailbox is not in its own list — you cannot write to yourself.
check("both sub-agents listed, acting mailbox excluded", agentRows.length, 2);
check("named agent uses its handle", agentNames.includes("docdex"), true);
check("unnamed agent falls back to its label", agentNames.includes("scratch"), true);
check("silent agent shows its address, not 'no messages'", text(agentRows.find((r) => text(r.querySelector(".tr-name")) === "docdex").querySelector(".tr-preview")), "/bekir/docdex");
check("unnamed agent is warned about", text(agentRows.find((r) => text(r.querySelector(".tr-name")) === "scratch").querySelector(".tr-preview")), "No handle — fleet trust will not match it");

console.log("\n— conversations below —");
const names = otherRows.map((r) => text(r.querySelector(".tr-name")));
// A key address is shown as a key address — truncated, but still recognisably /k/.
check("newest conversation first (stranger)", names[0], "/k/eeee5555f…");
check("named peer uses its contact alias", names[1], "my fleet");
check("unread badge on stranger", text(otherRows[0].querySelector(".dot")), "1");
check("held badge on fleet thread", text(otherRows[1].querySelector(".pill-held")), "held");
check("request preview reads as a request", text(otherRows[1].querySelector(".tr-preview")), "asks to run tests");

console.log("\n— open a thread —");
otherRows[1].click();
await settle(80);
check("thread header name", text($("peer-name")), "my fleet");
check("thread header address", text($("peer-sub")), "/bekir/agent1");
check("composer shown", $("composer").hidden, false);
const bubbles = [...$("messages").querySelectorAll(".bubble")];
check("two messages rendered", bubbles.length, 2);
check("plain body is text", text(bubbles[0].querySelector(".text")), "the build is green");
check("request renders its verb", text(bubbles[1].querySelector(".verb")), "run_tests");
check("request renders args", bubbles[1].querySelector(".args").textContent.includes('"suite": "unit"'), true);
check("request renders the note", text(bubbles[1].querySelector(".why")), "before the tag");
check("held reason is spelled out", text(bubbles[1].querySelector(".reason")), "that verb was not granted to this sender");
check("acked the unread message", calls.some((c) => c.path.startsWith("/v1/ack") && c.body.message_id === "m2"), true);

console.log("\n— body is never markup —");
const injected = "<img src=x onerror=alert(1)>";
INBOX.messages.push({
  message_id: "m4", from: "/k/aaaa1111bbbb2222cccc3333dd", body: injected,
  sender_handle: "/bekir/agent1", autonomy: "review", verb: null, held_because: "not_a_request",
  received_at: now - 10, read: true, sender_known: true, matched_contact: "/bekir/*",
  sender_standing: "unproven", sender_tier: "handle", alias: null, untrusted: true,
});
// The visibility handler refetches, which is the cheapest way to push new fixture mail through the
// real code path rather than reaching into app state.
window.document.dispatchEvent(new window.Event("visibilitychange"));
await settle(120);
const injectedBubble = [...$("messages").querySelectorAll(".text")].find((el) => el.textContent.includes("onerror"));
check("hostile body reached the view", Boolean(injectedBubble), true);
check("hostile body is inert text", injectedBubble && injectedBubble.textContent, injected);
check("no element was created from it", $("messages").querySelectorAll("img").length, 0);

console.log("\n— sending —");
const compose = $("compose");
compose.value = "on it";
compose.dispatchEvent(new window.Event("input"));
$("composer").dispatchEvent(new window.Event("submit", { cancelable: true, bubbles: true }));
await settle(120);
const sendCall = calls.find((c) => c.path === "/v1/send");
check("send called", Boolean(sendCall), true);
check("send addressed to the peer", sendCall && sendCall.body.to, "/bekir/agent1");
check("send carries the body", sendCall && sendCall.body.body, "on it");
check("send names the sending mailbox", sendCall && sendCall.body.from, "/k/cz6900v2h90vnwefj7g7ezvbh4");
const mine = [...$("messages").querySelectorAll("li.mine .text")];
check("outbound rendered in the thread", text(mine[mine.length - 1]), "on it");
const stored = JSON.parse(window.localStorage.getItem("ppi_sent:/k/cz6900v2h90vnwefj7g7ezvbh4") || "[]");
check("outbound persisted locally", stored.length, 1);
check("outbound recorded as sent", stored[0].status, "sent");

console.log("\n— peer normalisation —");
// Two sub-agents plus the two conversations: replying did not fork a third conversation.
check("sent to the handle, no new thread", [...$("threads").querySelectorAll(".thread-row")].length, 4);

console.log("\n— chatting with your own agent —");
const docdexRow = [...$("threads").querySelectorAll(".thread-row")]
  .find((r) => text(r.querySelector(".tr-name")) === "docdex");
docdexRow.click();
await settle(60);
check("thread opens on the agent", text($("peer-name")), "docdex");
check("header says it is yours", text($("peer-sub")), "/bekir/docdex · your mailbox");
$("peer-info-btn").click();
await settle(30);
check("offers to open that mailbox", text($("peer-info").querySelector(".open-mailbox")), "Open this mailbox");
check("explains which direction you are in", $("peer-info").querySelector(".note").textContent.includes("writing to this agent from /bekir/su_iam"), true);

compose.value = "status?";
compose.dispatchEvent(new window.Event("input"));
$("composer").dispatchEvent(new window.Event("submit", { cancelable: true, bubbles: true }));
await settle(120);
const toAgent = calls.filter((c) => c.path === "/v1/send").pop();
check("addressed to the agent's handle", toAgent.body.to, "/bekir/docdex");
check("sent from the acting mailbox", toAgent.body.from, "/k/cz6900v2h90vnwefj7g7ezvbh4");

console.log("\n— switching to that mailbox —");
$("peer-info").querySelector(".open-mailbox").click();
await settle(150);
check("now acting as the agent", text($("me-sub")), "/bekir/docdex");
check("its own row is gone from the agent list", [...$("threads").querySelectorAll(".tr-name")].every((n) => text(n) !== "docdex"), true);
check("the previous mailbox is now an agent row", [...$("threads").querySelectorAll(".tr-name")].some((n) => text(n) === "su_iam"), true);

console.log("\n— back closes the thread —");
$("back-btn").click();
await settle(40);
check("thread closed", $("composer").hidden, true);

console.log(`\n${errors.length} script error(s)`);
errors.forEach((e) => console.log("  " + e.split("\n")[0]));

console.log(failures === 0 && errors.length === 0 ? "\nPASS" : `\n${failures} failure(s)`);
process.exit(failures === 0 && errors.length === 0 ? 0 : 1);
