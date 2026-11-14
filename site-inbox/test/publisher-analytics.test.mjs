import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";
const script = readFileSync(
  new URL("../../site/publisher-analytics.js", import.meta.url),
  "utf8",
);
const tick = async () => {
  for (let i = 0; i < 15; i++) await new Promise((r) => setImmediate(r));
};
const usage = {
  jobs: 4,
  succeeded: 3,
  failed: 1,
  cancelled: 0,
  in_progress: 0,
  active_users: 2,
  new_installations: 2,
  active_installations: 3,
};
const amounts = {
  sales: 2,
  gross_minor: 1000,
  tax_minor: 0,
  commission_minor: 100,
  publisher_minor: 900,
  reversed_publisher_minor: 200,
  net_publisher_minor: 700,
};
function report() {
  return {
    publisher: { id: "p1", display_name: "One" },
    generated_at: "2026-11-14T10:00:00Z",
    usage,
    daily: [],
    app_choices: [
      { id: "a", title: "Alpha" },
      { id: "b", title: "Beta" },
    ],
    apps: [
      { id: "a", title: "Alpha", status: "published", usage },
      {
        id: "b",
        title: "Beta",
        status: "withdrawn",
        usage: { ...usage, jobs: 2 },
      },
    ],
    finance: {
      status: "available",
      currencies: [
        { currency: "USD", ...amounts },
        { currency: "EUR", ...amounts },
      ],
      apps: [
        { listing_id: "a", currency: "USD", ...amounts },
        { listing_id: "b", currency: "EUR", ...amounts },
      ],
      daily: [],
      publisher_balances: [
        {
          currency: "USD",
          payable_minor: 300,
          reserve_minor: 40,
          recoverable_minor: 0,
          paid_out_minor: 500,
        },
      ],
    },
  };
}
function page(t, handler, staff = false) {
  const dom = new JSDOM('<div id="root"></div>', {
    url: "https://pigeonpost.dev/account#analytics",
    runScripts: "outside-only",
  });
  t.after(() => dom.window.close());
  dom.window.eval(script);
  const calls = [],
    step = [];
  dom.window.PPMarketplace.analytics(
    dom.window.document.querySelector("#root"),
    {
      pb: async (path) => {
        calls.push(path);
        return handler(path);
      },
      stepUp: (e) => step.push(e),
    },
    { staff },
  );
  return {
    window: dom.window,
    root: dom.window.document.querySelector("#root"),
    calls,
    step,
  };
}
const publishers = [
  { id: "p1", display_name: "One" },
  { id: "p2", display_name: "Two" },
];
test("multi-app report separates currencies, shows outcomes and publisher-wide payouts", async (t) => {
  const p = page(t, (path) =>
    path === "/v1/publishers" ? { data: publishers } : { data: report() },
  );
  await tick();
  assert.match(p.root.textContent, /App performance/);
  assert.match(p.root.textContent, /withdrawn/);
  assert.match(p.root.textContent, /USD/);
  assert.match(p.root.textContent, /EUR/);
  assert.match(p.root.textContent, /Paid out in period/);
  assert.equal(p.root.querySelectorAll("svg").length, 2);
  assert.match(p.calls[1], /from=.*to=/);
});
test("app and date filters reach the shared staff endpoint", async (t) => {
  const p = page(
    t,
    (path) =>
      path.startsWith("/v1/staff/publisher-analytics")
        ? { data: publishers, next_offset: null }
        : { data: report() },
    true,
  );
  await tick();
  const app = p.root.querySelector("#pa-app");
  app.value = "b";
  app.dispatchEvent(new p.window.Event("change"));
  await tick();
  assert.match(p.calls.at(-1), /^\/v1\/staff\/publishers\/p1\/analytics\?/);
  assert.match(p.calls.at(-1), /listing_id=b/);
  p.root.querySelector("#pa-from").value = "2026-01-01";
  p.root.querySelector("#pa-to").value = "2026-01-31";
  p.root
    .querySelector(".pa-controls")
    .dispatchEvent(new p.window.Event("submit", { cancelable: true }));
  await tick();
  assert.match(p.calls.at(-1), /to=2026-02-01T00%3A00%3A00/);
});
test("unavailable finance never becomes zero earnings; CSV leaves missing money blank", async (t) => {
  const r = report();
  r.finance = { status: "unavailable", message: "Earnings unavailable" };
  const p = page(t, (path) =>
    path === "/v1/publishers" ? { data: publishers } : { data: r },
  );
  await tick();
  assert.match(p.root.textContent, /Earnings unavailable/);
  assert.doesNotMatch(p.root.textContent, /\$0\.00/);
  const csv = p.window.PPMarketplace.analyticsCsv(r);
  assert.match(csv, /,"","","","","","",""/);
});
test("stale responses cannot replace the newly selected publisher", async (t) => {
  let resolve;
  const old = new Promise((r) => (resolve = r));
  const p = page(t, (path) =>
    path === "/v1/publishers"
      ? { data: publishers }
      : path.includes("/p1/")
        ? old
        : { data: { ...report(), apps: [], app_choices: [] } },
  );
  await tick();
  const select = p.root.querySelector("#pa-publisher");
  select.value = "p2";
  select.dispatchEvent(new p.window.Event("change"));
  await tick();
  resolve({ data: report() });
  await tick();
  assert.match(p.root.textContent, /No apps in this report/);
  assert.doesNotMatch(p.root.textContent, /Alpha/);
});
test("CSV quotes names and neutralizes formula injection", async (t) => {
  const p = page(t, () => ({ data: [] }));
  await tick();
  const r = report();
  r.apps[0].title = '=HYPERLINK("bad")';
  const csv = p.window.PPMarketplace.analyticsCsv(r);
  assert.match(csv, /"'=HYPERLINK\(""bad""\)"/);
  assert.match(csv, /"a",.*"USD"/);
  assert.match(csv, /"b",.*"EUR"/);
});
test("step-up refusal offers verification and does not retain old report data", async (t) => {
  const failure = Object.assign(new Error("Verify"), {
    code: "step_up_required",
  });
  const p = page(
    t,
    () => {
      throw failure;
    },
    true,
  );
  await tick();
  assert.match(p.root.textContent, /strong sign-in/);
  [...p.root.querySelectorAll("button")]
    .find((b) => b.textContent === "Verify sign-in")
    .click();
  assert.equal(p.step.length, 1);
});
