// The packages page: slider stops, prices equal to the approved catalog, Talk to sales above 500.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { load, reply, tick, plain } from "./helpers.mjs";

const catalog = JSON.parse(readFileSync(new URL("../../store-adapter/catalog/pigeonpost-catalog.json", import.meta.url), "utf8"));

test("the display price copy equals the catalog's current enterprise editions", (t) => {
  const { window } = load(t, "index.html");
  const prices = window.PPE_CONFIG.prices;
  let n = 0;
  for (const [code, plan] of Object.entries(catalog.plans)) {
    if (plan.family !== "ent") continue;
    const year = Object.keys(plan.editions).sort().pop();
    const interval = plan.interval === "year" ? "yearly" : "monthly";
    assert.equal(prices[plan.currency][interval][plan.seats], plan.editions[year].amount, code);
    assert.equal(window.PPE.pricing.amount(plan.seats, interval, plan.currency), plan.editions[year].amount);
    n++;
  }
  assert.equal(n, 36);
  return new Promise((r) => setTimeout(r, 50));
  assert.deepEqual(plain(window.PPE_CONFIG.seatStops), [5, 10, 15, 20, 30, 50, 100, 200, 500]);
});

test("the slider prices each stop and turns into Talk to sales at 500+", async (t) => {
  const { window, $ } = load(t, "index.html", { fetch: () => reply(503, {}) });
  await tick();
  const slider = $("#seats");
  assert.equal($("#seats-value").textContent, "30");
  assert.match($("#price").textContent, /\$1,800 per year/);
  assert.equal($("#summary").textContent, "30 people · up to 300 agent addresses · 150 GB pooled storage");
  assert.equal($("#start").textContent, "Start with 30 seats");
  assert.match($("#start").getAttribute("href"), /^\/start\?seats=30&interval=yearly&currency=USD$/);
  $("[data-interval=monthly]").click();
  assert.match($("#price").textContent, /\$180 per month/);
  $("[data-currency=TRY]").click();
  assert.match($("#price").textContent, /9,191/);
  slider.value = "9"; slider.dispatchEvent(new window.Event("input"));
  assert.equal($("#seats-value").textContent, "500+");
  assert.equal($("#start").hidden, true);
  assert.equal($("#sales").hidden, false);
  slider.value = "0"; slider.dispatchEvent(new window.Event("input"));
  assert.equal($("#summary").textContent, "5 people · up to 50 agent addresses · 25 GB pooled storage");
});

test("the live catalog wins and a closed currency is said, not hidden", async (t) => {
  const live = { open_currencies: ["TRY"], plans: [{ code: "ent-30-yearly-usd", family: "ent", amount: "1799.00", available: true }] };
  const { $ } = load(t, "index.html", { fetch: (c) => c.url.endsWith("/api/v1/catalog") ? reply(200, live) : reply(404, {}) });
  await tick();
  assert.match($("#price").textContent, /1,799/);
  assert.equal($("#currency-note").hidden, false);
});

test("Talk to sales posts the form to the bridge", async (t) => {
  const { window, $, calls } = load(t, "index.html", { url: "https://enterprise.pigeonpost.dev/?seats=500", fetch: (c) => c.url.endsWith("/ent/sales") ? reply(202, { received: true }) : reply(404, {}) });
  await tick();
  const slider = $("#seats"); slider.value = "9"; slider.dispatchEvent(new window.Event("input"));
  $("#sales").click();
  const inputs = window.document.querySelectorAll(".sheet input");
  inputs[0].value = "Ada"; inputs[1].value = "Example Corp"; inputs[2].value = "ada@example.test";
  window.document.querySelector(".sheet form").dispatchEvent(new window.Event("submit", { cancelable: true }));
  await tick();
  const post = calls.find((c) => c.url.endsWith("/api/ent/sales"));
  assert.equal(post.method, "POST");
  assert.equal(post.body.company, "Example Corp");
  assert.equal(window.document.querySelector(".sheet"), null);
});
