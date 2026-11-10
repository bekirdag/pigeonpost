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
