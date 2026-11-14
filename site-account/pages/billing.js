// Billing details (who every web purchase is invoiced to; individual: TCKN for Turkish addresses,
// company: VKN / EU-UK VAT) and Invoices (every web purchase, with its PDF).
//
// Bridge: POST /v1/billing/profiles, PATCH /v1/billing/profiles/{id}, GET /v1/me/overview,
//         GET /v1/billing/invoices/{id}/download.
(function () {
  "use strict";
  const el = (...a) => PP.el(...a);
  const turkish = (country) => {
    const c = String(country || "").replace(/[ıİ]/g, "i").normalize("NFKD").replace(/[̀-ͯ]/g, "").toLowerCase().replace(/[^a-z0-9]/g, "");
    return !c || ["tr", "tur", "turkey", "turkiye"].includes(c);
  };

  /// The billing form. Which fields show and which are required follow the account type and the
  /// country, as checkout and saas_be expect them.
  ACC.billingForm = function (existing, { onSaved, onCancel } = {}) {
    const v = existing ? { ...existing, ...(existing.address || {}) } : {};
    const inputs = {};
    const fields = {};
    const mk = (name, label, attrs = {}, opts = {}) => {
      const input = attrs.tag === "select"
        ? ACC.select(attrs.options, v[name] || attrs.options[0][0], { name })
        : el("input", { name, type: attrs.type || "text", autocomplete: attrs.autocomplete || null, inputmode: attrs.inputmode || null, maxlength: attrs.maxlength || null, placeholder: attrs.placeholder || null });
      if (attrs.tag !== "select" && v[name] != null && typeof v[name] !== "object") input.value = v[name];
      inputs[name] = input;
      const f = ACC.field(label, input, opts);
      const err = el("p", { class: "pp-field-error", hidden: true });
      f.append(err);
      f.err = err;
      f.labelNode = f.querySelector("label");
      fields[name] = f;
      return f;
    };
    const form = el("form", { class: "pp-form", novalidate: true, id: "acc-billing-form" },
      mk("account_type", "Account type", { tag: "select", options: [["individual", "Individual"], ["entity", "Company"]] }),
      mk("legal_name", "Legal name", { autocomplete: "name" }),
      mk("billing_email", "Billing email", { type: "email", autocomplete: "email" }),
      mk("phone", "Phone", { type: "tel", autocomplete: "tel", placeholder: "+90…" }, { optional: true }),
      mk("line1", "Address", { autocomplete: "address-line1" }),
      mk("line2", "Address line 2", { autocomplete: "address-line2" }, { optional: true }),
      mk("city", "City", { autocomplete: "address-level2" }),
      mk("state", "District (İlçe)", { autocomplete: "address-level3" }),
      mk("postal_code", "Postal code", { autocomplete: "postal-code" }),
      mk("country", "Country", { autocomplete: "country-name", placeholder: "Türkiye / TR" }),
      mk("identity_number", "TCKN (Turkish identity number)", { inputmode: "numeric", maxlength: "11" }, { hint: "Required for billing addresses in Türkiye." }),
      mk("entity_name", "Company name", { autocomplete: "organization" }),
      mk("tax_id", "Tax ID / VAT (VKN in Türkiye)", {}, { hint: "EU or UK company: the VAT number with its country prefix, like DE123456789. We check it with VIES or HMRC and sell without VAT (reverse charge)." }),
      mk("tax_office", "Tax office", {}, { optional: true }));
    const status = el("p", { class: "pp-form-status", role: "status", "aria-live": "polite" });
    const save = ACC.btn("Save billing details", { kind: "primary", type: "submit", id: "acc-billing-save" });
    form.append(el("div", { class: "pp-form-actions" }, onCancel ? ACC.btn("Cancel", { onclick: onCancel }) : null, save), status);

    const rules = () => {
      const entity = inputs.account_type.value === "entity";
      const tr = turkish(inputs.country.value);
      for (const n of ["entity_name", "tax_id", "tax_office"]) { fields[n].hidden = !entity; inputs[n].disabled = !entity; }
      fields.identity_number.hidden = entity; inputs.identity_number.disabled = entity;
      fields.state.labelNode.firstChild.textContent = tr ? "District (İlçe)" : "State / region";
      return {
        required: ["legal_name", "billing_email", "line1", "city", "postal_code", "country", ...(tr ? ["state"] : []), ...(tr && !entity ? ["identity_number"] : []), ...(tr && entity ? ["tax_id"] : [])],
        patterns: { identity_number: [/^[0-9]{11}$/, "Enter your 11 digit TCKN."], ...(tr ? { tax_id: [/^[0-9]{10,11}$/, "Enter a 10 digit VKN or 11 digit TCKN."] } : {}), billing_email: [/^[^\s@]+@[^\s@]+\.[^\s@]+$/, "Enter an email address."] },
        entity,
      };
    };
    inputs.account_type.addEventListener("change", rules);
    inputs.country.addEventListener("input", rules);
    rules();

    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      for (const i of Object.values(inputs)) if (i.tagName === "INPUT") i.value = i.value.trim();
      const r = rules();
      let first = null;
      for (const [name, f] of Object.entries(fields)) {
        const i = inputs[name];
        let msg = "";
        if (!i.disabled) {
          if (r.required.includes(name) && !i.value) msg = `${f.labelNode.firstChild.textContent} is required.`;
          else if (i.value && r.patterns[name] && !r.patterns[name][0].test(i.value)) msg = r.patterns[name][1];
        }
        f.err.textContent = msg; f.err.hidden = !msg;
        i.toggleAttribute("aria-invalid", Boolean(msg));
        if (msg && !first) first = i;
      }
      if (first) { first.focus(); return; }
      const val = (n) => inputs[n].value;
      const payload = { account_type: val("account_type"), legal_name: val("legal_name"), billing_email: val("billing_email"), phone: val("phone"),
        address: { line1: val("line1"), line2: val("line2") || null, city: val("city"), state: val("state") || null, postal_code: val("postal_code"), country: val("country") } };
      if (r.entity) Object.assign(payload, { entity_name: val("entity_name"), tax_id: val("tax_id"), tax_office: val("tax_office"), identity_number: null });
      else Object.assign(payload, { identity_number: val("identity_number") || null, entity_name: null, tax_id: null, tax_office: null });
      save.disabled = true;
      status.className = "pp-form-status";
      status.textContent = "Saving…";
      try {
        await ACC.bridgeAction(existing && existing.id ? `/v1/billing/profiles/${encodeURIComponent(existing.id)}` : "/v1/billing/profiles", payload, existing && existing.id ? "PATCH" : "POST");
        status.textContent = "";
        ACC.forget("overview");
        if (onSaved) await onSaved();
      } catch (err) {
        status.className = "pp-form-status is-error";
        status.textContent = "Could not save: " + PP.explain(err);
      } finally { if (save.isConnected) save.disabled = false; }
    });
    form.inputs = inputs;
    return form;
  };

  ACC.page({
    key: "billing", title: "Billing details", group: "Billing", icon: "billing",
    async render(view, ctx, params) {
      const ov = await ACC.overview(true);
      const p = ov.billing.profiles[0] || null;
      const editing = !p || params[0] === "edit";
      view.append(PP.ui.header({ title: "Billing details", text: "Who we invoice. Plans, handles, Nests, services and credits all use these details." }));
      if (ov.billing.incomplete) view.append(PP.ui.gate({ text: ACC.gateText(ov), tone: "warning" }));
      const saved = async () => {
        PP.toast("Billing details saved.", "success");
        const pending = PP.durable.get("pp_pending_handle");
        PP.go(pending ? "handles" : "billing", pending ? "get" : "");
        if (!pending) PP.shell.refresh();
      };
      if (editing) {
        view.append(PP.ui.card({ title: p ? "Edit billing details" : "Add billing details", text: p ? null : "You need these before buying anything on the web. App Store and Google Play purchases don't." },
          ACC.billingForm(p, { onSaved: saved, onCancel: p ? () => PP.go("billing") : null })));
        return;
      }
      const a = p.address || {};
      const entity = p.account_type === "entity";
      view.append(el("div", { class: "pp-cols" },
        PP.ui.card({ title: p.legal_name || p.legalName || "Billing details", text: entity ? "Company" : "Individual", actions: [ov.billing.has ? PP.ui.badge("Complete", "success") : PP.ui.badge("Incomplete", "warning"), ACC.btn("Edit", { href: PP.href("billing", "edit"), id: "acc-billing-edit" })] },
          PP.ui.kv([
            ["Billing email", p.billing_email || p.billingEmail],
            ["Phone", p.phone],
            ["Address", [a.line1, a.line2, [a.postal_code, a.city].filter(Boolean).join(" "), a.state, a.country].filter(Boolean).join(", ")],
            entity ? ["Company", p.entity_name] : null,
            entity ? ["Tax ID", [p.tax_id, p.tax_office].filter(Boolean).join(" · ")] : ["TCKN", p.identity_number ? "On file" : null],
          ])),
        PP.ui.card({ title: "How billing works" }, el("ul", { class: "acc-ticks" },
          ["Sold by Piyote Bilişim; payments go through iyzico", "Your card details go to iyzico, never to us", "EU and UK businesses buy without VAT with a verified VAT number", "Every web purchase gets an invoice in Invoices"].map((t) => el("li", {}, PP.icon("check"), t))))));
    },
  });

  ACC.page({
    key: "invoices", title: "Invoices", group: "Billing", icon: "invoice",
    async render(view) {
      const ov = await ACC.overview(true);
      view.append(PP.ui.header({ title: "Invoices", text: "Every web purchase, with a PDF to download." }),
        PP.ui.table({
          caption: "Invoices",
          search: ov.invoices.length > 10 ? "Search invoices" : null,
          sort: { key: "date", dir: "desc" },
          columns: [
            { key: "date", label: "Date", sort: (i) => String(i.issued_at || i.created_at || ""), render: (i) => ACC.date(i.issued_at || i.created_at) },
            { key: "number", label: "Number", mono: true },
            { key: "status", label: "Status", render: (i) => PP.ui.status(String(i.status || "").toLowerCase(), { paid: ["Paid", "success"], issued: ["Issued", "info"], void: ["Void", "neutral"], refunded: ["Refunded", "warning"] }) },
            { key: "total", label: "Total", num: true, render: (i) => (i.total != null ? ACC.money(i.total, i.currency || "USD") : i.amount != null ? ACC.money(i.amount, i.currency || "USD") : "—") },
            { key: "pdf", label: "", render: (i) => (i.id ? ACC.btn("PDF", { size: "sm", icon: "docs", attrs: { "data-invoice": i.id }, onclick: () => ACC.openInvoice(i.id) }) : null) },
          ],
          rows: ov.invoices,
          empty: { icon: "invoice", title: "No invoices yet", text: "Invoices for web purchases appear here. App Store and Google Play send their own receipts." },
        }));
    },
  });
})();
