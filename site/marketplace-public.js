// The public marketplace page (marketplace.html): the live catalog, read without signing in.
//
// `GET /v1/marketplace/listings` answers anonymous callers with published public listings when
// the marketplace is open to everyone; until then (or if the postbox cannot be reached) the
// section stays hidden and the page reads as before. Cards link to the account page, where a
// signed-in person opens, buys, installs or runs a listing.
(function () {
  "use strict";

  const MP = window.PPMarketplace;
  const box = document.getElementById("mp-public");
  if (!MP || !box || typeof fetch !== "function") return;
  const el = MP.el;

  async function load() {
    let rows;
    try {
      const res = await fetch(`${MP.postboxOrigin}/v1/marketplace/listings`, { headers: { accept: "application/json" }, credentials: "omit" });
      if (!res.ok) return;
      rows = ((await res.json()) || {}).data;
    } catch (_) { return; }
    if (!Array.isArray(rows) || !rows.length) return;
    const open = "https://account.pigeonpost.dev/#services";
    const rank = (l) => (l.featured ? Number(l.featured_rank ?? 1e6) : 1e9);
    const sorted = rows.slice().sort((a, b) => rank(a) - rank(b) || Number(Boolean(a.coming_soon)) - Number(Boolean(b.coming_soon)));
    const soon = rows.every((l) => l.coming_soon);
    box.append(
      el("h2", { class: "mp-section-title", id: "mp-public-title", text: soon ? "Coming soon to the Marketplace" : "In the Marketplace" }),
      el("p", { class: "mp-public-note", text: soon
        ? "A first look at the services on their way. Each one is a sample from Pigeonpost until it opens."
        : "Sign in to open a service, install it into your Nest or run it." }),
      el("ul", { class: "mp-grid", "aria-labelledby": "mp-public-title" },
        sorted.slice(0, 12).map((l) => MP.card(l, { href: open, heading: "h3" }))));
    box.hidden = false;
  }

  load();
})();
