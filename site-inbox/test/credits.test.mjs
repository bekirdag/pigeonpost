// The account page's Credits card (site/credits.js) against fake bridge answers. Synthetic amounts only.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { JSDOM, VirtualConsole } from "jsdom";

const source = readFileSync(new URL("../../site/credits.js", import.meta.url), "utf8");

const tick = async (n = 25) => {
  for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r));
};

function card(t, { routes, deps } = {}) {
  const dom = new JSDOM('<section id="credits"></section>', {
    url: "https://pigeonpost.dev/account",
    runScripts: "outside-only",
    virtualConsole: new VirtualConsole(),
  });
  t.after(() => dom.window.close());

  const calls = [];
  const navigations = [];
  let clearReturnKey = null;

  const answer = (method, path, body) => {
    // The card's objects come from the page's realm; compare them as plain JSON.
    calls.push({ method, path, body: body === undefined ? undefined : JSON.parse(JSON.stringify(body)) });
    const res = routes ? routes(method, path, body) : undefined;
    if (res !== undefined) return res;
    
    // Default routes
    if (method === "GET" && path === "/v1/credits/packs") {
      return { packs: [{pack_id:"usd_10",amount_cents:1000},{pack_id:"usd_25",amount_cents:2500},{pack_id:"usd_50",amount_cents:5000},{pack_id:"usd_100",amount_cents:10000}], currencies: ["USD","EUR","TRY"] };
    }
    if (method === "POST" && path === "/v1/credits/quotes") {
      return { quote_id: "q-" + body.currency, pack_id: body.pack_id, credit_cents: 2500, charge: { currency: body.currency, amount_minor: body.currency === "TRY" ? 85625 : 2500, tax_minor: 0 }, fx: body.currency === "TRY" ? { rate: "34.25", source: "tcmb" } : null, expires_at: "2030-01-01T00:00:00Z" };
    }
    if (method === "GET" && path === "/v1/credits/balance") {
      return { available_cents: 7500, held_cents: 2500 };
    }
    if (method === "GET" && path.startsWith("/v1/credits/history")) {
      return { entries: [{ kind: "purchase", amount_cents: 10000, created_at: "2026-11-10T10:00:00Z", charge: { amount_minor: 342500, currency: "TRY" } }, { kind: "sale", amount_cents: -2500, created_at: "2026-11-11T10:00:00Z", description: "Wodo AI HR" }], next_cursor: null };
    }
    if (method === "POST" && path === "/v1/credits/checkout") {
      return { intent_id: body.intent_id, checkout_id: "c1", payment_page_url: "https://sandbox-cpp.iyzipay.com/?token=t" };
    }

    throw Object.assign(new Error("not found"), { status: 404, body: { code: "not_found" } });
  };

  const window = dom.window;
  const doc = window.document;
  
  window.eval(source);

  const section = doc.getElementById("credits");
  
  const defaultDeps = {
    bridgeGet: async (path) => answer("GET", path),
    bridgeAction: async (path, body) => answer("POST", path, body),
    billingProfileId: async () => "00000000-0000-4000-8000-000000000042",
    goBilling: () => {},
    randomHex32: () => "ab".repeat(16),
    search: "",
    navigate: (url) => navigations.push(url),
    wait: async () => {},
    attempts: 3,
    clearReturn: (key) => { clearReturnKey = key; },
  };

  const finalDeps = { ...defaultDeps, ...(deps || {}) };
  
  const done = window.PPCredits.mount(section, finalDeps).done;

  return {
    window,
    doc,
    $: (sel) => doc.querySelector(sel),
    calls,
    navigations,
    done,
    getClearReturnKey: () => clearReturnKey,
  };
}

test('the card shows the balance, four packs, and the charge in the chosen currency', async (t) => {
  const ctx = card(t);
  await ctx.done;
  await tick();

  // Check balance text
  const balEl = ctx.$('#cr-balance');
  assert.ok(balEl, "Balance element exists");
  // Balance: $75.00 · $25.00 reserved for runs in progress
  assert.match(balEl.textContent, /Balance: \$75\.00 · \$25\.00 reserved/);

  // Check packs
  const packInputs = ctx.doc.querySelectorAll('input[name="cr-pack"]');
  assert.strictEqual(packInputs.length, 4);

  // Check currency select
  const currencySelect = ctx.$('#cr-currency');
  assert.ok(currencySelect, "Currency select exists");
  const options = Array.from(currencySelect.options).map(o => o.value);
  assert.deepStrictEqual(options, ["USD", "EUR", "TRY"]);

  // Check initial quote (USD)
  const quoteEl = ctx.$('#cr-quote');
  assert.ok(quoteEl, "Quote element exists");
  // You pay $25.00 for $25.00 of credit
  assert.match(quoteEl.textContent, /You pay \$25\.00 for \$25\.00 of credit/);

  // Change currency to TRY
  currencySelect.value = "TRY";
  currencySelect.dispatchEvent(new ctx.window.Event("change"));
  await tick();

  // Check TRY quote
  // You pay TRY 856.25 for $25.00 of credit (1 USD = 34.25 TRY, TCMB rate)
  assert.match(quoteEl.textContent, /You pay (?:TRY\s?856\.25|₺856\.25)/);
  assert.match(quoteEl.textContent, /34\.25 TRY/);

  // Check last quote call body
  const lastQuoteCall = ctx.calls.filter(c => c.method === "POST" && c.path === "/v1/credits/quotes").pop();
  assert.ok(lastQuoteCall);
  assert.deepStrictEqual(lastQuoteCall.body, { pack_id: "usd_25", currency: "TRY" });
});

test('buy opens the iyzico page with the quote and billing profile', async (t) => {
  const ctx = card(t);
  await ctx.done;
  await tick();

  // Submit form
  const buyForm = ctx.$('#cr-buy');
  assert.ok(buyForm, "Buy form exists");
  buyForm.dispatchEvent(new ctx.window.Event("submit", { bubbles: true, cancelable: true }));
  await tick();

  // Check checkout call
  const checkoutCall = ctx.calls.find(c => c.method === "POST" && c.path === "/v1/credits/checkout");
  assert.ok(checkoutCall, "Checkout call made");
  
  // Expected body: { intent_id: "ab".repeat(16), quote_id: "q-USD", billing_profile_id: "00000000-0000-4000-8000-000000000042" }
  assert.deepStrictEqual(checkoutCall.body, {
    intent_id: "ab".repeat(16),
    quote_id: "q-USD",
    billing_profile_id: "00000000-0000-4000-8000-000000000042"
  });

  // Check navigation
  assert.deepStrictEqual(ctx.navigations, ["https://sandbox-cpp.iyzipay.com/?token=t"]);
});

test('without billing details buy goes to the billing form instead', async (t) => {
  let goBillingCount = 0;
  const ctx = card(t, {
    deps: {
      billingProfileId: async () => null,
      goBilling: () => { goBillingCount++; }
    }
  });

  await ctx.done;
  await tick();

  // Submit form
  const buyForm = ctx.$('#cr-buy');
  buyForm.dispatchEvent(new ctx.window.Event("submit", { bubbles: true, cancelable: true }));
  await tick();

  // No checkout call should have been made
  const hasCheckout = ctx.calls.some(c => c.method === "POST" && c.path === "/v1/credits/checkout");
  assert.strictEqual(hasCheckout, false);

  assert.strictEqual(goBillingCount, 1);

  const statusEl = ctx.$('#cr-status');
  assert.ok(statusEl);
  assert.match(statusEl.textContent, /Save your billing details/);
});

test('back from iyzico the card confirms the purchase', async (t) => {
  const intentId = "cd".repeat(16);
  
  const customRoutes = (method, path, body) => {
    if (method === "GET" && path === `/v1/credits/checkout/${intentId}`) {
      return { status: "paid", credit_cents: 2500 };
    }
    return undefined; // Fall back to default
  };

  const ctx = card(t, {
    routes: customRoutes,
    deps: { search: `?credits=${intentId}` }
  });

  await ctx.done;
  await tick();

  const statusEl = ctx.$('#cr-status');
  assert.ok(statusEl);
  // Payment received. $25.00 of credit was added
  assert.match(statusEl.textContent, /Payment received\. \$25\.00 of credit was added/);
  
  assert.strictEqual(ctx.getClearReturnKey(), "credits");
});

test('a failed payment adds nothing', async (t) => {
  const intentId = "cd".repeat(16);
  
  const customRoutes = (method, path, body) => {
    if (method === "GET" && path === `/v1/credits/checkout/${intentId}`) {
      return { status: "failed", credit_cents: 0 };
    }
    return undefined;
  };

  const ctx = card(t, {
    routes: customRoutes,
    deps: {
      search: `?credits=${intentId}`,
      clearReturn: (key) => {}
    }
  });

  await ctx.done;
  await tick();

  const statusEl = ctx.$('#cr-status');
  assert.ok(statusEl);
  // did not go through
  assert.match(statusEl.textContent, /did not go through/);
});

test('history lists purchases and spends', async (t) => {
  const ctx = card(t);
  await ctx.done;
  await tick();

  const historyList = ctx.$('#cr-history');
  assert.ok(historyList);
  
  const items = historyList.querySelectorAll('li');
  assert.strictEqual(items.length, 2);

  // First item: Purchase
  // Bought credit · +$100.00 (paid TRY 3425.00)
  const firstItemText = items[0].textContent;
  assert.match(firstItemText, /Bought credit · \+\$100\.00 \(paid TRY/);

  // Second item: Sale
  // Paid for a run · −$25.00 · Wodo AI HR
  const secondItemText = items[1].textContent;
  assert.match(secondItemText, /Paid for a run · −\$25\.00/);
  assert.match(secondItemText, /Wodo AI HR/);
});