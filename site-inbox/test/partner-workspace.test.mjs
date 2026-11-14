import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { JSDOM, VirtualConsole } from "jsdom";
const read = (p) =>
  readFileSync(new URL("../../site/" + p, import.meta.url), "utf8");
const tick = async () => {
  for (let i = 0; i < 20; i++) await new Promise((r) => setImmediate(r));
};
async function fixture(t, { staff = false, publishers = true } = {}) {
  const dom = new JSDOM('<main id="root"></main>', {
    url: "https://pigeonpost.dev/account#support",
    runScripts: "outside-only",
    virtualConsole: new VirtualConsole(),
  });
  const w = dom.window;
  const calls = [],
    sockets = [];
  class Socket {
    constructor(url) {
      this.url = url;
      sockets.push(this);
    }
    send(v) {
      this.sent = JSON.parse(v);
    }
    close() {
      this.closed = true;
    }
    event(v) {
      this.onmessage?.({ data: JSON.stringify(v) });
    }
  }
  w.WebSocket = Socket;
  const tickets = [
    {
      id: "t1",
      subject: "Application help",
      publisher_id: "p1",
      publisher_name: "Example Studio",
      publisher_status: "applied",
      status: "open",
      priority: "normal",
      category: "application",
      unread: true,
      last_seq: 1,
      updated_at: "2026-11-14T12:00:00Z",
      messages: [
        {
          seq: 1,
          body: "Can you check our documents?",
          author_kind: "partner",
          created_at: "2026-11-14T12:00:00Z",
        },
      ],
      older_before: null,
    },
  ];
  let fail = false,
    scope = "p1";
  const deps = {
    getToken: () => "private-token",
    postbox: "https://postbox.pigeonpost.dev",
    pb: async (path, init = {}) => {
      calls.push({ path, init });
      if (path === "/v1/publishers")
        return {
          data: publishers
            ? [
                { id: "p1", display_name: "Example Studio" },
                { id: "p2", display_name: "Second business" },
              ]
            : [],
        };
      const u = new URL(path, "https://x");
      if (init.method === "POST" && u.pathname.endsWith("/messages")) {
        if (fail) throw new Error("Network unavailable");
        const b = JSON.parse(init.body);
        tickets[0].messages.push({
          seq: 2,
          body: b.body,
          author_kind: staff ? "staff" : "partner",
          created_at: "2026-11-14T12:01:00Z",
        });
        return { data: { id: "t1" } };
      }
      if (u.pathname.endsWith("/read")) return { data: { read: true } };
      if (u.pathname === "/v1/support/tickets")
        return {
          data: u.searchParams.get("publisher") === "p2" ? [] : tickets,
          next_offset: null,
        };
      return { data: structuredClone(tickets[0]) };
    },
    stepUp: (v) => calls.push({ stepUp: v }),
  };
  w.eval(read("marketplace-core.js"));
  w.eval(read("workspace.js"));
  w.eval(read("partner-support.js"));
  const root = w.document.querySelector("#root");
  const ui = w.PPMarketplace.partnerSupport(root, deps, { staff });
  t.after(() => {
    ui.destroy();
    dom.window.close();
  });
  await tick();
  return { w, root, calls, sockets, tickets, ui, setFail: (v) => (fail = v) };
}
test("socket authentication never puts credentials in a URL and refreshes incoming messages", async (t) => {
  const p = await fixture(t);
  const s = p.sockets[0];
  assert.equal(s.url.protocol, "wss:");
  assert.equal(s.url.search, "");
  s.onopen();
  assert.equal(s.sent.token, "private-token");
  assert.equal(s.sent.publisher, "p1");
  s.event({ type: "ready" });
  await tick();
  assert.match(p.root.textContent, /Live updates on/);
  p.root.querySelector(".ws-ticket").click();
  await tick();
  p.tickets[0].messages.push({
    seq: 2,
    body: "We can help right now.",
    author_kind: "staff",
    created_at: "2026-11-14T12:01:00Z",
  });
  s.event({ type: "support.changed", ticket_id: "t1" });
  await tick();
  assert.match(
    p.root.querySelector(".ws-messages").textContent,
    /We can help right now/,
  );
});
test("incoming replies preserve draft text and failed sends can be retried", async (t) => {
  const p = await fixture(t);
  p.root.querySelector(".ws-ticket").click();
  await tick();
  let input = p.root.querySelector("textarea");
  input.value = "My unsent reply";
  input.dispatchEvent(new p.w.Event("input"));
  p.sockets[0].event({ type: "support.changed", ticket_id: "t1" });
  await tick();
  assert.equal(p.root.querySelector("textarea").value, "My unsent reply");
  p.setFail(true);
  p.root
    .querySelector(".ws-composer")
    .dispatchEvent(new p.w.Event("submit", { cancelable: true }));
  await tick();
  assert.equal(p.root.querySelector("textarea").value, "My unsent reply");
  p.setFail(false);
  p.root
    .querySelector(".ws-composer")
    .dispatchEvent(new p.w.Event("submit", { cancelable: true }));
  await tick();
  assert.equal(p.root.querySelector("textarea").value, "");
  const sends = p.calls.filter((x) => x.path?.includes("/messages"));
  assert.equal(
    JSON.parse(sends[0].init.body).client_id,
    JSON.parse(sends[1].init.body).client_id,
  );
});
test("business switch closes previous socket and clears its conversation", async (t) => {
  const p = await fixture(t);
  p.root.querySelector(".ws-ticket").click();
  await tick();
  const first = p.sockets[0];
  const select = p.root.querySelector('select[aria-label="Business"]');
  select.value = "p2";
  select.dispatchEvent(new p.w.Event("change"));
  await tick();
  assert.equal(first.closed, true);
  assert.equal(p.root.querySelector(".ws-composer"), null);
  assert.ok(p.calls.some((x) => x.path?.includes("publisher=p2")));
});
test("staff queues carry server scope and show triage controls", async (t) => {
  const p = await fixture(t, { staff: true });
  assert.ok(p.calls.some((x) => x.path?.includes("staff=true")));
  p.root.querySelector(".ws-ticket").click();
  await tick();
  assert.ok(p.root.querySelector('[aria-label="Priority"]'));
  assert.match(p.root.textContent, /Assign to me/);
});
test("empty publisher list points to business setup without opening sockets", async (t) => {
  const p = await fixture(t, { publishers: false });
  assert.equal(p.sockets.length, 0);
  assert.ok(p.root.querySelector('a[href="#business"]'));
});
test("closing a screen releases its connection", async (t) => {
  const p = await fixture(t);
  p.ui.destroy();
  assert.equal(p.sockets[0].closed, true);
  assert.equal(p.root.classList.contains("support-root"), false);
});
test("all application states have actionable copy and unknown names are safe", async (t) => {
  const p = await fixture(t);
  assert.match(
    p.w.PPMarketplace.applicationStatus("needs_information").hint,
    /missing information/,
  );
  assert.equal(
    p.w.PPMarketplace.applicationStatus("__proto__").label,
    "Status unavailable",
  );
  assert.match(p.w.PPMarketplace.applicationStatus("approved").hint, /My apps/);
});
