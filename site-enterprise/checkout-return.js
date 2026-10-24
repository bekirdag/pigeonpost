// After iyzico: poll the bridge with backoff until the organization is provisioned. The redirect
// itself proves nothing; the bridge and its reconciler decide, and they finish even if this page
// is closed (SDS §8.5).
(function () {
  "use strict";
  const PPE = window.PPE;
  const el = PPE.el;
  const delays = [1500, 2000, 3000, 5000, 8000, 13000, 15000];
  const LIMIT_MS = 10 * 60 * 1000;

  function box(...nodes) { document.getElementById("return-box").replaceChildren(...nodes); }

  PPE.checkoutReturn = { delayFor: (n) => delays[Math.min(n, delays.length - 1)] };

  async function poll() {
    PPE.renderAccount();
    const q = new URLSearchParams(window.location.search);
    const draft = q.get("d") || PPE.store.get("ppe_ent_draft");
    if (!draft || !/^org_[a-z0-9]{6,64}$/.test(draft)) {
      box(el("h1", {}, "Nothing to confirm"), el("p", { class: "muted" }, "Open your organization from the admin page."), el("a", { class: "btn btn-primary", href: "/app" }, "Open admin"));
      return;
    }
    if (!PPE.signedIn()) { await PPE.signIn({ returnTo: `/checkout/return?d=${draft}` }); return; }
    const started = Date.now();
    for (let n = 0; Date.now() - started < LIMIT_MS; n++) {
      let r = null;
      try { r = await PPE.bridge(`/ent/checkout/${draft}`); }
      catch (e) { if (e instanceof PPE.Redirecting) return; if (e.status === 404) break; }
      const org = r && r.org;
      if (r && r.status === "failed") {
        box(el("h1", {}, "The payment didn't go through"), el("p", { class: "muted" }, "Nothing was charged. You can try again with another card."),
          el("a", { class: "btn btn-primary", href: "/start" }, "Try again"));
        return;
      }
      if (org && ["active", "past_due"].includes(org.status)) {
        PPE.store.del("ppe_start"); PPE.store.del("ppe_ent_checkout"); PPE.store.del("ppe_ent_draft");
        box(el("h1", {}, `${org.name} is ready`), el("p", { class: "muted" }, "Invite your first teammates."));
        window.setTimeout(() => window.location.replace(`/app/people?org=${encodeURIComponent(org.id)}`), 800);
        return;
      }
      if (org && org.status === "paid_unprovisioned") {
        box(el("h1", {}, "Payment received"), el("p", { class: "muted" }, `Someone else took /${org.namespace} before your payment finished. Your payment is safe; we'll contact you to pick another address or refund it.`));
        return;
      }
      await new Promise((resolve) => window.setTimeout(resolve, PPE.checkoutReturn.delayFor(n)));
    }
    box(el("h1", {}, "Still confirming"), el("p", { class: "muted" }, "Your bank is taking longer than usual. We'll finish setting up your organization as soon as the payment is confirmed — you don't need to pay again."),
      el("a", { class: "btn", href: "/app" }, "Open admin"));
  }
  poll();
})();
