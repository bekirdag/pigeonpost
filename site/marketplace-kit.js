// The public marketplace in the design kit's look (templates/marketplace.html): listing cards,
// logo tiles, publisher badges and icons for marketplace.html (marketplace-public.js) and
// partners.html (partners.js). The account page keeps its own cards from marketplace-core.js.
//
// Everything drawn here comes from the postbox's public answers; nothing is made up. A listing's
// page is /marketplace.html?listing=<id>; buying and running happen in the account.
(function () {
  "use strict";

  const MP = window.PPMarketplace;
  if (!MP) return;
  const el = MP.el;
  const K = (MP.kit = {});
  const SVG = "http://www.w3.org/2000/svg";

  K.icon = function (name, extra) {
    const svg = document.createElementNS(SVG, "svg");
    svg.setAttribute("class", `pp-icon${extra ? ` ${extra}` : ""}`);
    svg.setAttribute("aria-hidden", "true");
    const use = document.createElementNS(SVG, "use");
    use.setAttribute("href", `/kit/icons.svg#i-${name}`);
    svg.append(use);
    return svg;
  };

  K.CATEGORY_ICONS = {
    productivity: "file-text", legal: "briefcase", finance: "receipt", support: "headphones",
    marketing: "trending-up", sales: "shopping-bag", language: "languages", research: "book-open",
    developer: "code", hr: "users", recruiting: "user-plus", other: "package",
  };
  K.categoryIcon = (code) => K.CATEGORY_ICONS[code] || "package";

  K.listingHref = (id) => `/marketplace.html?listing=${encodeURIComponent(id)}`;
  K.categoryHref = (code) => `/marketplace.html?category=${encodeURIComponent(code)}`;
  K.publisherHref = (id) => `/partners.html?publisher=${encodeURIComponent(id)}`;
  // The account page opens, buys and runs a listing; /account forwards there.
  K.accountHref = (id) => `/account?listing=${encodeURIComponent(id)}#services`;

  K.TONES = ["violet", "blue", "teal", "amber", "green", "red"];

  /// The kit's logo tile: the uploaded logo, or the title's initials on a tone picked from the id.
  K.logo = function (l, large) {
    const tone = K.TONES[MP.logoTone(l && l.id) % K.TONES.length];
    const tile = el("span", {
      class: `pp-listing-logo pp-listing-logo--${tone}${large ? " pp-listing-logo--l" : ""}`, "aria-hidden": "true",
    });
    const letters = () => tile.replaceChildren(MP.initials(l && (l.title || l.name)));
    const urls = l && l.logo && l.logo.urls;
    const src = urls ? MP.assetUrl(large ? urls["512"] || urls["128"] : urls["128"]) : null;
    if (!src) { letters(); return tile; }
    const img = el("img", { class: "mk-logo-img", src, alt: "", width: large ? "72" : "48", height: large ? "72" : "48", loading: "lazy", decoding: "async" });
    img.addEventListener("error", letters, { once: true });
    tile.classList.add("mk-logo--image");
    tile.append(img);
    return tile;
  };

  K.publisherName = (l) => {
    const p = (l && l.publisher) || {};
    return p.first_party ? "Pigeonpost" : p.display_name || "Unknown publisher";
  };

  /// The verified mark: a certified publisher, or Pigeonpost's own listing.
  K.verified = function (l, long) {
    const p = (l && l.publisher) || {};
    if (!p.first_party && !p.verified) return null;
    const text = p.first_party ? "Official" : long ? "Verified publisher" : "Verified";
    return el("span", { class: "pp-verified", title: p.first_party ? "Made by Pigeonpost" : "Verified publisher" }, K.icon("badge-check"), text);
  };

  /// Price, as the card's foot shows it: [amount, unit] or [text, null].
  K.price = function (l) {
    if (l.coming_soon) return ["Not on sale yet", null];
    if (l.price_from && Number(l.price_from.amount_cents) > 0) {
      return [MP.money(l.price_from.amount_cents, l.price_from.currency || "USD"), `per ${l.price_from.unit || "run"}`];
    }
    const p = l.price;
    if (!p || Number(p.amount_minor) === 0) return ["Free", null];
    return [MP.money(p.amount_minor, p.currency), p.term === "monthly" ? "per month" : "one-off"];
  };
  K.priceNode = function (l, cls) {
    const [amount, unit] = K.price(l);
    return el("span", { class: cls || "pp-listing-card__price" }, amount, unit ? " " : null, unit ? el("small", { text: unit }) : null);
  };

  K.hosting = function (l) {
    if (l.sample) return ["sparkles", "Sample from Pigeonpost"];
    if (l.hosting === "publisher") return ["server", "Hosted by the publisher"];
    return ["cloud", "Runs in your Nest"];
  };

  /// One catalog card (article.pp-listing-card). The title links to the listing's page.
  K.card = function (l, opts) {
    const o = opts || {};
    const soon = Boolean(l.coming_soon);
    const [hostIcon, hostText] = K.hosting(l);
    const category = MP.categoryLabel(l.category);
    return el("article", { class: `pp-listing-card${soon ? " pp-listing-card--soon" : ""}`, "data-listing": l.id },
      soon ? el("span", { class: "pp-listing-card__ribbon", "aria-hidden": "true", text: "Coming soon" }) : null,
      el("div", { class: "pp-listing-card__top" },
        K.logo(l),
        el("div", { class: "pp-listing-card__id" },
          el(o.heading || "h3", { class: "pp-listing-card__title" },
            el("a", { class: "pp-listing-card__link", href: K.listingHref(l.id), "aria-label": `${l.title}${soon ? " (coming soon)" : ""}` }, l.title)),
          el("p", { class: "pp-listing-card__publisher" }, K.publisherName(l), " ", K.verified(l)))),
      category ? el("span", { class: "pp-listing-card__category" }, K.icon(K.categoryIcon(l.category)), category) : null,
      el("p", { class: "pp-listing-card__desc", text: l.summary || "" }),
      el("div", { class: "pp-listing-card__foot" },
        K.priceNode(l),
        el("span", { class: "pp-listing-card__host" }, K.icon(hostIcon), hostText)));
  };

  /// Every public listing, following the cursor (a bounded number of pages).
  K.fetchListings = async function () {
    const rows = [];
    let cursor = null;
    for (let page = 0; page < 20; page++) {
      const qs = cursor ? `?cursor=${encodeURIComponent(cursor)}` : "";
      const res = await fetch(`${MP.postboxOrigin}/v1/marketplace/listings${qs}`, { headers: { accept: "application/json" }, credentials: "omit" });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const body = (await res.json()) || {};
      if (Array.isArray(body.data)) rows.push(...body.data);
      cursor = body.next_cursor;
      if (!cursor) break;
    }
    return rows;
  };

  K.empty = function (icon, title, text, action) {
    return el("div", { class: "pp-empty pp-empty--boxed" },
      el("span", { class: "pp-empty__icon" }, K.icon(icon)),
      el("p", { class: "pp-empty__title", text: title }),
      text ? el("p", { class: "pp-empty__text", text }) : null,
      action ? el("div", { class: "pp-empty__actions" }, action) : null);
  };
})();
