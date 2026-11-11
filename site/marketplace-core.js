// Pigeonpost marketplace on the web: shared pieces for the Services catalog (marketplace-catalog.js),
// the publisher console (marketplace-publisher.js) and the staff dashboard (marketplace-staff.js).
//
// Like products.js, nothing here holds a credential or makes a request of its own: every call goes
// through the `pb(path, init)` helper the account page hands in. Everything is behind the postbox's
// per-account `marketplace` feature; with it off the account page mounts none of this.
(function () {
  "use strict";

  const MP = (window.PPMarketplace = window.PPMarketplace || {});

  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? "" : String(v));
    }
    for (const c of children.flat()) if (c != null && c !== false) node.append(c);
    return node;
  }
  MP.el = el;

  // Sentences for the postbox's machine-readable codes. A code nobody wrote a sentence for falls
  // back to the server's own detail.
  const ERRORS = {
    feature_disabled: "The marketplace isn't available for your account yet.",
    not_found: "That no longer exists, or you can't see it.",
    forbidden: "Your account can't do this.",
    human_required: "Only a person signed in here can do this.",
    step_up_required: "Confirm it's you with two-step sign-in, then try again.",
    step_up_unavailable: "Strong sign-in isn't set up on this server yet, so this action is off.",
    staff_required: "This needs a Pigeonpost staff role.",
    staff_conflict: "You have a stake in this publisher, so you can't decide this.",
    publisher_role_required: "Your publisher role doesn't cover this. Ask a publisher admin.",
    publisher_not_certified: "Your publisher isn't certified yet.",
    publisher_revoked: "This publisher's certification is revoked.",
    application_exists: "There is already an open or approved application for this owner.",
    application_decided: "This application has already been decided.",
    bad_display_name: "Names are 2 to 80 printable characters.",
    template_exists: "That template name is taken.",
    version_exists: "That version has already been built for this template.",
    build_not_queued: "This build is no longer queued.",
    invalid_role: "Pick publisher admin or publisher finance.",
    role_already_held: "This person already holds that role.",
    not_org_member: "Publisher roles go to active members of the organization only.",
    review_closed: "This review is already decided or withdrawn.",
    review_required: "This release hasn't been approved by Pigeonpost review.",
    release_revoked: "This release was revoked.",
    security_quarantined: "This release is quarantined.",
    visibility_not_available: "Invite-only and organization visibility aren't available yet.",
    listing_address_taken: "Another listing already uses this address.",
    listing_address_changed: "This release uses a different address from the listing.",
    listing_suspended: "This listing is suspended.",
    listing_not_suspended: "This listing isn't suspended.",
    already_installed: "You already have this installed.",
    installation_removed: "This installation was uninstalled.",
    installation_changed: "This installation changed in the meantime. Reload and try again.",
    invalid_configuration: "Some settings aren't valid.",
    configuration_invalid_for_release: "Your settings don't fit that version. Change them and try again.",
    no_previous_release: "There is no earlier version to go back to.",
    up_to_date: "You already have the newest version.",
    not_newer: "That version isn't newer than the one you have.",
  };

  MP.errorText = function (e, extra) {
    const code = e && e.code;
    if (code && extra && extra[code]) return extra[code];
    if (code && ERRORS[code]) return ERRORS[code];
    if (e && e.status === 401) return "Sign in again to continue.";
    return `Something went wrong${e && e.message ? `: ${e.message}` : "."}`;
  };

  /// A short `role="status"` region the screen writes its outcomes to. `say` replaces its text,
  /// optionally followed by an action element (a step-up button).
  MP.statusRegion = function (id) {
    const node = el("p", { id, class: "mp-status", role: "status", "aria-live": "polite" });
    node.say = (text, ...extra) => { node.textContent = text || ""; for (const x of extra) if (x) node.append(" ", x); };
    return node;
  };

  /// Report a failed call: a step-up refusal gets a button that sends the person through a strong
  /// sign-in and back to `section`; anything else gets a sentence.
  MP.report = function (deps, status, e, section, extra) {
    if (e && e.code === "step_up_required" && deps.stepUp) {
      const body = e.body || {};
      const btn = el("button", {
        type: "button", class: "btn btn-small", "data-stepup": section,
        onclick: () => deps.stepUp({ acr_values: body.acr_values || null, max_age: body.max_age ?? 600 }, section),
      }, "Confirm it's you");
      status.say(ERRORS.step_up_required, btn);
      return;
    }
    let text = MP.errorText(e, extra);
    const issues = e && e.body && Array.isArray(e.body.issues) ? e.body.issues : [];
    if (issues.length) text += " " + issues.map((i) => `${(i.path || "").replace(/^\//, "") || "settings"}: ${i.message || i.code}`).join("; ");
    status.say(text);
  };

  MP.when = (iso) => (iso ? new Date(iso).toLocaleDateString() : "");

  /// A price in minor units, in the listing's currency.
  MP.money = function (minor, currency) {
    const n = Number(minor) / 100;
    try { return new Intl.NumberFormat(undefined, { style: "currency", currency }).format(n); } catch (_) { return `${n.toFixed(2)} ${currency}`; }
  };
  MP.priceText = function (price) {
    if (!price || Number(price.amount_minor) === 0) return "Free";
    const m = MP.money(price.amount_minor, price.currency);
    return price.term === "monthly" ? `${m} a month` : `${m}, one-off`;
  };

  // ---- the marketplace look: categories, logos, cards ------------------------------------------
  //
  // The postbox keeps the same category codes (store/marketplace_look.rs). A listing without an
  // uploaded logo (`logo: null`) is drawn with its initials on a colour picked from its id, so the
  // catalog, the listing page and the publisher console draw the same tile everywhere.

  MP.CATEGORIES = [
    ["productivity", "Productivity"], ["legal", "Legal"], ["finance", "Finance"],
    ["support", "Customer support"], ["marketing", "Marketing"], ["sales", "Sales"],
    ["language", "Language"], ["research", "Research"], ["developer", "Developer tools"],
    ["hr", "HR"], ["recruiting", "Recruiting"], ["other", "Other"],
  ];
  MP.categoryLabel = (code) => (MP.CATEGORIES.find(([c]) => c === code) || [null, null])[1];

  /// Where logo paths live. The account page talks to the postbox on its own origin.
  MP.postboxOrigin = "https://postbox.pigeonpost.dev";
  MP.assetUrl = function (path) {
    const p = String(path || "");
    if (/^https:\/\//.test(p)) return p;
    return /^\/v1\/marketplace\/listings\/[^/]+\/logo\/[0-9a-f]{16}\/(64|128|512)\.webp$/.test(p) ? MP.postboxOrigin + p : null;
  };

  /// Up to two letters: the first letters of the first two words, or the first two of one word.
  /// What is in brackets is left out ("Translator (TR/EN/DE)" is "Tr").
  MP.initials = function (title) {
    const words = String(title || "").replace(/\([^)]*\)/g, " ").split(/[^\p{L}\p{N}]+/u).filter(Boolean);
    if (!words.length) return "?";
    if (words.length === 1) {
      const w = Array.from(words[0]);
      return (w[0].toLocaleUpperCase() + (w[1] || "").toLocaleLowerCase());
    }
    return (Array.from(words[0])[0] + Array.from(words[1])[0]).toLocaleUpperCase();
  };

  /// FNV-1a over the id's UTF-16 units: the same tone for the same id on every client.
  MP.TONES = 24;
  // The hues a tone can take: the colour wheel without its yellow-to-lime stretch, where white
  // initials would be hard to read. Each tone is a gradient from this hue to a deeper, shifted one.
  MP.TONE_HUES = [0, 12, 24, 160, 172, 184, 196, 206, 216, 226, 236, 246, 256, 266, 276, 286, 296, 306, 316, 326, 336, 346, 148, 354];
  MP.logoTone = function (id) {
    let h = 0x811c9dc5;
    const s = String(id || "");
    for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 0x01000193) >>> 0; }
    return h % MP.TONES;
  };

  /// A listing's logo tile. `size` is the drawn size in CSS pixels (40, 48, 64, 72 or 96); the
  /// image variant is the next one up for sharp high-density screens. `alt` empty marks it
  /// decorative (the title is next to it).
  MP.logo = function (l, opts) {
    const o = opts || {};
    const size = o.size || 64;
    const alt = o.alt == null ? "" : o.alt;
    const fallback = () => {
      const tone = MP.logoTone(l && l.id);
      const tile = el("span", {
        class: `mp-logo mp-logo-default mp-tone-${tone}`, "data-size": String(size),
        "data-logo": "default", role: alt ? "img" : null, "aria-label": alt || null, "aria-hidden": alt ? null : "true",
      }, el("span", { class: "mp-logo-letters", text: MP.initials(l && l.title) }));
      // A custom property through the CSSOM, which a strict style-src policy allows.
      tile.style.setProperty("--mp-hue", String(MP.TONE_HUES[tone]));
      return tile;
    };
    const urls = l && l.logo && l.logo.urls;
    const pick = urls && (size > 64 ? urls["512"] : urls["128"]);
    const src = pick ? MP.assetUrl(pick) : null;
    if (!src) return fallback();
    const img = el("img", {
      class: "mp-logo mp-logo-image", "data-size": String(size), "data-logo": "image", src, alt,
      width: String(size), height: String(size), loading: "lazy", decoding: "async",
    });
    img.addEventListener("error", () => img.replaceWith(fallback()), { once: true });
    return img;
  };

  /// What a card says the price is.
  MP.cardPrice = function (l) {
    if (l.coming_soon) return "Coming soon";
    if (l.price_from && Number(l.price_from.amount_cents) > 0) return `${MP.money(l.price_from.amount_cents, l.price_from.currency || "USD")} per run`;
    const p = l.price;
    if (!p || Number(p.amount_minor) === 0) return "Free";
    const m = MP.money(p.amount_minor, p.currency);
    return p.term === "monthly" ? `${m}/month` : `${m} one-off`;
  };

  MP.hostingText = (l) => (l.hosting === "publisher" ? "Hosted by the publisher" : "Runs in your Nest");

  /// The publisher's name with the verified badge (a certified publisher; Pigeonpost itself is
  /// first-party).
  MP.publisherLine = function (l) {
    const p = l.publisher || {};
    const name = p.first_party ? "Pigeonpost" : p.display_name || "Unknown publisher";
    const verified = p.first_party || p.verified;
    return el("span", { class: "mp-publisher" },
      el("span", { class: "mp-publisher-name", text: name }),
      verified ? el("span", { class: "mp-verified", title: p.first_party ? "Made by Pigeonpost" : "Verified publisher" },
        el("span", { class: "mp-verified-mark", "aria-hidden": "true", text: "✓" }),
        el("span", { class: "mp-sr", text: p.first_party ? "Made by Pigeonpost" : "Verified publisher" })) : null);
  };

  /// One catalog card: a content box whose logo, title and description lead, with the publisher,
  /// category, where it runs and the price in a quiet footer. `open(listing)` shows its page (or
  /// `href` links to it); `installed` notes that this account has it; `featured` draws it larger.
  MP.card = function (l, opts) {
    const o = opts || {};
    const facts = el("span", { class: "mp-card-facts" });
    const fact = (node) => { if (facts.childNodes.length) facts.append(el("span", { class: "mp-dot", "aria-hidden": "true", text: "·" })); facts.append(node); };
    if (l.category && MP.categoryLabel(l.category)) fact(el("span", { class: "mp-card-category", "data-category": l.category, text: MP.categoryLabel(l.category) }));
    // A sample is a placeholder: where it would run is not decided yet.
    if (l.sample) fact(el("span", { class: "mp-card-sample", text: "Sample" }));
    else fact(el("span", { class: `mp-card-hosting${l.hosting === "publisher" ? " mp-card-hosting-publisher" : ""}`, "data-hosting": l.hosting || "pigeonpost", text: MP.hostingText(l) }));
    if (!l.coming_soon && o.installed) fact(el("span", { class: "mp-card-installed", text: "Installed" }));
    return el(o.tag || "li", { class: `mp-card${l.coming_soon ? " mp-card-coming" : ""}${o.featured ? " mp-card-featured" : ""}`, "data-listing": l.id },
      l.coming_soon ? el("span", { class: "mp-ribbon", "aria-hidden": "true", text: "Coming soon" }) : null,
      MP.logo(l, { size: o.featured ? 72 : 64 }),
      el(o.heading || "h5", { class: "mp-card-title" },
        o.href
          ? el("a", { class: "mp-card-link", href: o.href, "aria-label": `${l.title}${l.coming_soon ? " (coming soon)" : ""}` }, l.title)
          : el("button", { type: "button", class: "mp-card-link", "aria-label": `Details for ${l.title}${l.coming_soon ? " (coming soon)" : ""}`, onclick: () => o.open && o.open(l) }, l.title)),
      el("p", { class: "mp-card-summary", text: l.summary || "" }),
      // Publisher and price on one line, the facts under them.
      el("div", { class: "mp-card-foot" },
        MP.publisherLine(l),
        // The ribbon already says "Coming soon"; such a card has no price to show yet.
        l.coming_soon ? null : el("span", { class: "mp-card-price", text: MP.cardPrice(l) }),
        facts));
  };

  // ---- configuration schema → form ---------------------------------------------------------------
  //
  // The release's `config_schema` is one flat object of string, integer, number and boolean
  // properties (pigeonpost_core::nest_package). `writeOnly: true` marks a secret: it is never sent
  // back, so its field is always empty and says whether a value is set. Leaving a secret empty keeps
  // what is stored; "Clear" sends null.

  MP.configForm = function (schema, opts) {
    const o = opts || {};
    const prefix = o.idPrefix || "mp-cfg";
    const values = o.values || {};
    const setSecrets = new Set((o.secrets || []).filter((s) => s.set).map((s) => s.name));
    const props = (schema && schema.properties) || {};
    const required = new Set((schema && schema.required) || []);
    const names = Object.keys(props);
    const box = el("fieldset", { class: "mp-config", id: `${prefix}-fields` }, el("legend", { text: o.legend || "Settings" }));
    if (!names.length) {
      box.append(el("p", { class: "muted", text: "This service has no settings." }));
      return { node: box, empty: true, read: () => ({}) };
    }
    const readers = [];
    for (const name of names) {
      const p = props[name] || {};
      const id = `${prefix}-${name.replace(/[^A-Za-z0-9_-]/g, "_")}`;
      const label = p.title || name;
      const hint = p.description ? el("span", { class: "mp-hint", id: `${id}-hint`, text: p.description }) : null;
      const secret = p.writeOnly === true;
      const current = Object.prototype.hasOwnProperty.call(values, name) ? values[name] : p.default;
      let input;
      if (p.type === "boolean") {
        input = el("input", { type: "checkbox", id, checked: current === true, "aria-describedby": hint ? `${id}-hint` : null });
        box.append(el("label", { class: "mp-check", for: id }, input, ` ${label}`), hint);
        readers.push(() => [name, input.checked]);
        continue;
      }
      if (Array.isArray(p.enum)) {
        input = el("select", { id, required: required.has(name) && !secret, "aria-describedby": hint ? `${id}-hint` : null },
          required.has(name) ? null : el("option", { value: "", text: "—" }),
          p.enum.map((v) => el("option", { value: String(v), text: String(v), selected: current === v })));
      } else if (p.type === "integer" || p.type === "number") {
        input = el("input", {
          type: "number", id, step: p.type === "integer" ? "1" : "any", min: p.minimum, max: p.maximum,
          value: current == null ? null : String(current), required: required.has(name),
          "aria-describedby": hint ? `${id}-hint` : null,
        });
      } else {
        input = el("input", {
          type: secret ? "password" : "text", id, maxlength: p.maxLength, minlength: p.minLength,
          autocomplete: secret ? "new-password" : "off", spellcheck: "false",
          value: secret || current == null ? null : String(current),
          required: required.has(name) && !(secret && setSecrets.has(name)),
          "aria-describedby": [hint ? `${id}-hint` : null, secret ? `${id}-state` : null].filter(Boolean).join(" ") || null,
        });
      }
      box.append(el("label", { for: id }, label + (required.has(name) ? " (required)" : "")), input, hint);
      let clear = null;
      if (secret) {
        const isSet = setSecrets.has(name);
        box.append(el("span", { class: "mp-secret-state", id: `${id}-state`, "data-secret": name, text: isSet ? "Set. Leave empty to keep it." : "Not set." }));
        if (isSet && !required.has(name)) {
          clear = el("input", { type: "checkbox", id: `${id}-clear`, "data-clear": name });
          box.append(el("label", { class: "mp-check", for: `${id}-clear` }, clear, ` Clear ${label}`));
        }
      }
      readers.push(() => {
        if (secret) {
          if (clear && clear.checked) return [name, null];
          return input.value === "" ? null : [name, input.value];
        }
        if (input.value === "") return null;
        if (p.type === "integer") return [name, parseInt(input.value, 10)];
        if (p.type === "number") return [name, Number(input.value)];
        if (Array.isArray(p.enum)) {
          const hit = p.enum.find((v) => String(v) === input.value);
          return [name, hit === undefined ? input.value : hit];
        }
        return [name, input.value];
      });
    }
    return {
      node: box,
      empty: false,
      read() {
        const out = {};
        for (const r of readers) { const kv = r(); if (kv) out[kv[0]] = kv[1]; }
        return out;
      },
    };
  };
})();
