// Handles: every name the account owns, whichever store sold it (App Store, Google Play, the web),
// with where it renews; and "Get a handle": search, then register into a plan with room or pick a
// plan (P30). A name picked before a missing step (sign-in, billing, a plan) waits in
// `pp_pending_handle` and is picked up again here.
//
// Bridge routes: GET /v1/handles/{name}/availability, POST /v1/names {namespace}.
(function () {
  "use strict";
  const el = (...a) => PP.el(...a);
  const SOURCE = { apple: "App Store", google: "Google Play" };
  const MANAGE = {
    apple: "https://apps.apple.com/account/subscriptions",
    google: "https://play.google.com/store/account/subscriptions?package=dev.pigeonpost.inbox",
  };

  /// The format check is local and instant; the reserved list is a fast subset of the registry's.
  function validate(raw) {
    const name = String(raw || "").trim().toLowerCase().replace(/^\/+/, "");
    if (!name) return { ok: false, silent: true };
    if (/[^a-z0-9]/.test(name)) return { ok: false, reason: "Letters a–z and digits 0–9 only." };
    if (name.length < 3) return { ok: false, reason: "At least 3 characters." };
    if (name.length > 32) return { ok: false, reason: "At most 32 characters." };
    const r = window.PIGEONPOST_RESERVED && window.PIGEONPOST_RESERVED.reason(name);
    if (r) return { ok: false, reason: r.charAt(0).toUpperCase() + r.slice(1) + "." };
    return { ok: true, name };
  }
  ACC.validateHandle = validate;

  function ownedTab(view, ov) {
    view.append(
      PP.ui.table({
        caption: "Your handles",
        columns: [
          { key: "name", label: "Handle", sort: true, render: (h) => el("strong", { class: "pp-mono", translate: "no" }, `/${h.name}`) },
          { key: "source", label: "Bought in", sort: (h) => SOURCE[h.source] || "Pigeonpost", render: (h) => SOURCE[h.source] || "Pigeonpost (web)" },
          { key: "active", label: "Status", render: (h) => (h.active ? PP.ui.badge("Active", "success") : PP.ui.badge("Expired", "danger")) },
          { key: "expires_at", label: "Renewal", sort: true, render: (h) => (h.expires_at ? `${h.active ? "Paid through" : "Expired on"} ${ACC.date(h.expires_at)}` : "With your plan") },
          { key: "manage", label: "", render: (h) => (MANAGE[h.source] ? ACC.btn(`Manage in ${SOURCE[h.source]}`, { size: "sm", href: MANAGE[h.source], icon: "external" }) : ACC.btn("Plan", { size: "sm", href: PP.href("plan") })) },
        ],
        rows: ov.handles,
        empty: { icon: "at", title: "No handles yet", text: "Get a name like /you that people and agents can reach.", action: { label: "Get a handle", href: PP.href("handles", "get") } },
      }),
      el("p", { class: "pp-faint" }, "Names belong to your Pigeonpost account across mobile, desktop and web. Free /pp names and agent names are given to inboxes in Inboxes & agents."));
  }

  function getTab(view, ov) {
    const room = ACC.hasRoom(ov);
    const input = el("input", { class: "pp-input acc-handle-input", id: "acc-handle", type: "text", placeholder: "yourname", autocomplete: "off", autocapitalize: "none", spellcheck: "false", maxlength: "32", "aria-describedby": "acc-handle-status" });
    const status = el("p", { class: "acc-status", id: "acc-handle-status", role: "status", "aria-live": "polite" });
    const buy = ACC.btn(room ? "Register it" : "Get it", { kind: "primary", id: "acc-buy", disabled: true });
    const next = el("div", { id: "acc-handle-next" });
    let current = { ok: false };
    let checking = 0;
    let debounce = null;
    const say = (text, tone) => { status.className = `acc-status${tone ? ` is-${tone}` : ""}`; status.textContent = text || ""; };

    async function check(v) {
      const mine = ++checking;
      let answer = null;
      try { answer = await ACC.bridgeGet(`/v1/handles/${encodeURIComponent(v.name)}/availability`, { keepSession: true }); } catch { answer = null; }
      if (mine !== checking) return;
      // Unknown is not free: when the check cannot be made, the button stays off.
      if (!answer || answer.known === false) { say("Could not check that name just now. Try again in a moment.", "error"); return; }
      if (!answer.available) { say(`/${v.name} is already taken.`, "error"); return; }
      say(`/${v.name} is available.`, "ok");
      buy.disabled = false;
    }
    input.addEventListener("input", () => {
      const v = validate(input.value);
      current = v;
      window.clearTimeout(debounce);
      checking++;
      buy.disabled = true;
      next.replaceChildren();
      if (v.silent) { say(""); return; }
      if (!v.ok) { say(v.reason, "error"); return; }
      say(`Checking /${v.name}…`);
      debounce = window.setTimeout(() => check(v), ACC.debounceMs ?? 350);
    });

    // A name needs a plan with room, and a plan needs billing details: walk to the missing step.
    buy.addEventListener("click", async () => {
      if (!current.ok || buy.disabled) return;
      const name = current.name;
      if (ACC.hasRoom(ov)) {
        buy.disabled = true;
        try {
          await ACC.bridgeAction("/v1/names", { namespace: name });
          PP.durable.del("pp_pending_handle");
          PP.toast(`/${name} is yours.`, "success");
          ACC.forget("overview", "identities");
          PP.go("handles");
        } catch (e) { say(`Could not register /${name}: ${PP.explain(e)}`, "error"); buy.disabled = false; }
        return;
      }
      PP.durable.set("pp_pending_handle", name);
      if (ov.subscription) {
        next.replaceChildren(PP.ui.gate({ text: `Your plan's names are all registered. Choose a larger plan to add /${name}.`, tone: "info", action: { label: "Change plan", href: PP.href("plan") } }));
        return;
      }
      next.replaceChildren(PP.ui.card({ title: `Choose a plan for /${name}`, text: `${ACC.money(window.PP_CLOUD.perNameUSD, "USD")} per name per year, up to 10 names.` },
        ACC.planPicker(ov, Math.max(1, ov.plan.used + 1), { onCancel: () => next.replaceChildren() })));
    });

    const search = el("div", { class: "acc-search" },
      el("label", { class: "pp-sr", for: "acc-handle" }, "Handle"),
      el("div", { class: "acc-slash-field" }, el("span", { class: "acc-slash", "aria-hidden": "true" }, "/"), input), buy);
    const card = PP.ui.card({ title: "Find your name", text: room ? `Your plan has room for ${ov.plan.capacity - ov.plan.used} more name${ov.plan.capacity - ov.plan.used === 1 ? "" : "s"}.` : `Names come with a Pigeonpost Cloud plan: ${ACC.money(window.PP_CLOUD.perNameUSD, "USD")} per name per year, up to 10 names.` },
      search, status, next);
    // Registering into a plan with room needs no billing details; buying a plan does.
    view.append(room ? card : ACC.billingGate(ov, card));

    // Picked up again: the name chosen before sign-in, billing or a plan.
    const pending = PP.durable.get("pp_pending_handle");
    if (pending && validate(pending).ok) {
      input.value = pending;
      input.dispatchEvent(new window.Event("input"));
      if (room) PP.toast(`You're all set. Press “Register it” to add /${pending}.`);
    }
  }

  ACC.page({
    key: "handles", title: "Handles", group: "Messaging", icon: "at",
    async render(view, ctx, params) {
      const tab = params[0] === "get" ? "get" : "owned";
      const ov = await ACC.overview(tab === "owned");
      view.append(
        PP.ui.header({ title: "Handles", text: "Your Pigeonpost names, from every store.", actions: tab === "owned" ? ACC.btn("Get a handle", { kind: "primary", icon: "search", href: PP.href("handles", "get") }) : null }),
        PP.ui.tabs([{ key: "owned", label: "Your handles", count: ov.handles.length || null }, { key: "get", label: "Get a handle" }], tab, (k) => PP.go("handles", k === "get" ? "get" : "")));
      if (tab === "get") getTab(view, ov); else ownedTab(view, ov);
    },
  });
})();
