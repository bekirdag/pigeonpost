// The design kit's inbox template (pigeonpost-design-kit templates/inbox.html), as the inbox draws it:
// the rail, the quick filters, runs of messages with the sender's face, the files list and the
// details pane docked beside the conversation from 1200 px.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";
import { fixtureServer, OWNER } from "./fixtures.mjs";

const read = name => readFileSync(new URL("../" + name, import.meta.url), "utf8");
const tick = (ms = 30) => new Promise(resolve => setTimeout(resolve, ms));
async function until(predicate) {
  for (let n = 0; n < 200; n++) { if (predicate()) return; await tick(10); }
  assert.fail("UI did not settle");
}

async function app(t, { width = 1024, configure = () => {} } = {}) {
  const server = fixtureServer();
  configure(server);
  const errors = [], console = new VirtualConsole();
  console.on("jsdomError", e => errors.push(e.message));
  const dom = new JSDOM(read("index.html"), { url: "https://inbox.pigeonpost.dev/", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: console });
  const w = dom.window, $ = id => w.document.getElementById(id);
  Object.defineProperty(w, "crypto", { value: webcrypto });
  Object.defineProperty(w, "innerWidth", { value: width, configurable: true, writable: true });
  w.matchMedia = query => ({ matches: query.includes("min-width"), addEventListener() {} });
  w.fetch = server.fetch;
  w.localStorage.setItem("ppi_token", "fixture_account");
  w.eval(read("names.js"));
  w.eval(read("config.js"));
  w.eval(read("app.js"));
  t.after(() => { w.close(); assert.deepEqual(errors, [], "No uncaught script errors"); });
  await until(() => $("identity-btn").dataset.address === "/bekir/main");
  await until(() => $("threads").querySelector(".thread-row"));
  const row = name => [...$("threads").querySelectorAll(".thread-row")].find(r => r.querySelector(".tr-name").textContent === name);
  return { w, $, server, row };
}

// A second peer with unread mail, so the filters have something to tell apart.
function withUnread(server) {
  server.mailboxes[OWNER].messages.push({ message_id: "unread-1", direction: "in", from: "/k/ada", peer: "/ada/main",
    peer_handle: "/ada/main", body: "Signed the offer letter.", received_at: 1789600100, read: false, autonomy: "review" });
}

test("the list column has the template's parts, and the stylesheet that draws them loads last", async t => {
  const a = await app(t);
  for (const id of ["new-btn", "signout-btn", "identity-btn", "search", "list-filters", "messages-btn", "contacts-btn", "settings-btn", "side-nav"]) {
    assert.ok(a.$(id), id);
  }
  assert.ok(a.$("rail").contains(a.$("settings-btn")) && a.$("rail").contains(a.$("side-nav")), "the views sit in the rail");
  assert.equal(a.$("messages-btn").getAttribute("aria-pressed"), "true");
  const html = read("index.html");
  const links = [...html.matchAll(/<link rel="stylesheet" href="([^"?]+)/g)].map(m => m[1]);
  assert.equal(links.at(-1), "inbox.css", "inbox.css comes after every other stylesheet");
  for (const page of ["m.html", "join.html", "pair.html", "native-call.html", "g/index.html"]) {
    assert.ok(!read(page).includes("inbox.css"), page + " keeps its own look");
  }
  const css = read("inbox.css");
  assert.ok(!/#[0-9a-f]{3,6}\b/i.test(css.replace(/%23|#(peer|archive|threads|retry)[\w-]*/gi, "")), "colours come from the kit's tokens");
});

test("quick filters narrow the list without changing it, and say so when nothing matches", async t => {
  const a = await app(t, { configure: withUnread });
  const names = () => [...a.$("threads").querySelectorAll(".tr-name")].map(n => n.textContent);
  assert.equal(names().length, 2);
  assert.equal(a.row("Ada").dataset.unread, "true", "an unread row is marked for the stylesheet");
  a.$("list-filters").querySelector('[data-filter="unread"]').click();
  assert.deepEqual(names(), ["Ada"]);
  assert.equal(a.$("list-filters").querySelector('[data-filter="unread"]').getAttribute("aria-pressed"), "true");
  assert.equal(a.$("pane-list").dataset.filter, "unread");
  a.$("list-filters").querySelector('[data-filter="held"]').click();
  assert.deepEqual(names(), []);
  assert.equal(a.$("threads-empty").hidden, false);
  assert.equal(a.$("threads-empty").textContent, "Nothing is held for review.");
  a.$("list-filters").querySelector('[data-filter="all"]').click();
  assert.equal(names().length, 2);
  assert.equal(a.server.calls.filter(c => c.method !== "GET").length, 0, "filtering writes nothing");
});

test("the rail goes back to Messages from Calls and opens Contacts in Settings", async t => {
  const a = await app(t);
  a.$("contacts-btn").click();
  assert.equal(a.$("settings-sheet").hidden, false);
  assert.equal(a.$("settings-title").textContent, "Contacts and permissions");
  a.$("settings-close").click();
  a.$("messages-btn").click();
  assert.equal(a.$("threads").hidden, false);
  assert.equal(a.$("list-filters").hidden, false);
});

test("a run of messages from one side is marked first, middle and last, with the face on received ones", async t => {
  const a = await app(t);
  a.$("threads").querySelector(".thread-row").click();
  await until(() => a.$("messages").querySelector("[data-message-id]"));
  const rows = [...a.$("messages").children].filter(li => li.dataset.messageId);
  for (const li of rows) {
    assert.equal(Boolean(li.querySelector(":scope > .msg-avatar")), !li.classList.contains("mine"), "faces only beside received messages");
    assert.equal(li.querySelector(".msg-stack").children[0].className, "bubble", "the bubble still leads its column");
  }
  // The fixture alternates in, out, in: every in/in pair is a run.
  const positions = rows.map(li => (li.classList.contains("mine") ? "m" : "t") + ":" + (li.dataset.pos || "-"));
  for (let i = 1; i < rows.length - 1; i++) {
    const [side] = positions[i].split(":");
    const before = positions[i - 1].startsWith(side), after = positions[i + 1].startsWith(side);
    const want = before && after ? "middle" : before ? "last" : after ? "first" : "-";
    assert.equal(positions[i].split(":")[1], want, "row " + i);
  }
});

test("from 1200 px the details pane docks beside the conversation and stays open between people", async t => {
  const a = await app(t, { width: 1440, configure: withUnread });
  a.row("Alp").click();
  await tick();
  a.$("peer-info-btn").click();
  const sheet = a.$("peer-info-sheet");
  assert.equal(sheet.hidden, false);
  assert.equal(sheet.dataset.docked, "true");
  assert.equal(a.$("app").dataset.details, "open");
  assert.notEqual(a.$("app").inert, true, "the conversation stays usable beside it");
  assert.equal(a.$("peer-info-btn").getAttribute("aria-expanded"), "true");
  assert.equal(a.w.localStorage.getItem("ppi_details"), "open");
  a.row("Ada").click();
  await tick();
  assert.equal(sheet.hidden, false, "still open for the next person");
  assert.equal(a.$("peer-info-title").textContent, "Ada");
  // A dialog over it makes it inert with the rest of the page.
  a.$("settings-btn").click();
  assert.equal(sheet.inert, true);
  a.$("settings-close").click();
  assert.equal(sheet.inert, false);
  // The info button closes it again and forgets the choice.
  a.$("peer-info-btn").click();
  assert.equal(sheet.hidden, true);
  assert.equal(sheet.dataset.docked, undefined);
  assert.equal(a.$("app").dataset.details, undefined);
  assert.equal(a.w.localStorage.getItem("ppi_details"), null);
});

test("below 1200 px the details stay a dialog", async t => {
  const a = await app(t, { width: 1024 });
  a.row("Alp").click();
  await tick();
  a.$("peer-info-btn").click();
  assert.equal(a.$("peer-info-sheet").dataset.docked, undefined);
  assert.equal(a.$("app").inert, true);
});

test("the details list the conversation's files, newest first, each going to its message", async t => {
  const a = await app(t, { configure: server => {
    const m = server.mailboxes[OWNER].messages;
    m[30].attachments = [{ id: "f-old", filename: "old.pdf", media_type: "application/pdf", bytes: 2048 }];
    m[33].attachments = [{ id: "f-new", filename: "new.png", media_type: "image/png", bytes: 4096 }];
  } });
  a.row("Alp").click();
  await tick();
  a.$("peer-info-btn").click();
  assert.equal(a.$("peer-files-section").hidden, false);
  assert.equal(a.$("peer-files-title").textContent, "Files · 2");
  assert.deepEqual([...a.$("peer-files").querySelectorAll(".peer-file-name")].map(n => n.textContent), ["new.png", "old.pdf"]);
});

// Owner, 2026-11-18: "I like the all, unread, agents, people filtration." People is exactly the
// threads Agents is not, so the two together are All.
test("People shows exactly the threads Agents does not, in the kit's order", async t => {
  const a = await app(t, { configure: server => {
    server.mailboxes[OWNER].messages.push({ message_id: "agent-1", direction: "in", from: "/garden/main", peer: "/garden/main", peer_handle: "/garden/main", body: "Build is green.", received_at: 1789600200, read: true, autonomy: "review" });
  }});
  const buttons = [...a.$("list-filters").querySelectorAll("[data-filter]")];
  assert.deepEqual(buttons.map(b => b.dataset.filter), ["all", "unread", "agents", "people", "held"]);
  assert.ok(buttons.find(b => b.dataset.filter === "people").querySelector("svg.lu"), "people button contains an svg.lu");
  const count = () => a.$("threads").querySelectorAll(".thread-row").length;
  const all = count();
  assert.equal(all, 2);
  buttons.find(b => b.dataset.filter === "agents").click();
  const agents = count();
  buttons.find(b => b.dataset.filter === "people").click();
  const people = count();
  assert.ok(agents > 0, "agents filter shows some threads");
  assert.ok(people > 0, "people filter shows some threads");
  assert.equal(agents + people, all, "agents and people partitions cover all threads");
  assert.equal(buttons.find(b => b.dataset.filter === "people").getAttribute("aria-pressed"), "true", "people button is pressed");
  assert.equal(a.$("pane-list").dataset.filter, "people", "pane-list reflects people filter");
});

test("People says so when every conversation is with an agent", async t => {
  const a = await app(t, { configure: server => {
    const box = server.mailboxes[OWNER];
    for (const m of box.messages) { m.peer = m.peer_handle = "/garden/main"; }
    for (const th of box.threads) th.peer = "/garden/main";
    box.contacts = [];
  }});
  a.$("list-filters").querySelector('[data-filter="people"]').click();
  assert.equal(a.$("threads").querySelectorAll(".thread-row").length, 0, "no threads shown");
  assert.equal(a.$("threads-empty").hidden, false, "empty state is visible");
  assert.equal(a.$("threads-empty").textContent, "No conversations with people yet.", "empty state text is correct");
});

// Owner, 2026-11-18: "The web app often says 'offline' … remove it and refresh on activation."
test("there is no offline banner; bringing the page back refreshes the inbox once, quietly", async t => {
  let unreachable = false;
  const a = await app(t, { configure: server => {
    server.intercept = call => {
      if (unreachable && call.path === "/v1/inbox" && !call.query.has("wait")) throw new TypeError("Failed to fetch");
    };
  }});
  await until(() => a.$("inbox-status").hidden);
  assert.equal(a.$("offline-banner"), null, "the banner is gone from the page");
  const listings = () => a.server.calls.filter(c => c.path === "/v1/inbox" && !c.query.has("wait")).length;
  const before = listings();
  unreachable = true;
  // Tab shown, window focused and network back usually arrive together: one refresh, not three.
  a.w.document.dispatchEvent(new a.w.Event("visibilitychange"));
  a.w.dispatchEvent(new a.w.Event("focus"));
  a.w.dispatchEvent(new a.w.Event("online"));
  await tick(80);
  assert.equal(listings(), before + 1, "one refresh for one activation");
  assert.equal(a.$("inbox-error").hidden, true, "a failed refresh shows nothing");
  assert.equal(a.$("retry-inbox").hidden, true, "and offers no retry for a blip");
  assert.doesNotMatch(a.$("pane-list").textContent, /offline/i);
  assert.ok(a.$("threads").querySelector(".thread-row"), "what was loaded stays on screen");
  // Within the throttle window another activation does not ask again.
  a.w.dispatchEvent(new a.w.Event("focus"));
  await tick(40);
  assert.equal(listings(), before + 1, "throttled");
});
