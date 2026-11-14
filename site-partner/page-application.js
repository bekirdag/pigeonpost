// Application: the publisher application as eight steps (account type → legal details → tax →
// bank → documents → identity → agreements → review & submit). Every step saves the draft on the
// postbox, so a person can stop and come back; what Pigeonpost asked for appears inline; submit
// stays off until every required step is complete (the postbox checks the same rules again).
//   GET|PUT /v1/publisher-applications/{p}/dossier      {revision, details}
//   POST    /v1/publisher-applications/{p}/documents    {revision, category, filename, content_base64}
//   GET|DELETE …/documents/{doc}                        download / remove ({revision})
//   POST    /v1/publisher-applications/{p}/kyc          {revision} → {verification_url} (Shufti Pro)
//   POST    /v1/publisher-applications/{p}/submit       {revision}
(function () {
  "use strict";
  const PP = window.PP, PPP = window.PPP, ui = PP.ui;
  const el = (...a) => PP.el(...a);

  const COUNTRIES = "AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ BL BM BN BO BQ BR BS BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ DE DJ DK DM DO DZ EC EE EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GS GT GU GW GY HK HM HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM JO JP KE KG KH KI KM KN KP KR KW KY KZ LA LB LC LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV MW MX MY MZ NA NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO RS RU RW SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL TM TN TO TR TT TV TW TZ UA UG UM US UY UZ VA VC VE VG VI VN VU WF WS YE YT ZA ZM ZW".split(" ");
  let regionNames = null;
  try { regionNames = new Intl.DisplayNames(["en"], { type: "region" }); } catch { regionNames = null; }
  const countryName = (c) => { try { return (regionNames && c && regionNames.of(c)) || c || "—"; } catch { return c || "—"; } };

  /// Exactly the postbox's `Details` (it refuses unknown fields).
  const KEYS = ["kind", "country", "legal_name", "registration_number", "tax_id", "duns", "duns_unavailable_reason", "address", "contact_name", "contact_email", "bank_country", "bank_name", "account_holder", "iban", "account_number", "swift", "currency", "public_name", "public_summary", "website", "consent", "kyc_consent"];
  PPP.cleanDetails = function (d, fallbackName) {
    const out = {};
    for (const k of KEYS) out[k] = k === "consent" || k === "kyc_consent" ? Boolean(d && d[k]) : String((d && d[k]) || "");
    if (!out.kind) out.kind = "company";
    if (!out.public_name && fallbackName) out.public_name = fallbackName;
    return out;
  };

  const LABELS = {
    legal_name: "Full legal name", registration_number: "Company registration number", tax_id: "Tax identification number",
    duns: "DUNS number", duns_unavailable_reason: "No DUNS number? Tell us why", address: "Registered or home address",
    contact_name: "Authorised contact", contact_email: "Contact email", bank_country: "Bank country", bank_name: "Bank name",
    account_holder: "Account holder", iban: "IBAN", account_number: "Account number", swift: "SWIFT / BIC", currency: "Payout currency",
    public_name: "Public name", public_summary: "About your work", website: "Website", country: "Country",
  };

  /// Documents by country: what a company uploads as registration and tax evidence.
  PPP.requirements = function (country) {
    const v = {
      TR: ["Faaliyet belgesi (activity certificate)", "Vergi levhası (tax certificate)"],
      GB: ["Certificate of incorporation (Companies House)", "HMRC tax registration evidence"],
      US: ["State incorporation or business registration certificate", "IRS EIN confirmation letter (CP 575 or 147C)"],
    };
    return v[country] || ["Business registration or current activity certificate", "Tax registration certificate or official equivalent"];
  };
  const CATEGORY_TEXT = (country) => {
    const r = PPP.requirements(country);
    return { registration: r[0], tax: r[1], bank: "Bank account evidence", authorization: "Authorisation to act for the business", additional: "Additional document" };
  };

  // ---- completeness (mirrors the postbox's checks, so the person knows before submitting) -----

  function validIban(s) {
    s = String(s || "").replace(/\s/g, "").toUpperCase();
    if (!/^[A-Z]{2}[0-9]{2}[A-Z0-9]{11,30}$/.test(s)) return false;
    let n = 0;
    for (const ch of s.slice(4) + s.slice(0, 4)) {
      const v = /\d/.test(ch) ? Number(ch) : ch.charCodeAt(0) - 55;
      n = Number(String(n) + String(v)) % 97;
    }
    return n === 1;
  }
  PPP.validIban = validIban;

  /// What a step still needs, as short sentences. Empty means the step is complete.
  PPP.missing = function (key, app) {
    const d = app.details, out = [];
    const company = d.kind === "company";
    const blank = (k) => !String(d[k] || "").trim();
    const docs = app.documents || [];
    const request = PPP.request(app);
    switch (key) {
      case "type":
        if (!["company", "individual"].includes(d.kind)) out.push("Choose company or individual");
        if (blank("country")) out.push("Choose your country");
        break;
      case "legal":
        for (const k of ["legal_name", "address", "contact_name", "contact_email", "public_name"]) if (blank(k)) out.push(LABELS[k]);
        if (!blank("contact_email") && (!d.contact_email.includes("@") || /\s/.test(d.contact_email))) out.push("A valid contact email");
        if (company && blank("registration_number")) out.push(LABELS.registration_number);
        if (!blank("website") && !/^https:\/\/[^\s/]+/.test(d.website)) out.push("Website must start with https://");
        break;
      case "tax":
        if (company && blank("tax_id")) out.push(LABELS.tax_id);
        if (!blank("duns") && !/^\d{9}$/.test(d.duns)) out.push("DUNS has nine digits");
        if (company && blank("duns") && blank("duns_unavailable_reason")) out.push("A DUNS number, or why you don't have one");
        break;
      case "bank":
        for (const k of ["bank_country", "bank_name", "account_holder", "currency"]) if (blank(k)) out.push(LABELS[k]);
        if (!blank("currency") && !/^[A-Z]{3}$/.test(d.currency)) out.push("Currency is a three-letter code such as USD or TRY");
        if (blank("iban") && (blank("account_number") || blank("swift"))) out.push("An IBAN, or an account number with SWIFT/BIC");
        if (!blank("iban") && !validIban(d.iban)) out.push("The IBAN doesn't check out");
        if (!blank("swift") && !/^[A-Za-z0-9]{8}([A-Za-z0-9]{3})?$/.test(d.swift)) out.push("SWIFT/BIC has 8 or 11 letters and digits");
        break;
      case "documents": {
        const t = CATEGORY_TEXT(d.country);
        if (company) for (const c of ["registration", "tax"]) if (!docs.some((x) => x.category === c)) out.push(t[c]);
        if (request) {
          const since = request.at ? Date.parse(request.at) / 1000 : 0;
          for (const c of request.documents) if (!docs.some((x) => x.category === c && (!since || Number(x.created_at) >= since))) out.push(`New: ${t[c] || c}`);
        }
        break;
      }
      case "identity":
        if (!company && app.kyc_status !== "verified") out.push("Verify your identity");
        break;
      case "agreements":
        if (!d.consent) out.push("Confirm the declaration");
        break;
      default: break;
    }
    return out;
  };

  const STEPS = [
    ["type", "Account type"], ["legal", "Legal details"], ["tax", "Tax"], ["bank", "Bank"],
    ["documents", "Documents"], ["identity", "Identity"], ["agreements", "Agreements"], ["review", "Review & submit"],
  ];
  PPP.APPLICATION_STEPS = STEPS;
  PPP.allMissing = (app) => STEPS.filter(([k]) => k !== "review").map(([k, t]) => [k, t, PPP.missing(k, app)]).filter(([, , m]) => m.length);

  // ---- form pieces -------------------------------------------------------------------------------

  let uid = 0;
  function field(label, input, { hint, optional, wide } = {}) {
    const id = input.id || `ppp-f-${++uid}`;
    input.id = id;
    return el("div", { class: `pp-field${wide ? " pp-field-wide" : ""}` }, el("label", { for: id }, label, optional ? el("span", { class: "pp-optional" }, " (optional)") : null), input, hint ? el("p", { class: "pp-hint" }, hint) : null);
  }
  function text(d, key, { type = "text", rows, max = 256, placeholder, autocomplete, upper, disabled } = {}) {
    const input = rows ? el("textarea", { class: "pp-input", rows, maxlength: 2000, name: key, disabled: disabled || null }) : el("input", { class: "pp-input", type, name: key, maxlength: max, placeholder: placeholder || null, autocomplete: autocomplete || "off", spellcheck: "false", disabled: disabled || null });
    input.value = d[key] || "";
    input.addEventListener("input", () => { d[key] = upper ? input.value.toUpperCase() : input.value; });
    return input;
  }
  function country(d, key) {
    const sel = el("select", { class: "pp-input", name: key }, el("option", { value: "" }, "Choose a country"),
      COUNTRIES.map((c) => [c, countryName(c)]).sort((a, b) => a[1].localeCompare(b[1])).map(([c, n]) => el("option", { value: c, selected: d[key] === c ? true : null }, n)));
    sel.addEventListener("change", () => { d[key] = sel.value; });
    return sel;
  }
  function check(d, key, label) {
    const input = el("input", { type: "checkbox", name: key, checked: d[key] ? true : null });
    input.addEventListener("change", () => { d[key] = input.checked; });
    return el("div", { class: "pp-field pp-field-check" }, el("label", {}, input, el("span", {}, label)));
  }

  // ---- the page ------------------------------------------------------------------------------------

  function requestBanner(app) {
    const r = PPP.request(app);
    if (!r) return null;
    const t = CATEGORY_TEXT(app.details.country);
    return el("section", { class: "ppp-request-card", role: "note", "aria-label": "Pigeonpost's request" },
      el("div", { class: "ppp-request-head" }, PP.icon("alert"), el("h2", {}, "Pigeonpost needs more information")),
      el("p", {}, r.note || "Please update your application."),
      r.documents.length ? el("p", { class: "pp-muted" }, "Upload: ", r.documents.map((c) => t[c] || c).join(", ")) : null,
      r.at ? el("p", { class: "pp-faint" }, `Asked ${ui.whenTime(r.at)}`) : null);
  }

  function historyBlock(app) {
    const names = { draft_saved: "Draft saved", submitted: "Submitted", needs_information: "More information requested", document_added: "Document added", document_removed: "Document removed", kyc_started: "Identity check started", kyc_result: "Identity check result", in_review: "Review started", approved: "Approved", rejected: "Rejected", revoked: "Revoked", created: "Started" };
    const rows = (app.history || []).filter((h) => h.event !== "draft_saved");
    if (!rows.length) return null;
    return ui.section({ title: "History" }, el("ol", { class: "ppp-history" }, rows.slice(0, 30).map((h) =>
      el("li", {}, el("strong", {}, names[h.event] || String(h.event).replace(/_/g, " ")), el("span", { class: "pp-faint" }, ` · ${ui.whenTime(h.created_at)}`), h.note ? el("p", { class: "pp-muted" }, h.note) : null))));
  }

  function documentsList(ctx, app, editable, onChange) {
    const t = CATEGORY_TEXT(app.details.country);
    const base = PPP.appBase(ctx);
    return ui.table({
      caption: "Uploaded documents",
      empty: { icon: "docs", title: "No documents yet", text: editable ? "Upload them below. PDF, PNG or JPEG, up to 5 MB each." : "Nothing was uploaded." },
      columns: [
        { key: "category", label: "Document", render: (r) => t[r.category] || r.category },
        { key: "filename", label: "File", render: (r) => el("span", { translate: "no" }, r.filename) },
        { key: "size", label: "Size", num: true, render: (r) => `${Math.max(1, Math.ceil(Number(r.size || 0) / 1024))} KB` },
        { key: "created_at", label: "Added", render: (r) => ui.when(r.created_at) },
        { key: "actions", label: "", render: (r) => el("span", { class: "ppp-row-actions" },
          ui.button("Download", { size: "sm", kind: "quiet", onclick: async () => {
            const res = await PP.postbox(`${base}/documents/${PPP.enc(r.id)}`);
            PPP.saveBase64(res.data.filename, res.data.content_base64, r.mime);
          } }),
          editable ? ui.button("Remove", { size: "sm", kind: "quiet", onclick: async () => {
            if (!(await ui.confirm({ title: "Remove this document?", text: r.filename, confirm: "Remove", danger: true }))) return;
            const res = await PP.postbox(`${base}/documents/${PPP.enc(r.id)}`, { method: "DELETE", body: { revision: ctx.app.revision } });
            PPP.setApp(ctx, res.data);
            onChange();
          } }) : null) },
      ],
      rows: app.documents || [],
    });
  }

  /// The submitted application, read-only: status, history and what was sent.
  function lockedView(view, ctx) {
    const app = ctx.app, d = app.details, state = PPP.stateOf(ctx), s = PPP.STATES[state];
    view.append(ui.header({ title: "Application", text: s.hint, actions: [PPP.stateBadge(state), ui.button("Refresh", { icon: "audit", onclick: () => PP.shell.reload() })] }));
    if (state === "approved") view.append(ui.gate({ tone: "success", text: "Your business is approved. App releases still go through their own review.", action: { label: "Go to your apps", href: PP.href("apps") } }));
    else if (["submitted", "in_review"].includes(state)) view.append(ui.gate({ tone: "info", text: "Your application is locked while Pigeonpost reviews it. Any request for more information will appear here and on the overview.", action: { label: "Ask a question", href: PP.href("support") } }));
    else if (state === "rejected" || state === "revoked") view.append(ui.gate({ tone: "danger", text: ctx.pub.decision_reason ? `Reason: ${ctx.pub.decision_reason}` : "Contact support to talk about the decision.", action: { label: "Contact support", href: PP.href("support") } }));
    view.append(el("div", { class: "pp-cols" },
      ui.card({ title: "What you sent" }, ui.kv([
        ["Applicant", `${d.kind === "company" ? "Company" : "Individual"} · ${countryName(d.country)}`],
        ["Legal name", d.legal_name], d.kind === "company" ? ["Registration", d.registration_number] : null, ["Tax ID", d.tax_id], d.duns ? ["DUNS", d.duns] : null,
        ["Address", d.address], ["Contact", d.contact_name ? `${d.contact_name} · ${d.contact_email}` : ""],
        ["Bank", [d.bank_name, d.account_holder, countryName(d.bank_country)].filter(Boolean).join(" · ")], ["Payout currency", d.currency],
        ["Public name", d.public_name], ["Identity", d.kind === "individual" ? String(app.kyc_status || "").replace(/_/g, " ") : "Verified from documents"],
        ["Submitted", ui.whenTime(app.submitted_at)],
      ])),
      ui.card({ title: "Documents" }, documentsList(ctx, app, false, () => {}))));
    const h = historyBlock(app);
    if (h) view.append(h);
  }

  function stepBody(key, ctx, app, redraw) {
    const d = app.details, company = d.kind === "company";
    const form = el("div", { class: "pp-form ppp-step-form" });
    const add = (...n) => form.append(...n);
    switch (key) {
      case "type": {
        const opt = (value, title, textv, icon) => {
          const input = el("input", { type: "radio", name: "ppp-kind", value, checked: d.kind === value ? true : null });
          input.addEventListener("change", () => { d.kind = value; redraw(); });
          return el("label", { class: `ppp-choice${d.kind === value ? " is-on" : ""}` }, input, el("span", { class: "ppp-choice-icon" }, PP.icon(icon)), el("span", {}, el("strong", {}, title), el("span", { class: "pp-muted" }, textv)));
        };
        add(el("fieldset", { class: "ppp-choices pp-field-wide" }, el("legend", {}, "Who is applying?"),
          opt("company", "A company or legal entity", "Registered business: we check registration and tax documents (KYB).", "building"),
          opt("individual", "An individual developer", "You publish in your own name: you verify your identity online (KYC).", "people")),
          field(company ? "Country of registration" : "Country of residence", country(d, "country"), { hint: "This decides which documents we ask for and how payouts are taxed." }));
        break;
      }
      case "legal":
        add(field(company ? "Registered company name" : LABELS.legal_name, text(d, "legal_name", { autocomplete: company ? "organization" : "name" }), { hint: company ? "Exactly as on your registration certificate." : "Exactly as on your ID." }));
        if (company) add(field(LABELS.registration_number, text(d, "registration_number"), { hint: "Ticaret sicil / MERSIS no., Companies House number, state file number…" }));
        add(field(LABELS.address, text(d, "address", { rows: 3 }), { wide: true }),
          field(LABELS.contact_name, text(d, "contact_name", { autocomplete: "name" }), { hint: "The person Pigeonpost talks to about this application." }),
          field(LABELS.contact_email, text(d, "contact_email", { type: "email", autocomplete: "email" })),
          el("h3", { class: "pp-form-heading" }, "How buyers see you"),
          field(LABELS.public_name, text(d, "public_name"), { hint: "Shown next to your apps after approval." }),
          field(LABELS.website, text(d, "website", { type: "url", placeholder: "https://" }), { optional: true }),
          field(LABELS.public_summary, text(d, "public_summary", { rows: 3 }), { optional: true, wide: true, hint: "One or two sentences for your public partner page." }));
        break;
      case "tax":
        add(field(company ? LABELS.tax_id : "Tax identification number", text(d, "tax_id"), { optional: !company, hint: company ? "VKN, EIN, VAT or UTR — the number on your tax certificate." : "TCKN, SSN/ITIN or your national tax number. It sets the withholding on your payouts." }));
        if (company) {
          add(field(LABELS.duns, text(d, "duns", { max: 9, placeholder: "123456789" }), { optional: true, hint: "Nine digits. Free from Dun & Bradstreet." }),
            field(LABELS.duns_unavailable_reason, text(d, "duns_unavailable_reason", { rows: 2 }), { optional: true, wide: true, hint: "Only if you leave the DUNS number empty." }));
        }
        add(el("p", { class: "pp-hint pp-field-wide" }, "Withholding (stopaj) on payouts depends on your country and whether you are a company or an individual. Your monthly statement shows it as its own line."));
        break;
      case "bank":
        add(el("p", { class: "pp-muted pp-field-wide" }, "Where Pigeonpost pays your earnings each month. Bank details are private and checked by staff."),
          field(LABELS.bank_country, country(d, "bank_country")),
          field(LABELS.bank_name, text(d, "bank_name")),
          field(LABELS.account_holder, text(d, "account_holder"), { hint: "Must match your legal name." }),
          field(LABELS.iban, text(d, "iban", { max: 42, upper: true, placeholder: "TR00 0000 0000 0000 0000 0000 00" }), { optional: true, hint: "Or leave empty and give the account number and SWIFT/BIC." }),
          field(LABELS.account_number, text(d, "account_number"), { optional: true }),
          field(LABELS.swift, text(d, "swift", { max: 11, upper: true }), { optional: true }),
          field(LABELS.currency, text(d, "currency", { max: 3, upper: true, placeholder: "USD" }), { hint: "USD, EUR or TRY. Turkish payees can be paid in TRY." }));
        break;
      case "documents": {
        const req = PPP.requirements(d.country);
        const t = CATEGORY_TEXT(d.country);
        add(el("div", { class: "pp-field-wide ppp-doc-intro" },
          company ? el("p", {}, `For ${countryName(d.country || "TR")}, upload: `, el("strong", {}, req[0]), " and ", el("strong", {}, req[1]), ". Staff may ask for an equivalent or more.") : el("p", {}, "Individuals don't upload ID here: identity is checked in the next step. Add a document only if Pigeonpost asks for one."),
          el("p", { class: "pp-hint" }, "PDF, PNG or JPEG, up to 5 MB each, at most 12 documents. Documents are stored encrypted and seen only by Pigeonpost reviewers.")));
        add(el("div", { class: "pp-field-wide" }, documentsList(ctx, app, true, redraw)));
        const cat = el("select", { class: "pp-input", "aria-label": "Document type" }, ["registration", "tax", "bank", "authorization", "additional"].map((c) => el("option", { value: c }, t[c])));
        const status = el("p", { class: "pp-form-status", role: "status", "aria-live": "polite" });
        add(field("Document type", cat), el("div", { class: "pp-field pp-field-wide" }, ui.dropzone({
          label: "Drop a PDF, PNG or JPEG here, or choose a file", accept: ".pdf,.png,.jpg,.jpeg,application/pdf,image/png,image/jpeg",
          onFiles: async (files) => {
            const f = files[0];
            status.className = "pp-form-status";
            if (!/\.(pdf|png|jpe?g)$/i.test(f.name)) { status.textContent = "Upload a PDF, PNG or JPEG."; status.classList.add("is-error"); return; }
            if (f.size > 5 * 1024 * 1024) { status.textContent = "That file is over 5 MB."; status.classList.add("is-error"); return; }
            if ((app.documents || []).length >= 12) { status.textContent = "At most 12 documents: remove one first."; status.classList.add("is-error"); return; }
            status.textContent = "Uploading…";
            try {
              await save(ctx, app);
              const r = await PP.postbox(`${PPP.appBase(ctx)}/documents`, { method: "POST", body: { revision: ctx.app.revision, category: cat.value, filename: f.name.slice(-120), content_base64: await PPP.fileBase64(f) } });
              PPP.setApp(ctx, r.data);
              PP.toast("Document uploaded", "success");
              redraw();
            } catch (e) { if (!(e instanceof PP.Redirecting)) { status.textContent = PP.explain(e); status.classList.add("is-error"); } }
          },
        }), status));
        break;
      }
      case "identity":
        if (company) {
          add(el("div", { class: "pp-field-wide" }, ui.card({ title: "Business verification (KYB)", tone: "accent" },
            el("p", {}, "Pigeonpost verifies your company from its registration and tax documents and the details you entered. Nothing else is needed here."),
            el("p", { class: "pp-muted" }, `If staff need to confirm who may act for ${d.legal_name || "the company"}, they will ask for an authorisation document.`))));
        } else {
          const off = PP.store.get("pp_partner_kyc_off") === "1";
          const kyc = app.kyc_status || "not_started";
          const tone = { verified: "success", pending: "info", review_required: "warning", declined: "danger", not_started: "neutral" }[kyc] || "neutral";
          add(el("div", { class: "pp-field-wide ppp-kyc" },
            el("div", { class: "ppp-kyc-head" }, el("h3", {}, "Identity check (KYC)"), ui.badge(kyc.replace(/_/g, " "), tone)),
            el("p", {}, "Individuals verify a government ID and a live selfie with Shufti Pro, our verification partner. Your ID images and face data are handled by Shufti Pro, not uploaded to Pigeonpost."),
            check(d, "kyc_consent", "I agree that Pigeonpost sends my legal name and country to Shufti Pro to verify my identity."),
            off && kyc !== "verified"
              ? ui.gate({ tone: "info", text: "The identity check opens when Pigeonpost enables it. Your draft is saved: finish the other steps now, and come back here to verify." })
              : null,
            el("div", { class: "pp-form-actions ppp-left" },
              kyc !== "verified" ? ui.button(kyc === "pending" ? "Continue identity check" : "Start identity check", { kind: off ? "default" : "primary", icon: "security", onclick: async () => {
                if (!d.kyc_consent) { PP.toast("Tick the consent box first.", "error"); return; }
                if (!d.legal_name || !d.country) { PP.toast("Fill in your legal name and country first.", "error"); return; }
                await save(ctx, app);
                try {
                  const r = await PP.postbox(`${PPP.appBase(ctx)}/kyc`, { method: "POST", body: { revision: ctx.app.revision } });
                  const u = new URL(r.data.verification_url);
                  if (u.protocol !== "https:" || !(u.hostname === "shuftipro.com" || u.hostname.endsWith(".shuftipro.com"))) throw new Error("The verification link wasn't from Shufti Pro, so it wasn't opened.");
                  PP.store.del("pp_partner_kyc_off");
                  PP.navigate(u.href);
                } catch (e) {
                  if (e && e.code === "kyc_not_configured") { PP.store.set("pp_partner_kyc_off", "1"); redraw(); return; }
                  throw e;
                }
              } }) : null,
              ui.button("Refresh status", { kind: "quiet", onclick: async () => { await PP.shell.reload(); } }))));
        }
        break;
      case "agreements":
        add(el("div", { class: "pp-field-wide ppp-terms" },
          el("p", {}, "By submitting you confirm:"),
          el("ul", {}, el("li", {}, "you may apply for this business, and the information is accurate;"), el("li", {}, "Pigeonpost may verify it and review your apps before they are listed;"), el("li", {}, "Piyote sells your services as merchant of record and keeps 10% of each sale; earnings are paid monthly against your invoice.")),
          el("p", {}, el("a", { href: "https://pigeonpost.dev/app-terms.html", target: "_blank", rel: "noopener" }, "Terms of use"), " · ", el("a", { href: "https://pigeonpost.dev/app-privacy.html", target: "_blank", rel: "noopener" }, "Privacy policy"))),
          check(d, "consent", "I am authorised to apply, the information is accurate, and I agree to verification and review under the terms and privacy policy."));
        break;
      case "review": {
        const groups = [
          ["type", "Account type", [["Applicant", d.kind === "company" ? "Company" : "Individual"], ["Country", countryName(d.country)]]],
          ["legal", "Legal details", [["Legal name", d.legal_name], company ? ["Registration", d.registration_number] : null, ["Address", d.address], ["Contact", d.contact_name ? `${d.contact_name} · ${d.contact_email}` : ""], ["Public name", d.public_name]]],
          ["tax", "Tax", [["Tax ID", d.tax_id], company ? ["DUNS", d.duns || (d.duns_unavailable_reason ? "None — reason given" : "")] : null]],
          ["bank", "Bank", [["Bank", [d.bank_name, countryName(d.bank_country)].filter(Boolean).join(" · ")], ["Holder", d.account_holder], ["Account", d.iban ? `IBAN …${d.iban.replace(/\s/g, "").slice(-4)}` : d.account_number ? `…${d.account_number.slice(-4)} · ${d.swift}` : ""], ["Currency", d.currency]]],
          ["documents", "Documents", [["Uploaded", `${(app.documents || []).length} document${(app.documents || []).length === 1 ? "" : "s"}`]]],
          ["identity", "Identity", [["Check", company ? "From documents (KYB)" : String(app.kyc_status || "not started").replace(/_/g, " ")]]],
          ["agreements", "Agreements", [["Declaration", d.consent ? "Confirmed" : "Not yet"]]],
        ];
        add(el("div", { class: "pp-field-wide ppp-review" }, groups.map(([k, title, pairs]) => {
          const miss = PPP.missing(k, app);
          return el("section", { class: `ppp-review-group${miss.length ? " is-missing" : ""}` },
            el("div", { class: "ppp-review-head" }, el("h3", {}, title), miss.length ? ui.badge("Incomplete", "warning") : ui.badge("Complete", "success"),
              el("a", { class: "ppp-edit-link", href: PP.href("application", k) }, "Edit")),
            ui.kv(pairs), miss.length ? el("p", { class: "ppp-missing" }, `Still needed: ${miss.join(", ")}`) : null);
        })));
        break;
      }
      default: break;
    }
    return form;
  }

  async function save(ctx, app) {
    const r = await PP.postbox(`${PPP.appBase(ctx)}/dossier`, { method: "PUT", body: { revision: ctx.app.revision, details: PPP.cleanDetails(app.details) } });
    PPP.setApp(ctx, r.data);
    app.revision = ctx.app.revision;
    app.documents = ctx.app.documents;
    app.kyc_status = ctx.app.kyc_status;
    app.history = ctx.app.history;
    return r.data;
  }

  function editor(view, ctx, params) {
    // A working copy: what the person types stays here until it is saved.
    const app = { ...ctx.app, details: PPP.cleanDetails(ctx.app.details, ctx.pub.display_name) };
    const keys = STEPS.map(([k]) => k);
    const firstOpen = () => (PPP.allMissing(app)[0] || ["review"])[0];
    let current = keys.includes(params[0]) ? params[0] : firstOpen();
    const holder = el("div", { class: "ppp-app" });
    view.append(ui.header({ eyebrow: ctx.pub.display_name, title: "Your application", text: "Eight short steps. Every step is saved, so you can stop and come back at any time.", actions: PPP.stateBadge(PPP.stateOf(ctx)) }));
    const banner = requestBanner(ctx.app);
    if (banner) view.append(banner);
    view.append(holder);

    const go = async (key, saveFirst = true) => {
      if (saveFirst) await save(ctx, app);
      current = key;
      window.history.replaceState(null, "", PP.href("application", key));
      draw();
      const h = holder.querySelector("h2");
      if (h) { h.setAttribute("tabindex", "-1"); h.focus({ preventScroll: true }); }
    };

    function draw() {
      // Documents, revision and the identity check are the server's; the details are the draft.
      app.documents = ctx.app.documents; app.revision = ctx.app.revision; app.kyc_status = ctx.app.kyc_status;
      const idx = keys.indexOf(current);
      const steps = STEPS.map(([k, t]) => ({ key: k, title: t, done: k === "review" ? false : PPP.missing(k, app).length === 0 }));
      const status = el("p", { class: "pp-form-status", role: "status", "aria-live": "polite" });
      const run = (fn) => async () => {
        status.className = "pp-form-status"; status.textContent = "";
        try { await fn(); } catch (e) { if (!(e instanceof PP.Redirecting)) { status.textContent = PP.explain(e); status.classList.add("is-error"); } }
      };
      const missingHere = current === "review" ? [] : PPP.missing(current, app);
      const all = PPP.allMissing(app);
      const actions = el("div", { class: "pp-form-actions ppp-step-actions" },
        idx > 0 ? ui.button("Back", { onclick: run(() => go(keys[idx - 1])) }) : null,
        current !== "review" ? ui.button("Save draft", { kind: "quiet", onclick: run(async () => { await save(ctx, app); status.textContent = "Draft saved."; status.classList.add("is-ok"); }) }) : null,
        current !== "review"
          ? ui.button("Save and continue", { kind: "primary", icon: "chevron", onclick: run(() => go(keys[idx + 1])) })
          : ui.button("Submit for review", { kind: "primary", icon: "check", disabled: all.length > 0, onclick: run(async () => {
            await save(ctx, app);
            const r = await PP.postbox(`${PPP.appBase(ctx)}/submit`, { method: "POST", body: { revision: ctx.app.revision } });
            PPP.setApp(ctx, r.data);
            PP.toast("Application submitted. Updates will appear here.", "success");
            await PP.shell.reload();
          }) }));
      holder.replaceChildren(
        el("div", { class: "ppp-stepper-wrap" }, ui.stepper(steps, current, (k) => run(() => go(k))())),
        ui.card({ cls: "ppp-step-card" },
          el("div", { class: "ppp-step-head" }, el("p", { class: "pp-eyebrow" }, `Step ${idx + 1} of ${keys.length}`), el("h2", {}, STEPS[idx][1])),
          missingHere.length ? el("p", { class: "ppp-missing" }, `To complete this step: ${missingHere.join(", ")}.`) : null,
          current === "review" && all.length ? el("p", { class: "ppp-missing", id: "ppp-submit-blocked" }, `Submit opens when every step is complete. Still to do: ${all.map(([, t]) => t).join(", ")}.`) : null,
          stepBody(current, ctx, app, draw), actions, status));
      // On a phone the steps scroll sideways: keep the current one in view.
      window.setTimeout(() => {
        const on = holder.querySelector(".pp-step.is-current");
        const strip = holder.querySelector(".ppp-stepper-wrap");
        if (on && strip && strip.scrollWidth > strip.clientWidth) strip.scrollLeft = Math.max(0, strip.scrollLeft + on.getBoundingClientRect().left - strip.getBoundingClientRect().left - 16);
      }, 0);
    }
    draw();
    const h = historyBlock(ctx.app);
    if (h) view.append(h);
  }

  PPP.page({
    key: "application", title: "Application", group: "", icon: "form", order: 1,
    count: (ctx) => (PPP.stateOf(ctx) === "needs_information" ? 1 : null),
    async render(view, ctx, params) {
      if (ctx.disabled || !ctx.pub) {
        view.append(ui.header({ title: "Application", text: "Apply once to publish apps and services on Pigeonpost." }));
        view.append(ctx.disabled ? ui.empty({ icon: "info", title: "Publishing isn't open on this account yet" }) : ui.card({ title: "Start your application" }, PPP.startForm(ctx)));
        return;
      }
      if (!ctx.app) {
        view.append(ui.header({ title: "Application" }), ui.errorState(ctx.appError || new Error("Your application could not be loaded."), () => PP.shell.reload()));
        return;
      }
      if (PPP.editable(ctx)) editor(view, ctx, params);
      else lockedView(view, ctx);
    },
  });
})();
