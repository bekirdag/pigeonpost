// Pigeonpost for teams — package arithmetic, shared by the packages page and the start wizard.
//
// Prices are read from the bridge's public catalog (`GET /api/v1/catalog`) when it loads, else from
// the display copy in config.js. They are shown, never trusted: the bridge prices the checkout.
(function () {
  "use strict";
  const cfg = window.PPE_CONFIG || {};
  const PPE = (window.PPE = window.PPE || {});
  const P = (PPE.pricing = {});

  P.stops = () => (cfg.seatStops || [5, 10, 15, 20, 30, 50, 100, 200, 500]).slice();
  /// Slider positions: every stop, plus one more for "500+".
  P.positions = () => P.stops().length + 1;
  P.seatsAt = (index) => {
    const stops = P.stops();
    return index >= stops.length ? null : stops[Math.max(0, index)];
  };
  P.indexOf = (seats) => {
    const i = P.stops().indexOf(Number(seats));
    return i < 0 ? P.stops().indexOf(30) : i;
  };
  P.planCode = (seats, interval, currency) => `ent-${seats}-${interval}-${String(currency).toLowerCase()}`;

  let live = null; // code -> { amount, available }
  let openCurrencies = null;
  P.useCatalog = function (catalog) {
    if (!catalog || !Array.isArray(catalog.plans)) return;
    live = {};
    for (const p of catalog.plans) if (p.family === "ent") live[p.code] = { amount: p.amount, available: p.available !== false };
    if (Array.isArray(catalog.open_currencies)) openCurrencies = catalog.open_currencies;
  };
  P.reset = () => { live = null; openCurrencies = null; };
  P.isOpen = (currency) => !openCurrencies || openCurrencies.includes(currency);

  /// The amount for one plan, as a decimal string, or null when unknown.
  P.amount = function (seats, interval, currency) {
    const code = P.planCode(seats, interval, currency);
    if (live && live[code]) return live[code].amount;
    const table = cfg.prices && cfg.prices[currency] && cfg.prices[currency][interval];
    return table && table[seats] ? table[seats] : null;
  };

  /// "30 people · up to 300 agent addresses · 150 GB pooled storage"
  P.summary = (seats) => `${seats} people · up to ${seats * 10} agent addresses · ${seats * 5} GB pooled storage`;

  /// Per-seat monthly price, for the "per person" line.
  P.perSeatMonthly = function (seats, interval, currency) {
    const amount = P.amount(seats, interval, currency);
    if (amount === null) return null;
    const months = interval === "yearly" ? 12 : 1;
    return Math.round((Number(amount) / months / seats) * 100) / 100;
  };

  P.loadCatalog = async function () {
    try {
      const res = await window.fetch(`${cfg.apiBase || "/api"}/v1/catalog`, { headers: { accept: "application/json" } });
      if (res.ok) P.useCatalog(await res.json());
    } catch { /* the display copy stands */ }
  };
})();
