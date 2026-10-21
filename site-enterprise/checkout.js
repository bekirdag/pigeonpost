// The iyzico hosted payment form for an organization, isolated on its own page.
//
// saas_be returns the form as an HTML fragment with an inline <script> that loads iyzico's bundle.
// Only this page's content-security policy admits iyzico's hosts and a per-request nonce; every
// other page keeps `script-src 'self'`. The fragment is parsed inertly and only its scripts are
// re-created with the nonce (and only iyzico-hosted script URLs). Nothing of the form is stored.
(function () {
  "use strict";
  const PPE = window.PPE;
  const $ = PPE.$;
  const say = (text) => { $("#pay-summary").textContent = text; };

  PPE.mountIyzico = function mount(html, box, nonce) {
    const parsed = new DOMParser().parseFromString(`<div>${html}</div>`, "text/html").body.firstElementChild;
    const scripts = Array.from(parsed.querySelectorAll("script"));
    scripts.forEach((s) => s.remove());
    box.replaceChildren();
    if (!parsed.querySelector("#iyzipay-checkout-form")) box.append(PPE.el("div", { id: "iyzipay-checkout-form", class: "responsive" }));
    box.append(...Array.from(parsed.childNodes));
    for (const source of scripts) {
      const script = document.createElement("script");
      if (source.src) {
        if (!/^https:\/\/([a-z0-9-]+\.)*iyzipay\.com\//.test(source.src)) continue;
        script.src = source.src;
      } else {
        script.text = source.textContent || "";
      }
      script.nonce = nonce;
      box.append(script);
    }
  };

  async function run() {
    PPE.renderAccount();
    const pending = PPE.store.json("ppe_ent_checkout");
    if (!pending || !/^[a-f0-9]{32}$/.test(pending.intent_id || "") || !PPE.signedIn()) { window.location.replace("/start"); return; }
    try {
      const r = await PPE.bridge("/ent/checkout", { method: "POST", body: pending });
      if (r.org_id) PPE.store.set("ppe_ent_draft", r.org_id);
      const seats = /^ent-(\d+)-/.exec(r.plan_code || "")?.[1];
      say(`${seats} seats: ${PPE.money(r.amount, r.currency)} ${r.interval === "year" ? "per year" : "per month"}, renewing until you cancel.`);
      PPE.mountIyzico(r.checkout_form_content, $("#pay-form"), document.querySelector('meta[name="csp-nonce"]')?.content || "");
    } catch (e) {
      if (e instanceof PPE.Redirecting) return;
      if ([400, 409].includes(e.status)) {
        // A refused intent is finished; the wizard starts a fresh one.
        PPE.store.del("ppe_ent_checkout");
        const draft = PPE.store.json("ppe_start");
        if (draft) { draft.intent = null; draft.step = e.code === "billing_profile_required" ? 2 : draft.step; PPE.store.set("ppe_start", JSON.stringify(draft)); }
      }
      say("The payment form could not be opened: " + PPE.explain(e));
    }
  }
  run();
})();
