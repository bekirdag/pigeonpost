// Verified publisher registration on the account page (developer program plan §2–§3): choose an
// account type, fill the application (saved as a draft), upload the documents its type needs,
// verify identity with Shufti Pro (a sandbox until the account is live), accept the agreements
// and submit with two-step sign-in. Then a status page with the history, and a reply when
// Pigeonpost asks for more information.
//
// Calls (all through deps.pb; uploads and downloads through deps.pbRaw):
//   GET  /v1/publisher-registration/requirements           account types, documents, agreement versions
//   GET|POST /v1/publisher-registrations                     {account_type, org_id?, public_name?}
//   GET|PATCH /v1/publisher-registrations/{id}               {public_name, profile, bank, tax, accept_agreements, revision}
//   POST /v1/publisher-registrations/{id}/documents?type=&filename=&issued_on=&expires_on=   raw file
//   GET|DELETE /v1/publisher-registrations/{id}/documents/{doc}
//   POST /v1/publisher-registrations/{id}/kyc                → {verification_url, sandbox}
//   POST /v1/publisher-registrations/{id}/kyc/{check}/simulate {outcome}   sandbox only
//   POST /v1/publisher-registrations/{id}/submit             {message?} (step-up)
//
// Bank and tax details are never shown back unmasked: the form shows what is saved and offers to
// replace it.
(function () {
  "use strict";

  const MP = (window.PPMarketplace = window.PPMarketplace || {});
  const el = (...a) => MP.el(...a);
  const TR = /^tr\b/i.test((window.navigator && window.navigator.language) || "");

  // English and Turkish. Keys are short; `t(key)` falls back to English.
  const S = {
    title: ["Become a verified publisher", "Doğrulanmış yayıncı olun"],
    intro: [
      "Pigeonpost verifies every publisher before they can sell, like an app store developer account. You need your legal details, a bank account in your legal name, your tax details, the documents for your account type and an identity check.",
      "Pigeonpost, satış yapabilmeleri için her yayıncıyı bir uygulama mağazası geliştirici hesabı gibi doğrular. Yasal bilgileriniz, adınıza açılmış bir banka hesabı, vergi bilgileriniz, hesap türünüze uygun belgeler ve kimlik doğrulaması gerekir.",
    ],
    type: ["Account type", "Hesap türü"],
    publishAs: ["Publish as", "Yayıncı"],
    myAccount: ["My own account", "Kendi hesabım"],
    publicName: ["Publisher name buyers will see", "Alıcıların göreceği yayıncı adı"],
    start: ["Start application", "Başvuruyu başlat"],
    steps: [["Details", "Payout & tax", "Documents", "Identity", "Review & submit"], ["Bilgiler", "Ödeme ve vergi", "Belgeler", "Kimlik", "Kontrol ve gönder"]],
    save: ["Save draft", "Taslağı kaydet"],
    saved: ["Draft saved.", "Taslak kaydedildi."],
    next: ["Save and continue", "Kaydet ve devam et"],
    back: ["Back", "Geri"],
    legalName: ["Legal name (as on your ID or registry)", "Yasal ad (kimlikte veya sicilde yazdığı gibi)"],
    tradingName: ["Trading name (optional)", "Ticari ad (isteğe bağlı)"],
    country: ["Country", "Ülke"],
    line1: ["Address", "Adres"],
    line2: ["Address line 2 (optional)", "Adres satırı 2 (isteğe bağlı)"],
    city: ["City", "Şehir"],
    postal: ["Postal code", "Posta kodu"],
    website: ["Website (https://)", "Web sitesi (https://)"],
    supportEmail: ["Support email buyers can use", "Alıcıların kullanacağı destek e-postası"],
    supportAddress: ["Support Pigeonpost address (optional)", "Destek Pigeonpost adresi (isteğe bağlı)"],
    contactEmail: ["Email we write to about this application (private)", "Bu başvuru için size yazacağımız e-posta (gizli)"],
    mersis: ["MERSİS number", "MERSİS numarası"],
    tradeRegistry: ["Trade registry number", "Ticaret sicil numarası"],
    registration: ["Company registration number", "Şirket sicil numarası"],
    duns: ["D-U-N-S number (optional)", "D-U-N-S numarası (isteğe bağlı)"],
    signatory: ["Authorised signatory", "Yetkili imza sahibi"],
    signatoryName: ["Full name", "Ad soyad"],
    signatoryTitle: ["Title", "Unvan"],
    signatoryHint: ["The signatory verifies their identity in the Identity step.", "Yetkili kişi kimliğini Kimlik adımında doğrular."],
    bank: ["Bank account", "Banka hesabı"],
    bankHint: ["In your legal name. Turkish payees are paid in TRY to a Turkish IBAN; payees outside Turkey in USD.", "Yasal adınıza olmalıdır. Türkiye'deki alıcılara TRY olarak Türk IBAN'ına, yurt dışındakilere USD olarak ödenir."],
    holder: ["Account holder name", "Hesap sahibinin adı"],
    method: ["How we pay you", "Ödeme yöntemi"],
    iban: ["IBAN", "IBAN"],
    swift: ["SWIFT/BIC", "SWIFT/BIC"],
    account: ["Account number", "Hesap numarası"],
    routing: ["ACH routing number", "ACH routing numarası"],
    achType: ["Account type", "Hesap türü"],
    bankName: ["Bank name", "Banka adı"],
    bankAddress: ["Bank address", "Banka adresi"],
    savedBank: ["Saved:", "Kayıtlı:"],
    replace: ["Replace", "Değiştir"],
    tax: ["Tax profile", "Vergi bilgileri"],
    taxId: ["Tax id", "Vergi kimlik numarası"],
    taxCountry: ["Tax residence country", "Vergi mukimi olunan ülke"],
    treaty: ["Claim a tax treaty rate (country, optional)", "Vergi anlaşması oranı talep et (ülke, isteğe bağlı)"],
    resExpiry: ["Certificate of residence valid until", "Mukimlik belgesinin geçerlilik tarihi"],
    taxHint: ["Pigeonpost buys from you and resells, so it may withhold Turkish tax (stopaj) from payouts depending on who you are. Statements show gross, withholding and net.", "Pigeonpost sizden alıp yeniden sattığı için, kim olduğunuza göre ödemelerden stopaj kesebilir. Hesap özetlerinde brüt, kesinti ve net gösterilir."],
    docsIntro: ["PDF, JPEG or PNG, up to 10 MB each. Files are scanned for malware and stored encrypted.", "PDF, JPEG veya PNG, her biri en fazla 10 MB. Dosyalar zararlı yazılım için taranır ve şifreli saklanır."],
    required: ["Required", "Zorunlu"],
    optional: ["Optional", "İsteğe bağlı"],
    ifTreaty: ["Required for a treaty rate", "Anlaşma oranı için zorunlu"],
    upload: ["Upload", "Yükle"],
    issued: ["Issued on", "Düzenlenme tarihi"],
    expires: ["Expires on", "Geçerlilik sonu"],
    remove: ["Remove", "Kaldır"],
    download: ["Download", "İndir"],
    uploaded: ["Uploaded.", "Yüklendi."],
    scan: { pending: ["waiting for the malware scan", "zararlı yazılım taraması bekleniyor"], clean: ["scanned, clean", "tarandı, temiz"], infected: ["failed the malware scan", "zararlı yazılım taramasından geçemedi"], error: ["scan will be retried", "tarama yeniden denenecek"] },
    identityIntro: ["Verify the identity of {who} with Shufti Pro: an ID document, a selfie and sanctions screening. It takes a few minutes.", "{who} kimliğini Shufti Pro ile doğrulayın: kimlik belgesi, özçekim ve yaptırım taraması. Birkaç dakika sürer."],
    you: ["yourself", "kendinizin"],
    theSignatory: ["the authorised signatory", "yetkili imza sahibinin"],
    startKyc: ["Start identity verification", "Kimlik doğrulamasını başlat"],
    restartKyc: ["Start again", "Yeniden başlat"],
    openKyc: ["Open in a new tab", "Yeni sekmede aç"],
    kycUnavailable: ["Identity verification is temporarily unavailable, so the application can't be submitted yet. Your draft is saved; come back later.", "Kimlik doğrulaması geçici olarak kullanılamıyor, bu yüzden başvuru şimdilik gönderilemez. Taslağınız kayıtlı; daha sonra tekrar deneyin."],
    sandbox: ["Sandbox: Shufti Pro isn't connected on this server yet, so nothing is verified for real.", "Deneme ortamı: Shufti Pro bu sunucuya henüz bağlı değil, gerçek doğrulama yapılmıyor."],
    simulateOk: ["Simulate: verified", "Dene: doğrulandı"],
    simulateNo: ["Simulate: declined", "Dene: reddedildi"],
    kyc: { pending: ["In progress", "Sürüyor"], accepted: ["Verified", "Doğrulandı"], declined: ["Declined", "Reddedildi"], cancelled: ["Did not finish", "Tamamlanmadı"], error: ["Did not finish", "Tamamlanmadı"] },
    agree: ["I accept the Pigeonpost Developer Distribution Agreement and the Developer Content Policy (drafts, {v}).", "Pigeonpost Geliştirici Dağıtım Sözleşmesi'ni ve Geliştirici İçerik Politikası'nı kabul ediyorum (taslak, {v})."],
    reply: ["Your reply to Pigeonpost", "Pigeonpost'a yanıtınız"],
    submit: ["Submit for review", "İncelemeye gönder"],
    resubmit: ["Resubmit", "Yeniden gönder"],
    submitHint: ["Submitting needs a recent two-step sign-in.", "Göndermek için yakın zamanda iki adımlı giriş gerekir."],
    submitted: ["Submitted. Pigeonpost reviews it and writes to you by email and Pigeonpost message.", "Gönderildi. Pigeonpost inceleyip size e-posta ve Pigeonpost mesajıyla yazacak."],
    history: ["History", "Geçmiş"],
    edit: ["Update the application", "Başvuruyu güncelle"],
    staffMessage: ["Message from Pigeonpost", "Pigeonpost'tan mesaj"],
    state: {
      draft: ["Draft", "Taslak"], submitted: ["Submitted — waiting for review", "Gönderildi — inceleme bekliyor"], in_review: ["In review", "İncelemede"],
      needs_info: ["More information needed", "Ek bilgi gerekiyor"], approved: ["Approved — verified publisher", "Onaylandı — doğrulanmış yayıncı"], rejected: ["Not approved", "Onaylanmadı"],
    },
    checklist: ["Checklist", "Kontrol listesi"],
    newApplication: ["Start a new application", "Yeni başvuru başlat"],
  };
  const t = (key, vars) => {
    const parts = key.split(".");
    let v = S;
    for (const p of parts) v = v && v[p];
    let out = Array.isArray(v) ? (TR && v[1] ? v[1] : v[0]) : key;
    for (const [k, x] of Object.entries(vars || {})) out = String(out).replace(`{${k}}`, x);
    return out;
  };

  const ERRORS = {
    application_exists: "There is already an open application for this owner.",
    application_locked: "This application is with Pigeonpost and can't be changed now.",
    application_incomplete: "Finish the checklist items marked below first.",
    revision_mismatch: "The application changed in another tab. Reload and try again.",
    account_type_needs_person: "Individuals and sole proprietors register for their own account.",
    country_mismatch: "That country doesn't match the account type.",
    iban_checksum: "That IBAN's check digits are wrong.",
    iban_length: "That IBAN has the wrong length for its country.",
    iban_country: "That country doesn't use IBANs; use SWIFT or ACH.",
    iban_format: "That isn't an IBAN.",
    iban_must_be_turkish: "Turkish payees are paid to a Turkish IBAN.",
    iban_must_be_foreign: "Payees outside Turkey are paid in USD to an account outside Turkey.",
    swift_required: "Add the bank's SWIFT/BIC.",
    swift_format: "That SWIFT/BIC isn't valid.",
    routing_checksum: "That routing number's check digit is wrong.",
    tckn_checksum: "That T.C. Kimlik No isn't valid.",
    tckn_format: "A T.C. Kimlik No has 11 digits.",
    vkn_checksum: "That Vergi Kimlik No isn't valid.",
    vkn_format: "A Vergi Kimlik No has 10 digits.",
    residence_certificate_required: "A treaty rate needs the certificate of residence and its expiry date.",
    residence_certificate_expired: "The certificate of residence must still be valid.",
    unsupported_document: "Upload a PDF, JPEG or PNG file.",
    document_too_large: "Documents are at most 10 MB.",
    issued_on_required: "Give the date this document was issued.",
    expires_on_required: "Give the date this document expires.",
    documents_unavailable: "Document uploads aren't available on this server yet.",
    kyc_unavailable: "Identity verification isn't answering right now. Try again shortly.",
    kyc_done: "Your identity is already verified.",
    malformed_document: "That file isn't a well-formed PDF, JPEG or PNG. Export it again and upload the new file.",
    swift_must_be_foreign: "Payees outside Turkey are paid in USD to a bank outside Turkey.",
    ach_requires_us: "ACH is for payees in the United States.",
    country_required: "Give your country in the Details step first.",
    document_not_scanned: "This file hasn't passed the malware scan yet.",
    agreement_version_mismatch: "The agreements changed. Read and accept the current versions.",
    registration_unavailable: "Publisher registration isn't available right now.",
  };
  const say = (deps, status, e) => MP.report(deps, status, e, "publisher", ERRORS);

  MP.registration = function (root, deps, opts) {
    const o = opts || {};
    const st = { reqs: null, apps: null, app: null, step: 0, error: null, editBank: false, editTax: false, orgs: o.orgs || [], gen: 0 };
    const status = MP.statusRegion("mp-reg-status");
    const spec = () => (st.reqs?.account_types || []).find((x) => x.id === st.app?.account_type) || null;
    const path = () => `/v1/publisher-registrations/${encodeURIComponent(st.app.id)}`;
    const json = (method, p, body) => deps.pb(p, { method, body: JSON.stringify(body || {}) });

    async function load(id) {
      const gen = ++st.gen;
      try {
        const [reqs, list] = await Promise.all([
          st.reqs ? Promise.resolve({ data: st.reqs }) : deps.pb("/v1/publisher-registration/requirements"),
          deps.pb("/v1/publisher-registrations"),
        ]);
        if (gen !== st.gen) return;
        if (!reqs || !reqs.data || !Array.isArray(reqs.data.account_types)) throw Object.assign(new Error("unavailable"), { code: "registration_unavailable" });
        st.reqs = reqs.data;
        st.apps = Array.isArray(list && list.data) ? list.data : [];
        // The open application, else the latest verified one (its status page), newest first.
        const pick = st.apps.find((a) => !["approved", "rejected"].includes(a.state) && a.account_type !== "legacy")
          || st.apps.find((a) => a.account_type !== "legacy");
        const open = id || st.app?.id || (pick || {}).id;
        st.app = open ? (await deps.pb(`/v1/publisher-registrations/${encodeURIComponent(open)}`)).data : null;
        st.error = null;
      } catch (e) {
        st.error = e;
      }
      if (gen === st.gen) draw();
    }

    async function act(fn, done, after) {
      try {
        const r = await fn();
        if (r && r.data && r.data.id && r.data.state) st.app = r.data;
        if (done) status.say(done);
        if (after) await after(r);
        draw();
        return r;
      } catch (e) {
        say(deps, status, e);
        if (e.code === "application_incomplete" && e.body && e.body.checklist && st.app) { st.app.checklist = e.body.checklist; draw(); }
        return null;
      }
    }

    // ---- pieces -----------------------------------------------------------------------------

    const field = (id, label, input, hint) => [el("label", { for: id }, label), input, hint ? el("span", { class: "mp-hint", text: hint }) : null];
    const input = (id, value, extra) => el("input", Object.assign({ id, value: value || "", autocomplete: "off" }, extra || {}));
    // Native append stringifies arrays; flatten the optional groups first.
    const add = (node, ...parts) => { for (const p of parts.flat(3)) if (p != null && p !== false) node.append(p); };
    const val = (id) => (root.querySelector(`#${id}`)?.value || "").trim();
    const opt = (id) => val(id) || undefined;

    function stepper() {
      const names = S.steps[TR ? 1 : 0];
      return el("ol", { class: "mp-steps", "aria-label": "Application steps" }, names.map((n, i) =>
        el("li", { class: i === st.step ? "mp-step is-current" : "mp-step", "aria-current": i === st.step ? "step" : null },
          el("button", { type: "button", class: "btn btn-small btn-quiet", "data-step": i, onclick: () => { st.step = i; draw(); } }, `${i + 1}. ${n}`))));
    }

    function nav(save) {
      return el("div", { class: "mp-reg-nav" },
        st.step > 0 ? el("button", { type: "button", class: "btn btn-small btn-quiet", onclick: () => { st.step -= 1; draw(); } }, t("back")) : null,
        save ? el("button", { type: "button", class: "btn btn-small", id: "mp-reg-save", onclick: () => save(false) }, t("save")) : null,
        save ? el("button", { type: "button", class: "btn btn-small btn-primary", id: "mp-reg-next", onclick: () => save(true) }, t("next")) : null);
    }

    function startForm() {
      const types = st.reqs.account_types;
      const typeSel = el("select", { id: "mp-reg-type", required: true }, types.map((x) => el("option", { value: x.id, text: TR ? x.label_tr : x.label })));
      const owner = el("select", { id: "mp-reg-owner" }, el("option", { value: "", text: t("myAccount") }), st.orgs.map((x) => el("option", { value: x.id, text: x.name })));
      const name = input("mp-reg-name", "", { required: true, minlength: "2", maxlength: "80" });
      const docsList = el("ul", { class: "mp-list", id: "mp-reg-type-docs" });
      const describe = () => {
        const x = types.find((y) => y.id === typeSel.value);
        docsList.textContent = "";
        for (const d of x.documents) docsList.append(el("li", { text: `${TR ? d.label_tr : d.label} — ${t(d.need === "required" ? "required" : d.need === "if_treaty" ? "ifTreaty" : "optional")}` }));
        if (!x.documents.length) docsList.append(el("li", { text: TR ? "Belge gerekmez; kimlik doğrulaması yeterli." : "No company documents; the identity check covers you." }));
        owner.disabled = !x.org_allowed;
        if (!x.org_allowed) owner.value = "";
      };
      typeSel.addEventListener("change", describe);
      const form = el("form", { class: "ac-form", id: "mp-reg-start", onsubmit: (e) => {
        e.preventDefault();
        const body = { account_type: typeSel.value, public_name: name.value.trim() };
        if (owner.value) body.org_id = owner.value;
        act(() => json("POST", "/v1/publisher-registrations", body), t("saved"), (r) => load(r.data.id));
      } },
        el("fieldset", {}, el("legend", { text: t("title") }),
          ...field("mp-reg-type", t("type"), typeSel),
          docsList,
          st.orgs.length ? field("mp-reg-owner", t("publishAs"), owner) : [],
          ...field("mp-reg-name", t("publicName"), name),
          el("button", { type: "submit", class: "btn btn-primary" }, t("start"))));
      queueMicrotask(describe);
      return form;
    }

    function detailsStep() {
      const a = st.app, p = a.profile || {}, x = spec(), ad = p.address || {};
      const country = input("mp-reg-country", p.country || (x.country || ""), { maxlength: "2", pattern: "[A-Za-z]{2}", required: true, spellcheck: "false" });
      if (x.country) country.readOnly = true;
      const fs = el("fieldset", { id: "mp-reg-details" }, el("legend", { text: S.steps[TR ? 1 : 0][0] }),
        ...field("mp-reg-public", t("publicName"), input("mp-reg-public", a.public_name, { maxlength: "80", required: true })),
        ...field("mp-reg-legal", t("legalName"), input("mp-reg-legal", p.legal_name, { maxlength: "200", required: true, autocomplete: "name" })),
        x.company ? field("mp-reg-trading", t("tradingName"), input("mp-reg-trading", p.trading_name, { maxlength: "200" })) : [],
        ...field("mp-reg-country", t("country"), country),
        ...field("mp-reg-line1", t("line1"), input("mp-reg-line1", ad.line1, { maxlength: "200", autocomplete: "address-line1" })),
        ...field("mp-reg-line2", t("line2"), input("mp-reg-line2", ad.line2, { maxlength: "200", autocomplete: "address-line2" })),
        ...field("mp-reg-city", t("city"), input("mp-reg-city", ad.city, { maxlength: "100", autocomplete: "address-level2" })),
        ...field("mp-reg-postal", t("postal"), input("mp-reg-postal", ad.postal_code, { maxlength: "20", autocomplete: "postal-code" })),
        ...field("mp-reg-website", t("website"), input("mp-reg-website", p.website, { type: "url", maxlength: "200", placeholder: "https://" })),
        ...field("mp-reg-support", t("supportEmail"), input("mp-reg-support", p.support_email, { type: "email", maxlength: "254" })),
        ...field("mp-reg-support-addr", t("supportAddress"), input("mp-reg-support-addr", p.support_address, { maxlength: "120", placeholder: "/you/support" })),
        ...field("mp-reg-contact", t("contactEmail"), input("mp-reg-contact", p.contact_email, { type: "email", maxlength: "254", autocomplete: "email" })),
        x.id === "company_tr" ? [...field("mp-reg-mersis", t("mersis"), input("mp-reg-mersis", p.mersis_number, { inputmode: "numeric", maxlength: "20" })),
          ...field("mp-reg-sicil", t("tradeRegistry"), input("mp-reg-sicil", p.trade_registry_number, { maxlength: "32" }))] : [],
        x.id === "company_foreign" ? field("mp-reg-regno", t("registration"), input("mp-reg-regno", p.registration_number, { maxlength: "64" })) : [],
        x.company ? field("mp-reg-duns", t("duns"), input("mp-reg-duns", p.duns_number, { inputmode: "numeric", maxlength: "11" })) : []);
      if (x.company) {
        const s = p.signatory || {};
        fs.append(el("fieldset", { id: "mp-reg-signatory" }, el("legend", { text: t("signatory") }),
          el("p", { class: "mp-hint", text: t("signatoryHint") }),
          ...field("mp-reg-sig-name", t("signatoryName"), input("mp-reg-sig-name", s.name, { maxlength: "200" })),
          ...field("mp-reg-sig-title", t("signatoryTitle"), input("mp-reg-sig-title", s.title, { maxlength: "100" }))));
      }
      const save = (next) => {
        const countryCode = val("mp-reg-country").toUpperCase();
        const profile = {
          legal_name: opt("mp-reg-legal"), trading_name: opt("mp-reg-trading"), country: countryCode || undefined,
          address: { line1: val("mp-reg-line1"), line2: opt("mp-reg-line2"), city: val("mp-reg-city"), postal_code: val("mp-reg-postal"), country: countryCode },
          website: opt("mp-reg-website"), support_email: opt("mp-reg-support"), support_address: opt("mp-reg-support-addr"), contact_email: opt("mp-reg-contact"),
          mersis_number: opt("mp-reg-mersis"), trade_registry_number: opt("mp-reg-sicil"), registration_number: opt("mp-reg-regno"), duns_number: opt("mp-reg-duns"),
        };
        if (x.company) profile.signatory = { name: val("mp-reg-sig-name"), title: val("mp-reg-sig-title") };
        const body = { profile, revision: a.revision };
        if (val("mp-reg-public")) body.public_name = val("mp-reg-public");
        act(() => json("PATCH", path(), body), t("saved"), () => { if (next) st.step = 1; });
      };
      return [fs, nav(save)];
    }

    function payoutStep() {
      const a = st.app, x = spec();
      const parts = [];
      // Bank
      const bankBox = el("fieldset", { id: "mp-reg-bank" }, el("legend", { text: t("bank") }), el("p", { class: "mp-hint", text: t("bankHint") }));
      if (a.bank && !st.editBank) {
        const b = a.bank;
        bankBox.append(el("p", { id: "mp-reg-bank-saved" }, el("strong", { text: `${t("savedBank")} ` }),
          `${b.holder_name} · ${b.iban || b.account_number || ""}${b.swift_bic ? ` · ${b.swift_bic}` : ""}${b.routing_number ? ` · ${b.routing_number}` : ""} · ${b.currency}`),
          el("button", { type: "button", class: "btn btn-small", id: "mp-reg-bank-replace", onclick: () => { st.editBank = true; draw(); } }, t("replace")));
      } else {
        const methods = x.bank_methods;
        const method = el("select", { id: "mp-reg-method" }, methods.map((m) => el("option", { value: m, text: { iban: "IBAN", swift: "SWIFT (account number + BIC)", ach: "ACH (US bank)" }[m] })));
        const groups = {
          iban: el("div", { "data-method": "iban" }, ...field("mp-reg-iban", t("iban"), input("mp-reg-iban", "", { spellcheck: "false", maxlength: "42", placeholder: x.country === "TR" ? "TR00 0000 0000 0000 0000 0000 00" : "" })),
            x.country === "TR" ? [] : field("mp-reg-bic", t("swift"), input("mp-reg-bic", "", { maxlength: "11", spellcheck: "false" }))),
          swift: el("div", { "data-method": "swift", hidden: true }, ...field("mp-reg-acct", t("account"), input("mp-reg-acct", "", { maxlength: "34" })),
            ...field("mp-reg-bic2", t("swift"), input("mp-reg-bic2", "", { maxlength: "11" }))),
          ach: el("div", { "data-method": "ach", hidden: true }, ...field("mp-reg-routing", t("routing"), input("mp-reg-routing", "", { inputmode: "numeric", maxlength: "9" })),
            ...field("mp-reg-acct2", t("account"), input("mp-reg-acct2", "", { inputmode: "numeric", maxlength: "17" })),
            ...field("mp-reg-achtype", t("achType"), el("select", { id: "mp-reg-achtype" }, el("option", { value: "checking", text: "Checking" }), el("option", { value: "savings", text: "Savings" })))),
        };
        method.addEventListener("change", () => { for (const [k, g] of Object.entries(groups)) g.hidden = k !== method.value; });
        add(bankBox, ...field("mp-reg-holder", t("holder"), input("mp-reg-holder", a.profile?.legal_name || "", { maxlength: "140", autocomplete: "name" })),
          methods.length > 1 ? field("mp-reg-method", t("method"), method) : [], ...Object.values(groups),
          ...field("mp-reg-bankname", t("bankName"), input("mp-reg-bankname", "", { maxlength: "128" })),
          x.country === "TR" ? [] : field("mp-reg-bankaddr", t("bankAddress"), input("mp-reg-bankaddr", "", { maxlength: "255" })));
      }
      parts.push(bankBox);
      // Tax
      const taxBox = el("fieldset", { id: "mp-reg-tax" }, el("legend", { text: t("tax") }), el("p", { class: "mp-hint", text: t("taxHint") }));
      if (a.tax && !st.editTax) {
        const tx = a.tax;
        taxBox.append(el("p", { id: "mp-reg-tax-saved" }, el("strong", { text: `${t("savedBank")} ` }),
          `${tx.tax_id || "—"} · ${tx.tax_country}${tx.treaty_country ? ` · treaty ${tx.treaty_country} until ${tx.residence_certificate_expires_on}` : ""}`),
          el("button", { type: "button", class: "btn btn-small", id: "mp-reg-tax-replace", onclick: () => { st.editTax = true; draw(); } }, t("replace")));
      } else {
        const hint = x.tax_id === "tckn" ? "T.C. Kimlik No (11)" : x.tax_id === "vkn" ? (x.id === "sole_proprietor_tr" ? "VKN (10) / TCKN (11)" : "VKN (10)") : x.tax_id === "foreign_required" ? "EIN / VAT / TIN" : "TIN (optional)";
        add(taxBox, ...field("mp-reg-taxid", `${t("taxId")} — ${hint}`, input("mp-reg-taxid", "", { maxlength: "32", spellcheck: "false" })),
          ...field("mp-reg-taxcountry", t("taxCountry"), input("mp-reg-taxcountry", x.country || a.profile?.country || "", { maxlength: "2", readOnly: !!x.country })),
          x.country === "TR" ? [] : [...field("mp-reg-treaty", t("treaty"), input("mp-reg-treaty", "", { maxlength: "2" })),
            ...field("mp-reg-resexp", t("resExpiry"), input("mp-reg-resexp", "", { type: "date" }))]);
      }
      parts.push(taxBox);
      const save = (next) => {
        const body = { revision: a.revision };
        if (!a.bank || st.editBank) {
          const m = val("mp-reg-method") || "iban";
          body.bank = { holder_name: val("mp-reg-holder"), method: m, bank_name: opt("mp-reg-bankname"), bank_address: opt("mp-reg-bankaddr") };
          if (m === "iban") Object.assign(body.bank, { iban: val("mp-reg-iban"), swift_bic: opt("mp-reg-bic") });
          if (m === "swift") Object.assign(body.bank, { account_number: val("mp-reg-acct"), swift_bic: opt("mp-reg-bic2") });
          if (m === "ach") Object.assign(body.bank, { routing_number: val("mp-reg-routing"), account_number: val("mp-reg-acct2"), ach_account_type: val("mp-reg-achtype") });
          if (!body.bank.holder_name && !body.bank.iban && !body.bank.account_number) delete body.bank;
        }
        if (!a.tax || st.editTax) {
          body.tax = { tax_id: opt("mp-reg-taxid"), tax_country: val("mp-reg-taxcountry").toUpperCase(), treaty_country: opt("mp-reg-treaty")?.toUpperCase(), residence_certificate_expires_on: opt("mp-reg-resexp") };
          if (!body.tax.tax_country) delete body.tax;
        }
        act(() => json("PATCH", path(), body), t("saved"), () => { st.editBank = false; st.editTax = false; if (next) st.step = 2; });
      };
      parts.push(nav(save));
      return parts;
    }

    async function uploadDoc(type, file, issued, expires) {
      const q = new URLSearchParams({ type, filename: file.name || "document" });
      if (issued) q.set("issued_on", issued);
      if (expires) q.set("expires_on", expires);
      return act(() => deps.pbRaw(`${path()}/documents?${q}`, { method: "POST", body: file }), t("uploaded"), () => load(st.app.id));
    }

    async function downloadDoc(d) {
      try {
        const blob = await deps.pbRaw(`${path()}/documents/${encodeURIComponent(d.id)}`, { as: "blob" });
        const url = URL.createObjectURL(blob);
        const a = el("a", { href: url, download: d.filename });
        document.body.append(a); a.click(); a.remove();
        setTimeout(() => URL.revokeObjectURL(url), 10_000);
      } catch (e) { say(deps, status, e); }
    }

    function documentsStep() {
      const a = st.app, x = spec();
      const box = el("fieldset", { id: "mp-reg-docs" }, el("legend", { text: S.steps[TR ? 1 : 0][2] }), el("p", { class: "mp-hint", text: t("docsIntro") }));
      const list = el("ul", { class: "mp-list mp-doclist" });
      for (const d of x.documents) {
        const have = (a.documents || []).filter((f) => f.type === d.type);
        const file = el("input", { type: "file", id: `mp-doc-${d.type}`, accept: "application/pdf,image/jpeg,image/png", "aria-label": TR ? d.label_tr : d.label });
        const issued = d.max_age_days ? el("input", { type: "date", id: `mp-doc-${d.type}-issued`, "aria-label": t("issued") }) : null;
        const expires = d.expires ? el("input", { type: "date", id: `mp-doc-${d.type}-expires`, "aria-label": t("expires") }) : null;
        list.append(el("li", { class: "mp-doc", "data-doc": d.type },
          el("div", { class: "mp-doc-head" }, el("strong", { text: TR ? d.label_tr : d.label }), " ",
            el("span", { class: d.need === "required" ? "mp-chip" : "mp-chip mp-chip-quiet", text: t(d.need === "required" ? "required" : d.need === "if_treaty" ? "ifTreaty" : "optional") })),
          have.length ? el("ul", { class: "mp-list" }, have.map((f) => el("li", { class: "mp-row", "data-file": f.id },
            el("span", { text: f.filename }), el("span", { class: f.scan === "infected" ? "pp-warn" : "muted", text: t(`scan.${f.scan}`) }),
            el("button", { type: "button", class: "btn btn-small btn-quiet", onclick: () => downloadDoc(f) }, t("download")),
            a.editable ? el("button", { type: "button", class: "btn btn-small btn-quiet", "data-remove": f.id, onclick: () => act(() => deps.pb(`${path()}/documents/${encodeURIComponent(f.id)}`, { method: "DELETE" }), null, () => load(a.id)) }, t("remove")) : null))) : null,
          a.editable ? el("div", { class: "mp-doc-upload" }, file,
            issued ? el("label", {}, `${t("issued")} `, issued) : null,
            expires ? el("label", {}, `${t("expires")} `, expires) : null,
            el("button", { type: "button", class: "btn btn-small", "data-upload": d.type, onclick: () => {
              if (!file.files || !file.files[0]) { status.say(ERRORS.unsupported_document); return; }
              if (file.files[0].size > (st.reqs.document_max_bytes || 10485760)) { status.say(ERRORS.document_too_large); return; }
              uploadDoc(d.type, file.files[0], issued?.value, expires?.value);
            } }, t("upload"))) : null));
      }
      if (!x.documents.length) list.append(el("li", { class: "muted", text: TR ? "Bu hesap türü için belge gerekmez." : "This account type needs no documents." }));
      box.append(list);
      return [box, nav(() => { st.step = 3; draw(); })];
    }

    function identityStep() {
      const a = st.app, x = spec();
      const checks = a.kyc || [];
      const c = checks[checks.length - 1];
      const box = el("fieldset", { id: "mp-reg-kyc" }, el("legend", { text: S.steps[TR ? 1 : 0][3] }),
        el("p", { text: t("identityIntro", { who: x.kyc_subject === "signatory" ? t("theSignatory") : t("you") }) }));
      if (a.kyc_provider === "mock") box.append(el("p", { class: "pp-warn", id: "mp-reg-sandbox", text: t("sandbox") }));
      const unavailable = a.kyc_provider === "unavailable";
      if (unavailable) box.append(el("p", { class: "pp-warn", id: "mp-reg-kyc-unavailable", role: "status", text: t("kycUnavailable") }));
      if (c) {
        box.append(el("p", { id: "mp-reg-kyc-state" }, el("strong", { text: t(`kyc.${c.result}`) }), c.declined_reason ? ` — ${c.declined_reason}` : ""));
        if (c.result === "pending" && c.verification_url) {
          box.append(el("iframe", { src: c.verification_url, id: "mp-reg-kyc-frame", class: "mp-kyc-frame", title: "Shufti Pro identity verification", allow: "camera; microphone; fullscreen", referrerpolicy: "no-referrer" }),
            el("p", {}, el("a", { href: c.verification_url, target: "_blank", rel: "noopener noreferrer" }, t("openKyc"))));
        }
        if (c.result === "pending" && c.sandbox && a.editable) {
          box.append(el("div", { class: "mp-reg-nav" },
            el("button", { type: "button", class: "btn btn-small", id: "mp-reg-sim-ok", onclick: () => act(() => json("POST", `${path()}/kyc/${encodeURIComponent(c.id)}/simulate`, { outcome: "accepted" })) }, t("simulateOk")),
            el("button", { type: "button", class: "btn btn-small btn-quiet", id: "mp-reg-sim-no", onclick: () => act(() => json("POST", `${path()}/kyc/${encodeURIComponent(c.id)}/simulate`, { outcome: "declined" })) }, t("simulateNo"))));
        }
      }
      if (a.editable && !unavailable && (!c || c.result !== "accepted")) {
        box.append(el("button", { type: "button", class: "btn btn-primary", id: "mp-reg-kyc-start", onclick: () => act(() => json("POST", `${path()}/kyc`, {}), null, () => load(a.id)) }, c ? t("restartKyc") : t("startKyc")));
      }
      return [box, nav(() => { st.step = 4; draw(); })];
    }

    function checklistView(items) {
      return el("ul", { class: "mp-list mp-checklist", id: "mp-reg-checklist", "aria-label": t("checklist") }, (items || []).map((i) =>
        el("li", { class: `mp-check-item ${i.ok === true ? "is-ok" : i.ok === false ? "is-missing" : "is-info"}`, "data-check": i.id },
          el("span", { class: "mp-check-mark", "aria-hidden": "true", text: i.ok === true ? "✓" : i.ok === false ? "•" : "–" }),
          el("span", { text: i.detail }))));
    }

    function reviewStep() {
      const a = st.app;
      const v = a.agreement || {};
      const agreed = v.agreement_version === v.current_agreement_version && v.content_policy_version === v.current_content_policy_version;
      const box = el("fieldset", { id: "mp-reg-review" }, el("legend", { text: S.steps[TR ? 1 : 0][4] }), checklistView(a.checklist));
      const agree = el("input", { type: "checkbox", id: "mp-reg-agree", checked: agreed || null, disabled: agreed || null });
      box.append(el("label", { class: "mp-check", for: "mp-reg-agree" }, agree, " ", t("agree", { v: v.current_agreement_version || "" })));
      if (!agreed) agree.addEventListener("change", () => {
        if (agree.checked) act(() => json("PATCH", path(), { revision: a.revision, accept_agreements: { agreement_version: v.current_agreement_version, content_policy_version: v.current_content_policy_version } }));
      });
      const reply = a.state === "needs_info" ? el("textarea", { id: "mp-reg-reply", rows: "3", maxlength: "4000" }) : null;
      if (reply) box.append(el("label", { for: "mp-reg-reply" }, t("reply")), reply);
      box.append(el("p", { class: "mp-hint", text: t("submitHint") }),
        el("button", { type: "button", class: "btn btn-primary", id: "mp-reg-submit", onclick: () => {
          const body = { revision: a.revision };
          if (reply && reply.value.trim()) body.message = reply.value.trim();
          act(() => json("POST", `${path()}/submit`, body), t("submitted"));
        } }, a.state === "needs_info" ? t("resubmit") : t("submit")));
      return [box, nav(null)];
    }

    function statusView() {
      const a = st.app;
      const box = el("section", { class: "mp-section", id: "mp-reg-state", "aria-labelledby": "mp-reg-state-h" },
        el("h4", { id: "mp-reg-state-h", text: `${a.public_name || a.account_type_label} · ${a.account_type_label}` }),
        el("p", { class: `mp-badge mp-badge-${a.state}`, id: "mp-reg-badge", text: t(`state.${a.state}`) }));
      if (a.staff_message && ["needs_info", "rejected"].includes(a.state)) {
        box.append(el("div", { class: "mp-callout", id: "mp-reg-staff-message" }, el("strong", { text: `${t("staffMessage")}: ` }), a.staff_message.message));
      }
      if (["submitted", "in_review"].includes(a.state)) box.append(checklistView(a.checklist));
      const ev = (a.events || []).filter((e) => e.to || e.message);
      if (ev.length) {
        box.append(el("details", { class: "mp-more", id: "mp-reg-history" }, el("summary", { text: t("history") }),
          el("ul", { class: "mp-list" }, ev.map((e) => el("li", { class: "mp-row" },
            el("span", { class: "muted", text: MP.when(e.at) }), el("span", { text: e.to ? t(`state.${e.to}`) : e.kind }), e.message ? el("span", { text: `“${e.message}”` }) : null)))));
      }
      if (a.state === "rejected") box.append(el("button", { type: "button", class: "btn btn-small", id: "mp-reg-new", onclick: () => { st.app = null; draw(); } }, t("newApplication")));
      return box;
    }

    function draw() {
      root.textContent = "";
      root.append(el("h4", { id: "mp-reg-title", text: t("title") }), status);
      if (st.error) { root.append(el("p", { class: "muted", text: MP.errorText(st.error, ERRORS) })); return; }
      if (!st.reqs) { root.append(el("p", { class: "muted", text: "Loading…" })); return; }
      if (!st.app) { root.append(el("p", { class: "muted", text: t("intro") }), startForm()); return; }
      const a = st.app;
      if (a.state !== "draft") root.append(statusView());
      if (!a.editable || !spec()) {
        if (!spec() && a.editable) root.append(startForm());
        return;
      }
      root.append(stepper());
      const step = [detailsStep, payoutStep, documentsStep, identityStep, reviewStep][st.step] || detailsStep;
      root.append(el("div", { class: "mp-reg-body", id: `mp-reg-step-${st.step}` }, ...step()));
    }

    draw();
    load();
    return { reload: () => load(st.app && st.app.id) };
  };
})();
