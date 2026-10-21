// Pigeonpost for teams — the packages page: one slider, one price, one primary action.
(function () {
  "use strict";
  const PPE = window.PPE;
  const P = PPE.pricing;
  const $ = PPE.$;
  const state = { index: P.indexOf(30), interval: "yearly", currency: "USD" };

  function render() {
    const seats = P.seatsAt(state.index);
    const slider = $("#seats");
    slider.max = String(P.positions() - 1);
    slider.value = String(state.index);
    const start = $("#start");
    const sales = $("#sales");
    const note = $("#currency-note");
    if (seats === null) {
      $("#seats-value").textContent = "500+";
      slider.setAttribute("aria-valuetext", "More than 500 people");
      $("#price").textContent = "Let's talk";
      $("#summary").textContent = "More than 500 people, with volume pricing and a custom agreement.";
      start.hidden = true;
      sales.hidden = false;
      note.hidden = true;
      return;
    }
    $("#seats-value").textContent = String(seats);
    slider.setAttribute("aria-valuetext", `${seats} people`);
    const amount = P.amount(seats, state.interval, state.currency);
    const price = $("#price");
    price.replaceChildren();
    if (amount === null) {
      price.textContent = "Price unavailable";
    } else {
      price.append(PPE.money(amount, state.currency), PPE.el("small", {}, state.interval === "yearly" ? " per year" : " per month"));
      const per = P.perSeatMonthly(seats, state.interval, state.currency);
      price.append(PPE.el("div", { class: "note" }, `${PPE.money(per, state.currency)} per person per month`));
    }
    $("#summary").textContent = P.summary(seats);
    const open = P.isOpen(state.currency);
    note.hidden = open;
    note.textContent = open ? "" : `Checkout in ${state.currency} opens soon. You can start now in another currency.`;
    start.hidden = false;
    sales.hidden = true;
    start.textContent = `Start with ${seats} seats`;
    start.href = `/start?seats=${seats}&interval=${state.interval}&currency=${state.currency}`;
    for (const b of PPE.$$("[data-interval]")) b.setAttribute("aria-pressed", String(b.dataset.interval === state.interval));
    for (const b of PPE.$$("[data-currency]")) b.setAttribute("aria-pressed", String(b.dataset.currency === state.currency));
  }

  function salesSheet() {
    PPE.openSheet("Talk to sales", (body) => {
      const name = PPE.el("input", { type: "text", autocomplete: "name", required: true, maxlength: 120 });
      const company = PPE.el("input", { type: "text", autocomplete: "organization", required: true, maxlength: 160 });
      const contact = PPE.el("input", { type: "text", autocomplete: "off", spellcheck: "false", required: true, maxlength: 254 });
      const size = PPE.el("select", {}, ["500–1,000", "1,000–5,000", "More than 5,000"].map((s) => PPE.el("option", {}, s)));
      const message = PPE.el("textarea", { maxlength: 2000 });
      const status = PPE.el("p", { class: "status-line", role: "status" });
      const send = PPE.el("button", { class: "btn btn-primary", type: "submit" }, "Send");
      const form = PPE.el("form", { novalidate: true },
        PPE.el("p", { class: "muted" }, "Tell us about your team and we'll get back to you within one business day."),
        PPE.field("Your name", name), PPE.field("Company", company),
        PPE.field("Contact address", contact, "Where we should reply."),
        PPE.field("Team size", size), PPE.field("Anything else?", message), status,
        PPE.el("div", { class: "actions" }, send));
      form.addEventListener("submit", async (e) => {
        e.preventDefault();
        if (!name.value.trim() || !company.value.trim() || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(contact.value.trim())) {
          status.className = "status-line status-bad";
          status.textContent = "Fill in your name, company and a valid contact address.";
          return;
        }
        send.disabled = true;
        try {
          await PPE.bridge("/ent/sales", { method: "POST", auth: false, body: {
            name: name.value.trim(), company: company.value.trim(), contact: contact.value.trim(),
            size: size.value, ...(message.value.trim() ? { message: message.value.trim() } : {}) } });
          PPE.closeSheet();
          PPE.toast("Thanks — we'll be in touch.");
        } catch (err) {
          send.disabled = false;
          status.className = "status-line status-bad";
          status.textContent = PPE.explain(err);
        }
      });
      body.append(form);
    });
  }

  function init() {
    PPE.renderAccount();
    const q = new URLSearchParams(window.location.search);
    if (q.get("seats")) state.index = P.indexOf(q.get("seats"));
    if (["monthly", "yearly"].includes(q.get("interval"))) state.interval = q.get("interval");
    if (["USD", "TRY"].includes(q.get("currency"))) state.currency = q.get("currency");
    $("#seats").addEventListener("input", (e) => { state.index = Number(e.target.value); render(); });
    for (const b of PPE.$$("[data-interval]")) b.addEventListener("click", () => { state.interval = b.dataset.interval; render(); });
    for (const b of PPE.$$("[data-currency]")) b.addEventListener("click", () => { state.currency = b.dataset.currency; render(); });
    $("#sales").addEventListener("click", salesSheet);
    render();
    P.loadCatalog().then(render);
  }
  PPE.packages = { render, state, salesSheet };
  init();
})();
