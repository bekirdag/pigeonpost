// Listings & pricing: what buyers see in the Services catalog and what they pay. Editing needs
// an approved publisher and an admin role. Prices, publish and withdraw may ask for a recent
// strong sign-in (401 step_up_required): PP.api leaves for the realm and PPP's resume replays the
// same call on return.
//   GET|POST /v1/publishers/{p}/listings                     {release_id, title, summary, description?, visibility}
//   PATCH    /v1/publishers/{p}/listings/{l}                 title, summary, description, visibility
//   PUT      /v1/publishers/{p}/listings/{l}/storefront      {category, support_url, privacy_url}
//   PUT|DELETE /v1/publishers/{p}/listings/{l}/logo          the image as the body
//   PUT|DELETE /v1/publishers/{p}/listings/{l}/price         {amount_minor, currency, term}  (step-up)
//   PUT|DELETE …/{l}/operations/{op}/price                   {amount_cents, currency: USD}   (step-up)
//   POST     /v1/publishers/{p}/listings/{l}/publish | /withdraw                         (step-up)
//   GET      /v1/marketplace/listings/{l}                    operations of a hosted listing
(function () {
  "use strict";
  const PP = window.PP, PPP = window.PPP, ui = PP.ui;
  const el = (...a) => PP.el(...a);

  const STATUS = { draft: ["Draft", "neutral"], published: ["Published", "success"], withdrawn: ["Withdrawn", "neutral"], suspended: ["Suspended by Pigeonpost", "danger"] };
  const VISIBILITY = [["public", "Public — in the Services catalog"], ["unlisted", "Unlisted — only people with the link"], ["private", "Private — only your team"]];

  function list(view, ctx, rows, approvedReviews, admin) {
    view.append(ui.header({ title: "Listings & pricing", text: "Your apps in the Services catalog: their page, logo, category and price.",
      actions: admin && approvedReviews.length ? ui.button("New listing", { kind: "primary", icon: "plus", href: PP.href("listings", "new") }) : null }));
    if (!admin) view.append(ui.gate({ tone: "info", text: "Your role can read listings but not change them." }));
    view.append(ui.table({
      caption: "Listings", search: "Search listings",
      filters: [{ key: "status", label: "Status", options: Object.entries(STATUS).map(([k, v]) => [k, v[0]]) }],
      columns: [
        { key: "title", label: "App", sort: true, render: (l) => el("span", { class: "ppp-app-cell" }, PPP.logo(l, 40), el("span", {}, el("strong", {}, l.title), el("span", { class: "pp-muted" }, l.summary || ""))) },
        { key: "status", label: "Status", render: (l) => ui.status(l.status, STATUS) },
        { key: "visibility", label: "Visibility", render: (l) => String(l.visibility || "").replace(/_/g, " ") },
        { key: "price", label: "Price", render: (l) => PPP.priceText(l) },
        { key: "version", label: "Version", mono: true },
      ],
      rows, rowHref: (l) => PP.href("listings", l.id),
      empty: { icon: "tag", title: "No listings yet", text: approvedReviews.length ? "Create a listing for an approved release." : "A release approved in review can be listed here.", action: approvedReviews.length && admin ? { label: "New listing", href: PP.href("listings", "new") } : { label: "Go to apps", href: PP.href("apps") } },
    }));
  }

  function createForm(view, ctx, approvedReviews, d, preset) {
    PP.shell.crumb("New listing");
    view.append(ui.header({ title: "New listing", text: "An approved release isn't public until you publish a listing for it." }));
    const name = (rid) => { const b = d.builds.find((x) => x.release_id === rid); const t = b && d.templates.find((x) => x.id === b.template_id); return b ? `${t ? t.name : "Release"} ${b.version}` : rid; };
    view.append(ui.card({}, ui.form([
      { name: "release_id", label: "Approved release", type: "select", required: true, value: preset || approvedReviews[0].release_id, options: approvedReviews.map((r) => [r.release_id, name(r.release_id)]) },
      { name: "visibility", label: "Who can see it", type: "select", required: true, value: "public", options: VISIBILITY },
      { name: "title", label: "Title", required: true, maxlength: 120, wide: true },
      { name: "summary", label: "Summary", required: true, maxlength: 300, wide: true, hint: "One line on the catalog card." },
      { name: "description", label: "Description", type: "textarea", rows: 6, maxlength: 5000, wide: true },
    ], { submit: "Save as draft", onSubmit: async (v) => {
      const body = { release_id: v.release_id, title: v.title, summary: v.summary, visibility: v.visibility };
      if (v.description) body.description = v.description;
      const r = await PP.postbox(`${PPP.base(ctx)}/listings`, { method: "POST", body });
      PP.toast("Listing saved as a draft. Add a logo and price, then publish it.", "success");
      PP.go("listings", r.data && r.data.id ? r.data.id : "");
    } })));
  }

  function hostedPrices(ctx, l, lpath, admin) {
    const box = el("div", { class: "ppp-prices" }, ui.loading(2));
    PP.postbox(`/v1/marketplace/listings/${PPP.enc(l.id)}`).then((r) => {
      const ops = (r.data && r.data.operations) || [];
      if (!ops.length) { box.replaceChildren(el("p", { class: "pp-muted" }, "This listing has no operations.")); return; }
      box.replaceChildren(...ops.map((op) => {
        const cur = op.price && Number(op.price.amount_cents) > 0 ? op.price : null;
        const path = `${lpath}/operations/${PPP.enc(op.name)}/price`;
        return el("div", { class: "ppp-price-row" },
          el("div", {}, el("strong", {}, op.title || op.name), el("p", { class: "pp-muted" }, cur ? `${ui.money(cur.amount_cents, "USD")} per job${cur.on_sale === false ? " (not on sale yet)" : ""}` : "Free")),
          admin ? ui.form([{ name: "amount", label: "Price per job (USD)", type: "number", min: 0.01, step: 0.01, required: true, value: cur ? (cur.amount_cents / 100).toFixed(2) : "" }], {
            submit: "Save price", cls: "ppp-inline-form",
            secondary: cur ? ui.button("Make it free", { kind: "quiet", onclick: async () => { await PPP.strong(path, "DELETE"); PP.toast("This operation is free again.", "success"); PP.shell.refresh(); } }) : null,
            onSubmit: async (v) => {
              const cents = Math.round(Number(v.amount) * 100);
              if (!Number.isInteger(cents) || cents <= 0) throw new Error("Enter a price above zero.");
              await PPP.strong(path, "PUT", { amount_cents: cents, currency: "USD" });
              PP.toast("Price saved. It goes on sale once Pigeonpost's billing holds it.", "success");
              PP.shell.refresh();
            },
          }) : null);
      }));
    }).catch((e) => { if (!(e instanceof PP.Redirecting)) box.replaceChildren(ui.errorState(e)); });
    return box;
  }

  function cardPrice(ctx, l, lpath, admin) {
    const cur = l.price || {};
    const priced = Number(cur.amount_minor) > 0;
    const nearest = el("div", { class: "ppp-nearest", role: "status" });
    if (!admin) return el("p", {}, PPP.priceText(l));
    const form = ui.form([
      { name: "amount", label: "Amount (tax included)", type: "number", min: 0.01, step: 0.01, required: true, value: priced ? (Number(cur.amount_minor) / 100).toFixed(2) : "" },
      { name: "currency", label: "Currency", type: "select", required: true, value: cur.currency || "USD", options: [["USD", "USD"], ["EUR", "EUR"], ["TRY", "TRY"]] },
      { name: "term", label: "Billing", type: "select", required: true, value: cur.term === "monthly" ? "monthly" : "one_off", options: [["one_off", "One-off"], ["monthly", "Monthly"]] },
    ], {
      submit: "Save price",
      secondary: priced ? ui.button("Make it free", { kind: "quiet", onclick: async () => { await PPP.strong(`${lpath}/price`, "DELETE"); PP.toast("The listing is free again.", "success"); PP.shell.refresh(); } }) : null,
      onSubmit: async (v) => send(Math.round(Number(v.amount) * 100), v.currency, v.term),
    });
    async function send(minor, currency, term) {
      if (!Number.isInteger(minor) || minor <= 0) throw new Error("Enter a price above zero.");
      nearest.replaceChildren();
      try {
        await PPP.strong(`${lpath}/price`, "PUT", { amount_minor: minor, currency, term });
      } catch (e) {
        if (e && e.code === "price_not_invoiceable") {
          const near = ((e.body && e.body.nearest) || []).filter((n) => Number.isInteger(n) && n > 0).slice(0, 4);
          if (near.length) nearest.append(el("span", {}, "That amount can't be invoiced exactly with tax. Pick one: "), ...near.map((n) => ui.button(ui.money(n, currency), { size: "sm", onclick: () => send(n, currency, term) })));
        }
        throw e;
      }
      PP.toast("Price saved. It goes on sale once Pigeonpost's billing holds it.", "success");
      PP.shell.refresh();
      return "Saved.";
    }
    return el("div", { class: "ppp-prices" }, el("p", { class: "pp-muted" }, `Now: ${PPP.priceText(l)}.`), form, nearest);
  }

  async function detail(view, ctx, id, admin) {
    const rows = (await PP.postbox(`${PPP.base(ctx)}/listings`)).data || [];
    const l = rows.find((x) => x.id === id);
    if (!l) { view.append(ui.empty({ icon: "tag", title: "Listing not found", text: "It may have been removed.", action: { label: "All listings", href: PP.href("listings") } })); return; }
    PP.shell.crumb(l.title);
    const lpath = `${PPP.base(ctx)}/listings/${PPP.enc(l.id)}`;
    const canPublish = admin && (l.status === "draft" || l.status === "withdrawn");
    view.append(ui.header({ eyebrow: "Listing", title: l.title, text: l.summary,
      actions: [
        ui.status(l.status, STATUS),
        canPublish ? ui.button("Publish", { kind: "primary", icon: "check", onclick: async () => {
          await PPP.strong(`${lpath}/publish`, "POST", {});
          PP.toast(`${l.title} is published.`, "success");
          PP.shell.refresh();
        } }) : null,
        admin && l.status === "published" ? ui.button("Withdraw", { kind: "danger", onclick: async () => {
          if (!(await ui.confirm({ title: `Withdraw ${l.title}?`, text: "It leaves the catalog. Existing installations keep working.", confirm: "Withdraw", danger: true }))) return;
          await PPP.strong(`${lpath}/withdraw`, "POST", {});
          PP.toast(`${l.title} is withdrawn.`, "success");
          PP.shell.refresh();
        } }) : null,
      ] }));
    if (l.moderation_reason) view.append(ui.gate({ tone: "danger", text: `Pigeonpost: ${l.moderation_reason}`, action: { label: "Contact support", href: PP.href("support") } }));
    if (canPublish) view.append(ui.gate({ tone: "info", text: "Publishing and prices ask you to confirm it's you with two-step sign-in." }));

    const ro = !admin;
    const listingForm = ro ? ui.kv([["Title", l.title], ["Summary", l.summary], ["Description", l.description], ["Visibility", l.visibility]]) : ui.form([
      { name: "title", label: "Title", required: true, maxlength: 120, value: l.title, wide: true, disabled: ro },
      { name: "summary", label: "Summary", required: true, maxlength: 300, value: l.summary, wide: true, disabled: ro },
      { name: "description", label: "Description", type: "textarea", rows: 7, maxlength: 5000, value: l.description || "", wide: true, disabled: ro },
      { name: "visibility", label: "Who can see it", type: "select", required: true, value: l.visibility, options: VISIBILITY, disabled: ro },
    ], { submit: "Save listing", onSubmit: ro ? null : async (v) => {
      const patch = {};
      for (const k of ["title", "summary", "visibility"]) if (v[k] !== (l[k] || "")) patch[k] = v[k];
      if (v.description !== (l.description || "")) patch.description = v.description;
      if (!Object.keys(patch).length) return "Nothing changed.";
      await PP.postbox(lpath, { method: "PATCH", body: patch });
      PP.toast("Listing saved", "success");
      PP.shell.refresh();
    } });
    const storeForm = ro ? ui.kv([["Category", (PPP.CATEGORIES.find(([k]) => k === l.category) || [null, ""])[1]], ["Support link", l.support_url], ["Privacy policy", l.privacy_url]]) : ui.form([
      { name: "category", label: "Category", type: "select", value: l.category || "", options: [["", "Choose a category"], ...PPP.CATEGORIES], disabled: ro },
      { name: "support_url", label: "Support link", type: "url", value: l.support_url || "", placeholder: "https://", pattern: "https://\\S+", patternHint: "An https:// link.", disabled: ro },
      { name: "privacy_url", label: "Privacy policy link", type: "url", value: l.privacy_url || "", placeholder: "https://", pattern: "https://\\S+", patternHint: "An https:// link.", disabled: ro },
    ], { submit: "Save storefront", onSubmit: ro ? null : async (v) => {
      await PP.postbox(`${lpath}/storefront`, { method: "PUT", body: { category: v.category || null, support_url: v.support_url || null, privacy_url: v.privacy_url || null } });
      PP.toast("Storefront saved", "success");
      return "Saved.";
    } });
    const logoStatus = el("p", { class: "pp-form-status", role: "status", "aria-live": "polite" });
    const logo = el("div", { class: "ppp-logo-edit" },
      PPP.logo(l, 96),
      el("div", { class: "ppp-logo-side" },
        el("p", { class: "pp-muted" }, "Square PNG, JPEG or WebP, at least 256 × 256 (512 × 512 looks best), up to 1 MB. Without one, your initials are shown."),
        admin ? ui.dropzone({ label: "Drop a logo here, or choose an image", accept: "image/png,image/jpeg,image/webp", onFiles: async (files) => {
          const f = files[0];
          logoStatus.className = "pp-form-status";
          if (!["image/png", "image/jpeg", "image/webp"].includes(f.type)) { logoStatus.textContent = "A logo is a PNG, JPEG or WebP image."; logoStatus.classList.add("is-error"); return; }
          if (f.size > 1024 * 1024) { logoStatus.textContent = "A logo is at most 1 MB."; logoStatus.classList.add("is-error"); return; }
          try {
            await PP.postbox(`${lpath}/logo`, { method: "PUT", blob: f, contentType: f.type });
            PP.toast("Logo saved", "success");
            PP.shell.refresh();
          } catch (e) { if (!(e instanceof PP.Redirecting)) { logoStatus.textContent = PP.explain(e); logoStatus.classList.add("is-error"); } }
        } }) : null,
        admin && l.logo ? ui.button("Use the default", { size: "sm", kind: "quiet", onclick: async () => { await PP.postbox(`${lpath}/logo`, { method: "DELETE" }); PP.shell.refresh(); } }) : null,
        logoStatus));

    const hosted = l.hosting === "publisher";
    view.append(el("div", { class: "pp-cols" },
      el("div", { class: "pp-view-inner" },
        ui.card({ title: "Listing", text: "The page buyers see in the Services catalog." }, listingForm),
        ui.card({ title: "Storefront", text: "Category and the links buyers use for help and privacy." }, storeForm)),
      el("div", { class: "pp-view-inner" },
        ui.card({ title: "Logo" }, logo),
        ui.card({ title: hosted ? "Price per job" : "Price", text: hosted ? "Each operation is priced per job in US dollars and paid from the buyer's credit. Pigeonpost keeps 10% of each job." : "Prices include tax. Piyote sells as merchant of record and keeps 10%." },
          hosted ? hostedPrices(ctx, l, lpath, admin) : cardPrice(ctx, l, lpath, admin)),
        ui.card({ title: "Facts" }, ui.kv([["Version", l.version], ["Runs", hosted ? "On your servers (publisher-hosted)" : "In the buyer's Nest"], ["Address", l.address || l.service_address], ["Published", ui.when(l.published_at)], ["Updated", ui.when(l.updated_at)]])))));
  }

  PPP.page({
    key: "listings", title: "Listings & pricing", group: "My business", icon: "tag", order: 4,
    async render(view, ctx, params) {
      if (!ctx.pub) { view.append(ui.header({ title: "Listings & pricing" }), PPP.noPublisher(ctx, "Listing and pricing apps")); return; }
      if (!PPP.approved(ctx)) {
        view.append(ui.header({ title: "Listings & pricing", text: "Your apps in the Services catalog: their page, logo, category and price." }),
          PPP.gate(ctx, "Finish your application first. Listings, prices and publishing open once your business is approved.",
            ui.table({ columns: [{ key: "a", label: "App" }, { key: "b", label: "Status" }, { key: "c", label: "Price" }], rows: [], empty: { icon: "tag", title: "No listings yet" } })));
        return;
      }
      const admin = PPP.isAdmin(ctx);
      const base = PPP.base(ctx);
      if (params[0] && params[0] !== "new") { await detail(view, ctx, params[0], admin); return; }
      const get = (sub) => PP.postbox(base + sub).then((r) => r.data || []);
      const [rows, reviews, builds, templates] = await Promise.all([get("/listings"), get("/reviews"), get("/builds").catch(() => []), get("/templates").catch(() => [])]);
      const approvedReviews = reviews.filter((r) => r.status === "approved" && (r.allowed_actions ? r.allowed_actions.includes("publish_listing") : true));
      if (params[0] === "new") {
        if (!admin || !approvedReviews.length) { view.append(ui.header({ title: "New listing" }), ui.empty({ icon: "review", title: "No approved release to list", text: "A release must pass review before it can be listed.", action: { label: "Go to apps", href: PP.href("apps", "reviews") } })); return; }
        createForm(view, ctx, approvedReviews, { builds, templates }, params[1]);
        return;
      }
      list(view, ctx, rows, approvedReviews, admin);
    },
  });
})();
