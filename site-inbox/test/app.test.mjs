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
      sender_handle: "/bekir/agent1", peer: "/bekir/agent1", peer_handle: "/bekir/agent1", thread_id: "t-agent1",
      direction: "in", autonomy: "review", verb: null,
      held_because: "not_a_request", received_at: now - 7200, read: true,
    },
    {
      message_id: "m2", from: "/k/aaaa1111bbbb2222cccc3333dd",
      body: JSON.stringify({ v: 1, verb: "run_tests", args: { suite: "unit" }, note: "before the tag" }),
      sender_score: 3, sender_standing: "unproven", sender_tier: "handle", untrusted: true,
      sender_known: true, alias: null, matched_contact: "/bekir/*",
      sender_handle: "/bekir/agent1", peer: "/bekir/agent1", peer_handle: "/bekir/agent1", thread_id: "t-agent1",
      direction: "in", autonomy: "review", verb: "run_tests",
      held_because: "verb_denied", received_at: now - 600, read: false,
    },
    {
      message_id: "m3", from: "/k/eeee5555ffff6666gggg7777hh", body: "hello from a stranger",
      sender_score: 0, sender_standing: "unproven", sender_tier: "anonymous", untrusted: true,
      sender_known: false, alias: null, matched_contact: null,
      sender_handle: null, peer: "/k/eeee5555ffff6666gggg7777hh", peer_handle: null, thread_id: "t-stranger",
      direction: "in", autonomy: "review", verb: null,
      held_because: "sender_not_auto", received_at: now - 60, read: false,
    },
    // An own agent that *has* corresponded. It belongs in the list; `scratch`, which never has,
    // does not — that contrast is the point of the two fixtures.
    {
      message_id: "m_docdex", from: "/k/zz1111v2h90vnwefj7g7ezvbh9", body: "index rebuilt",
      sender_score: 3, sender_standing: "unproven", sender_tier: "handle", untrusted: true,
      sender_known: true, alias: null, matched_contact: "/bekir/*",
      sender_handle: "/bekir/docdex", peer: "/bekir/docdex", peer_handle: "/bekir/docdex",
      direction: "in", autonomy: "review", verb: null,
      held_because: "not_a_request", received_at: now - 10800, read: true,
    },
    // The sender's own copy, which is what makes a thread a conversation.
    {
      message_id: "m_out1", direction: "out", from: "/k/cz6900v2h90vnwefj7g7ezvbh4",
      to: "/bekir/agent1", peer: "/bekir/agent1", peer_handle: "/bekir/agent1", thread_id: "t-agent1",
      body: "on it, running now", untrusted: false, autonomy: null,
      sent_at: now - 3600, received_at: now - 3600, read: true,
    },
  ],
  policy: { accept_all: true, auto_accept_known: false },
};

const CONTACTS = {
  contacts: [{ peer: "/bekir/*", alias: "my fleet", admission: "allow", autonomy: "review", allowed_verbs: [] }],
  policy: { accept_all: true, auto_accept_known: false },
  vocabulary: {
    grantable: ["report_status", "answer_question", "read_file", "run_tests"],
    never_auto: ["git_push", "deploy", "read_credentials", "spend", "delete_files", "run_shell"],
  },
};

// Two conversations with one peer, one of them opened and never written in — which is the case
// only the server knows about, since it has no messages to be inferred from.
const THREADS = [
  { thread_id: "t-agent1", peer: "/bekir/agent1", title: null, is_default: true,
    created_at: now - 9000, last_at: now - 600, archived: false },
  { thread_id: "t-agent1-deploy", peer: "/bekir/agent1", title: "deploy", is_default: false,
    created_at: now - 8000, last_at: now - 8000, archived: false },
  { thread_id: "t-stranger", peer: "/k/eeee5555ffff6666gggg7777hh", title: null, is_default: true,
    created_at: now - 60, last_at: now - 60, archived: false },
];

const ARCHIVED = new Set();
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
    // axum + serde deserialize a bool from `true`/`false` only. `include_sent=1` is a 400 before the
    // request is even authenticated — which is exactly how this shipped broken once, because the
    // fixture used to accept any query string.
    const flag = new URL("https://x" + path).searchParams.get("include_sent");
    if (flag !== null && flag !== "true" && flag !== "false") {
      return json({ error: "bad_request", detail: `Failed to deserialize query string: include_sent: provided string was not \`true\` or \`false\`` }, false, 400);
    }
    if (path.includes("wait=")) return new Promise(() => {}); // long poll: never resolves in the test
    return json(INBOX);
  }
  if (path.startsWith("/v1/threads")) {
    if ((opts.method || "GET") === "POST") {
      const b = JSON.parse(opts.body);
      THREADS.push({ thread_id: "t-new", peer: b.peer, title: b.title, is_default: false,
                     created_at: now, last_at: now, archived: false });
      return json({ thread_id: "t-new" }, true, 201);
    }
    return json({ threads: THREADS });
  }
  if (path.startsWith("/v1/contacts")) return json(CONTACTS);
  if (path.startsWith("/v1/archive")) {
    if ((opts.method || "GET") === "GET") return json({ archived: [...ARCHIVED] });
    const b = JSON.parse(opts.body);
    if (b.archived) ARCHIVED.add(b.peer); else ARCHIVED.delete(b.peer);
    return json({ ok: true });
  }
  if (path.startsWith("/v1/ack")) return json({ ok: true });
  if (path.startsWith("/v1/send")) return json({ message_id: "sent1", sent_copy_id: "copy1" }, true, 201);
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

console.log("\n— the list is correspondence, not a directory —");
const groups = [...$("threads").children];
const agentRows = [];
const otherRows = [];
let bucket = otherRows;
for (const el of groups) {
  if (el.classList.contains("group-head")) {
    bucket = text(el) === "Your agents" ? agentRows : otherRows;
    continue;
  }
  bucket.push(el.querySelector(".thread-row"));
}
const allNames = [...agentRows, ...otherRows].map((r) => text(r.querySelector(".tr-name")));
// `docdex` and `scratch` are on the account and have never exchanged a message. A fleet of a dozen
// such agents would bury the conversations that are real, so they belong in the identity picker
// rather than in this list.
check("an own agent you have corresponded with is listed", allNames.includes("docdex"), true);
check("a silent own agent is not", allNames.includes("scratch"), false);
check("every mailbox is still reachable from the picker", $("identity-btn").disabled, false);

console.log("\n— conversations —");
const names = otherRows.map((r) => text(r.querySelector(".tr-name")));
// A key address is shown as a key address — truncated, but still recognisably /k/.
check("newest conversation first (stranger)", names[0], "/k/eeee5555f…");
check("named peer uses its contact alias", names[1], "my fleet");
check("unread badge on stranger", text(otherRows[0].querySelector(".dot")), "1");
check("held badge on fleet thread", text(otherRows[1].querySelector(".pill-held")), "held");
check("request preview reads as a request", text(otherRows[1].querySelector(".tr-preview")), "asks to run tests");
check("the conversation was requested, not just the inbox", calls.some((c) => c.path.includes("include_sent=true")), true);

console.log("\n— open a thread —");
otherRows[1].click();
await settle(80);
check("thread header name", text($("peer-name")), "my fleet");
check("thread header address", text($("peer-sub")), "/bekir/agent1");
check("composer shown", $("composer").hidden, false);
const bubbles = [...$("messages").querySelectorAll(".bubble")];
check("both sides of the conversation rendered", bubbles.length, 3);
check("plain body is text", text(bubbles[0].querySelector(".text")), "the build is green");
// The server's own copy of what this mailbox sent, in the middle of the thread by time.
const mineRows = [...$("messages").querySelectorAll("li.mine")];
check("the sent message is on my side", mineRows.length, 1);
check("the sent message reads back", text(mineRows[0].querySelector(".text")), "on it, running now");
check("request renders its verb", text(bubbles[2].querySelector(".verb")), "run_tests");
check("request renders args", bubbles[2].querySelector(".args").textContent.includes('"suite": "unit"'), true);
check("request renders the note", text(bubbles[2].querySelector(".why")), "before the tag");
check("held reason is spelled out", text(bubbles[2].querySelector(".reason")), "that verb was not granted to this sender");
check("acked the unread message", calls.some((c) => c.path.startsWith("/v1/ack") && c.body.message_id === "m2"), true);

console.log("\n— body is never markup —");
const injected = "<img src=x onerror=alert(1)>";
INBOX.messages.push({
  message_id: "m4", from: "/k/aaaa1111bbbb2222cccc3333dd", body: injected,
  peer: "/bekir/agent1", thread_id: "t-agent1", sender_handle: "/bekir/agent1", autonomy: "review", verb: null, held_because: "not_a_request",
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
// The composer asks for work by default now, so the body on the wire is a request envelope and
// the typed words survive inside it.
const sentEnvelope = JSON.parse(sendCall.body.body);
check("send carries the words", sentEnvelope.note, "on it");
check("send asks for something", sentEnvelope.verb, "make_change");
check("send names the sending mailbox", sendCall && sendCall.body.from, "/k/cz6900v2h90vnwefj7g7ezvbh4");
// It appears straight away, rendered as the request it is rather than as raw JSON — a sent
// request is still a request.
const mineReq = [...$("messages").querySelectorAll("li.mine .request .why")];
check("outbound appears immediately", mineReq.some((el) => text(el) === "on it"), true);
check("and reads as a request, not as JSON", [...$("messages").querySelectorAll("li.mine .verb")].some((el) => text(el) === "make_change"), true);
// Nothing is written to the browser any more — the postbox keeps the sent copy.
check("no per-device history is kept", window.localStorage.getItem("ppi_sent:/k/cz6900v2h90vnwefj7g7ezvbh4"), "null");

console.log("\n— peer normalisation —");
// The two conversations, and still two: replying to `/bekir/agent1` by handle must land in the
// thread its `/k/` messages already built, not fork a third.
check("sent to the handle, no new thread", [...$("threads").querySelectorAll(".thread-row")].length, 3);

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
// It is not promoted into the list just for being an own mailbox — the list stays correspondence.
// Switching back to it is the picker's job, and the picker still offers it.
check("the previous mailbox is reachable from the picker", $("identity-btn").disabled, false);

console.log("\n— back closes the thread —");
$("back-btn").click();
await settle(40);
check("thread closed", $("composer").hidden, true);

console.log("\n— phone: opening a thread must not raise the keyboard —");
// Narrow the window so the app's own media query reports a phone, then open a thread the way a
// tap does and check nothing took focus. On desktop the composer is focused; on a phone that is
// the keyboard covering the conversation you just opened.
window.innerWidth = 400;
window.document.activeElement.blur();
[...$("threads").querySelectorAll(".thread-row")][0].click();
await settle(80);
check("composer did not steal focus on a phone",
  window.document.activeElement === window.document.body ||
  window.document.activeElement.id !== "compose", true);

window.innerWidth = 1200;
[...$("threads").querySelectorAll(".thread-row")][0].click();
await settle(80);
check("composer is focused on desktop", window.document.activeElement.id, "compose");

console.log("\n— mobile layout —");
// jsdom does not lay out, so these assert the rules that fixed a real bug: a grid item defaults to
// min-width:auto, so a nowrap preview pushed the pane wider than the phone and cut off the
// timestamp and unread badge on the right.
console.log("\n— archiving a conversation —");
const beforeRows = [...$("threads").querySelectorAll(".thread-row")].length;
// Open the stranger's thread and file it away.
const strangerRow = [...$("threads").querySelectorAll(".thread-row")]
  .find((r) => text(r.querySelector(".tr-name")).startsWith("/k/eeee"));
strangerRow.click();
await settle(60);
$("archive-btn").click();
await settle(120);
check("the archive was told", calls.some((c) => c.path === "/v1/archive" && c.method === "PUT"), true);
check("it leaves the inbox", [...$("threads").querySelectorAll(".thread-row")].length, beforeRows - 1);
check("and the thread closes", $("composer").hidden, true);

console.log("\n— the archive view —");
$("settings-btn").click();
await settle(40);
check("settings opens", $("settings-sheet").hidden, false);
check("it counts what is filed", text($("archive-count")).includes("1"), true);
$("open-archive").click();
await settle(80);
check("settings closes behind it", $("settings-sheet").hidden, true);
check("the banner says where you are", $("archive-banner").hidden, false);
const archivedNames = [...$("threads").querySelectorAll(".tr-name")].map((n) => text(n));
check("only the filed conversation is here", archivedNames.length, 1);
check("and it is the right one", archivedNames[0].startsWith("/k/eeee"), true);
$("archive-exit").click();
await settle(80);
check("back to the inbox", $("archive-banner").hidden, true);
check("with the rest of the conversations", [...$("threads").querySelectorAll(".thread-row")].length, beforeRows - 1);

console.log("\n— starting a conversation with a new address —");
$("new-btn").click();
await settle(40);
check("the sheet opens", $("new-sheet").hidden, false);
$("new-send").click();
await settle(40);
check("it will not send to nobody", $("new-error").hidden, false);
$("new-peer").value = "/bekir/fresh";
$("new-body").value = "first contact";
$("new-send").click();
await settle(150);
const newSend = calls.filter((c) => c.path === "/v1/send").pop();
check("addressed as typed", newSend.body.to, "/bekir/fresh");
check("carrying the message", newSend.body.body, "first contact");
check("and the sheet closes", $("new-sheet").hidden, true);

console.log("\n— trusted senders —");
$("settings-btn").click();
await settle(40);
check("the fleet contact is listed", text($("contact-list")).includes("/bekir/*"), true);
$("contact-add").click();
await settle(40);
check("the editor opens", $("contact-sheet").hidden, false);
check("a new sender's address is editable", $("contact-peer").disabled, false);
$("contact-peer").value = "/bekir/newcomer";
$("contact-autonomy").value = "auto";
const runTests = [...$("contact-verbs").querySelectorAll("input")].find((i) => i.value === "run_tests");
runTests.checked = true;
$("contact-save").click();
await settle(150);
const put = calls.filter((c) => c.path === "/v1/contacts" && c.method === "PUT").pop();
check("the contact was written", put.body.peer, "/bekir/newcomer");
check("with the verb granted", put.body.allowed_verbs.includes("run_tests"), true);
check("never-auto verbs cannot be granted", [...$("contact-verbs").querySelectorAll("input:disabled")].length > 0, true);

console.log("\n— what a message asks for —");
// The sender asks for the most; the recipient decides. A plain message that no agent will ever act
// on is the wrong default for an app whose whole point is reaching agents.
check("defaults to asking for work", $("compose-verb").value, "make_change");

otherRows[1].click();
await settle(80);
const beforeAsk = calls.length;
compose.value = "fix the failing pipeline";
compose.dispatchEvent(new window.Event("input"));
$("composer").dispatchEvent(new window.Event("submit", { cancelable: true, bubbles: true }));
await settle(120);
const asked = calls.slice(beforeAsk).find((c) => c.path === "/v1/send");
const envelope = JSON.parse(asked.body.body);
check("sends a request envelope", envelope.v, 1);
check("with the default verb", envelope.verb, "make_change");
check("the typed text becomes the task", envelope.args.task, "fix the failing pipeline");
check("and survives as the note", envelope.note, "fix the failing pipeline");

// A verb that carries its request in a different argument.
$("compose-verb").value = "answer_question";
const beforeQ = calls.length;
compose.value = "is the build green?";
compose.dispatchEvent(new window.Event("input"));
$("composer").dispatchEvent(new window.Event("submit", { cancelable: true, bubbles: true }));
await settle(120);
const q = JSON.parse(calls.slice(beforeQ).find((c) => c.path === "/v1/send").body.body);
check("a question goes in args.question", q.args.question, "is the build green?");

// Prose is still possible, and is what talking to a person needs.
$("compose-verb").value = "";
const beforePlain = calls.length;
compose.value = "morning";
compose.dispatchEvent(new window.Event("input"));
$("composer").dispatchEvent(new window.Event("submit", { cancelable: true, bubbles: true }));
await settle(120);
check("plain stays plain", calls.slice(beforePlain).find((c) => c.path === "/v1/send").body.body, "morning");

// A push with nothing to push is refused here rather than held on arrival for a reason that
// reads like a permissions problem.
$("compose-verb").value = "deploy";
$("compose-branch").value = "";
const beforeNoBranch = calls.length;
compose.value = "ship it";
compose.dispatchEvent(new window.Event("input"));
$("composer").dispatchEvent(new window.Event("submit", { cancelable: true, bubbles: true }));
await settle(120);
check("a deploy with no branch is not sent", calls.slice(beforeNoBranch).some((c) => c.path === "/v1/send"), false);

$("compose-branch").value = "main";
const beforeBranch = calls.length;
compose.value = "ship it";
compose.dispatchEvent(new window.Event("input"));
$("composer").dispatchEvent(new window.Event("submit", { cancelable: true, bubbles: true }));
await settle(120);
const dep = JSON.parse(calls.slice(beforeBranch).find((c) => c.path === "/v1/send").body.body);
check("a named branch is carried", dep.args.branch, "main");
$("compose-verb").value = "make_change";

console.log("\n— threads within a peer —");
// One conversation still shows the pane, because the button that opens a second thread lives in
// it — hiding it until a second thread exists made that button unreachable.
otherRows[0].click();
await settle(80);
check("one conversation still shows the thread pane", $("pane-subs").hidden, false);
check("so the button to open another is reachable", $("subs-new").offsetParent !== undefined, true);
check("and the layout makes room", $("app").getAttribute("data-subs"), "true");
check("with the default thread listed", [...$("subs").querySelectorAll(".sub-row")].length, 1);

// The fleet agent has two: the default, and one opened with nothing said in it yet.
otherRows[1].click();
await settle(80);
check("two conversations reveal the thread pane", $("pane-subs").hidden, false);
check("and the layout makes room for it", $("app").getAttribute("data-subs"), "true");
check("the pane names the peer", text($("subs-peer")), "my fleet");

const subRows = [...$("subs").querySelectorAll(".sub-row")];
check("both conversations are listed", subRows.length, 2);
check("most recent first", text(subRows[0].querySelector(".sub-title")), "Default thread");
check("the unnamed one is marked as such", subRows[0].querySelector(".sub-title.is-default") !== null, true);
check("the named one keeps its name", text(subRows[1].querySelector(".sub-title")), "deploy");
check("an empty thread says so", text(subRows[1].querySelector(".sub-meta")), "no messages yet");
check("the open one is marked current", subRows[0].getAttribute("aria-current"), "true");

// Messages belong to the thread that is open, which is the whole point of having them.
check("the default thread shows its messages", $("messages").querySelectorAll(".bubble").length > 0, true);
subRows[1].click();
await settle(80);
check("an empty thread shows nothing", $("messages").querySelectorAll(".bubble").length, 0);
const afterPick = [...$("subs").querySelectorAll(".sub-row")];
check("selection moved", afterPick[1].getAttribute("aria-current"), "true");
check("and left the one before it", afterPick[0].getAttribute("aria-current"), null);

// And a reply lands where it was written, not in whichever conversation the peer last used.
const before = calls.length;
compose.value = "starting the deploy";
compose.dispatchEvent(new window.Event("input"));
$("composer").dispatchEvent(new window.Event("submit", { cancelable: true, bubbles: true }));
await settle(120);
const threadedSend = calls.slice(before).find((c) => c.path === "/v1/send");
check("the send names its thread", threadedSend && threadedSend.body.thread_id, "t-agent1-deploy");

console.log("\n— opening a thread —");
const beforeNew = calls.length;
$("subs-new").click();
await settle(60);
check("the dialog opens", $("thread-sheet").hidden, false);

// An empty name is refused in the dialog, not as a toast somewhere else, and opens nothing.
const beforeEmpty = calls.length;
$("thread-title-input").value = "   ";
$("thread-create").click();
await settle(60);
check("an unnamed thread is refused", $("thread-error").hidden, false);
check("and nothing was created", calls.slice(beforeEmpty).some((c) => c.path === "/v1/threads" && c.method === "POST"), false);
check("the dialog stays open to fix it", $("thread-sheet").hidden, false);

$("thread-title-input").value = "release notes";
$("thread-create").click();
await settle(120);
check("the dialog closes on success", $("thread-sheet").hidden, true);
const opened = calls.slice(beforeNew).find((c) => c.path === "/v1/threads" && c.method === "POST");
check("the thread was opened on the server", Boolean(opened), true);
check("with the title given", opened && opened.body.title, "release notes");
check("for the peer on screen", opened && opened.body.peer, "/bekir/agent1");
check("and it is listed", [...$("subs").querySelectorAll(".sub-title")].some((el) => text(el) === "release notes"), true);
// Named and selected in one step: opening a thread and then having to find it would be two.
const selected = [...$("subs").querySelectorAll(".sub-row")].find((r) => r.getAttribute("aria-current") === "true");
check("and selected", text(selected.querySelector(".sub-title")), "release notes");
// Cancelling must not open anything.
const beforeCancel = calls.length;
$("subs-new").click();
await settle(60);
$("thread-cancel").click();
await settle(80);
check("cancelling closes the dialog", $("thread-sheet").hidden, true);
check("cancelling opens nothing", calls.slice(beforeCancel).some((c) => c.path === "/v1/threads" && c.method === "POST"), false);
// And it is a real dialog: Escape closes it, which a window.prompt could never offer.
$("subs-new").click();
await settle(60);
check("reopened for the escape check", $("thread-sheet").hidden, false);
window.document.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
await settle(60);
check("escape closes it", $("thread-sheet").hidden, true);

console.log("\n— the phone walks out one screen at a time —");
const subsCss = readFileSync(`${APP}/app.css`, "utf8");
check("threads pane is a screen of its own on a phone", /@media \(max-width: 780px\)[\s\S]*\.pane-subs \{[\s\S]*position: fixed/.test(subsCss), true);
check("and sits under the messages screen", /\.pane-subs \{[\s\S]*z-index: 25/.test(subsCss), true);
check("parked threads pane is not focusable", /\.pane-subs \{[\s\S]*visibility: hidden/.test(subsCss), true);

console.log("\n— message zoom, not page zoom —");
const html = readFileSync(`${APP}/index.html`, "utf8");
check("pinch zoom is off", /user-scalable=no/.test(html), true);
check("the layout cannot be scaled", /maximum-scale=1/.test(html), true);

const css = readFileSync(`${APP}/app.css`, "utf8");
check("only the messages scale", /\.messages \{ font-size: calc\(15px \* var\(--msg-scale/.test(css), true);
// 16px inputs are what stop iOS zooming a page it would then refuse to unzoom.
check("form fields do not trigger an iOS zoom", /\.field input\[type="text"\][\s\S]{0,400}font-size:\s*16px/.test(css), true);

check("panes may shrink below their content", /\.pane-list\s*\{[^}]*min-width:\s*0/s.test(css), true);
check("thread pane may shrink too", /\.pane-thread\s*\{[^}]*min-width:\s*0/s.test(css), true);
check("phone column is minmax(0, 1fr)", css.includes("grid-template-columns: minmax(0, 1fr)"), true);
check("document never scrolls at all", /body\s*\{[^}]*overflow:\s*hidden/s.test(css), true);
check("both scrollers refuse sideways scroll", (css.match(/overflow-x:\s*hidden/g) || []).length >= 2, true);
// A single viewport unit is a single point of failure: a browser that does not know `dvh` drops the
// declaration, the shell loses its height, and the composer floats off the bottom of a grown page.
check("viewport height has a fallback chain", /height:\s*100vh;[\s\S]{0,200}height:\s*100svh;[\s\S]{0,200}height:\s*100dvh;/.test(css), true);
check("flex items may shrink below their longest word", /\.messages li \{[^}]*min-width:\s*0/s.test(css), true);
check("parked thread pane is not focusable", /\.pane-thread\s*\{[^}]*visibility:\s*hidden/s.test(css), true);

console.log(`\n${errors.length} script error(s)`);
errors.forEach((e) => console.log("  " + e.split("\n")[0]));

console.log(failures === 0 && errors.length === 0 ? "\nPASS" : `\n${failures} failure(s)`);
process.exit(failures === 0 && errors.length === 0 ? 0 : 1);
