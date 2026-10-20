// The iyzico hosted payment form, isolated on its own page.
//
// saas_be returns the form as an HTML fragment with an inline <script> that loads iyzico's bundle.
// This page's content-security policy admits iyzico's hosts and a per-request nonce, nothing else;
// the account page keeps its strict policy. The fragment is parsed inertly, and only its scripts
// are re-created with the nonce. No card data, form token or form content is stored anywhere.
(function () {
  "use strict";
  const cfg = window.PIGEONPOST_ACCOUNT || {};
  const $ = (s) => document.querySelector(s);
  const token = () => sessionStorage.getItem("pp_session") || localStorage.getItem("pp_session");
  const back = (why) => { if (why) sessionStorage.setItem("pp_checkout_notice", why); window.location.replace("/account"); };
  const say = (text) => { $("#pp-pay-summary").textContent = text; };

  function money(a, c) { try { return new Intl.NumberFormat(undefined, { style: "currency", currency: c }).format(a); } catch { return `${a} ${c}`; } }

  async function post(path, body) {
    const res = await fetch(`${cfg.apiBase || "/api"}${path}`, {
      method: "POST",
      headers: { authorization: `Bearer ${token()}`, "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify(body),
    });
    const json = await res.json().catch(() => ({}));
    if (!res.ok) throw Object.assign(new Error(json.error || `Request failed (${res.status})`), { status: res.status });
    return json;
  }

  function mount(html) {
    const nonce = document.querySelector('meta[name="csp-nonce"]')?.content || "";
    const parsed = new DOMParser().parseFromString(`<div>${html}</div>`, "text/html").body.firstElementChild;
    const scripts = Array.from(parsed.querySelectorAll("script"));
    scripts.forEach((s) => s.remove());
    const box = $("#pp-pay-form");
    box.replaceChildren();
    if (!parsed.querySelector("#iyzipay-checkout-form")) {
      const root = document.createElement("div");
      root.id = "iyzipay-checkout-form";
      root.className = "responsive";
      box.appendChild(root);
    }
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
      box.appendChild(script);
    }
  }

  async function run() {
    let pending;
    try { pending = JSON.parse(sessionStorage.getItem("pp_cloud_checkout") || "null"); } catch { pending = null; }
    if (!pending || !/^[a-f0-9]{32}$/.test(pending.intent_id || "") || !token()) return back();
    try {
      if (pending.kind === "card") {
        $("#pp-pay-title").textContent = "Update your card";
        const r = await post(`/v1/cloud/subscriptions/${encodeURIComponent(pending.subscription_id)}/card-update`,
          { intent_id: pending.intent_id, locale: pending.locale });
        say("Enter the new card for your Pigeonpost Cloud plan.");
        sessionStorage.removeItem("pp_cloud_checkout");
        mount(r.checkout_form_content);
        return;
      }
      const r = await post("/v1/cloud/checkout", { intent_id: pending.intent_id, plan_code: pending.plan_code,
        billing_profile_id: pending.billing_profile_id, locale: pending.locale });
      const names = /cloud-(\d+)names/.exec(r.plan_code)?.[1];
      say(`Pigeonpost Cloud, ${names} name${names === "1" ? "" : "s"}: ${money(Number(r.amount), r.currency)} per year, renewing yearly until you cancel.`);
      mount(r.checkout_form_content);
    } catch (e) {
      if (e.status === 401) return back("Your session expired. Sign in and try again.");
      // A refused intent is finished; the account page starts a fresh one after the fix.
      if ([400, 409, 429].includes(e.status)) sessionStorage.removeItem("pp_cloud_checkout");
      say("The payment form could not be opened: " + e.message);
    }
  }
  run();
})();
