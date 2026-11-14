// Business: the business details the application holds, the public directory profile and the
// team. Another business (for an organization) can be started here too.
//   PUT  /v1/publisher-applications/{p}/dossier        public profile while the draft is open
//   GET|POST /v1/publishers/{p}/roles, DELETE …/roles/{id}   an organization publisher's team
(function () {
  "use strict";
  const PP = window.PP, PPP = window.PPP, ui = PP.ui;
  const el = (...a) => PP.el(...a);

  const mask = (v, keep = 4) => { const s = String(v || "").replace(/\s/g, ""); return s ? `•••• ${s.slice(-keep)}` : ""; };

  function details(ctx) {
    const d = (ctx.app && ctx.app.details) || {};
    const editable = PPP.editable(ctx);
    return [
      ui.card({ title: "Business", text: "From your application. Pigeonpost uses these for contracts, invoices and payouts.", actions: editable ? ui.button("Edit in application", { size: "sm", href: PP.href("application", "legal") }) : ui.button("Ask to change", { size: "sm", href: PP.href("support") }) },
        ui.kv([
          ["Publisher name", el("span", { translate: "no" }, ctx.pub.display_name)],
          ["Publishes as", ctx.pub.owner_kind === "org" ? "An organization" : "A person or their own company"],
          ["Your role", PPP.ROLE_TEXT[ctx.pub.role] || ctx.pub.role],
          ["Status", PPP.stateBadge(PPP.stateOf(ctx))],
          ["Type", d.kind === "individual" ? "Individual developer" : d.kind === "company" ? "Company" : ""],
          ["Legal name", d.legal_name], d.kind === "company" ? ["Registration no.", d.registration_number] : null,
          ["Tax ID", d.tax_id], d.duns ? ["DUNS", d.duns] : null, ["Country", d.country], ["Address", d.address],
          ["Contact", d.contact_name ? `${d.contact_name} · ${d.contact_email}` : ""],
        ])),
      ui.card({ title: "Payout account", text: "Private. Only you and Pigeonpost finance see these.", actions: editable ? ui.button("Edit", { size: "sm", href: PP.href("application", "bank") }) : null },
        ui.kv([["Bank", [d.bank_name, d.bank_country].filter(Boolean).join(" · ")], ["Account holder", d.account_holder], ["Account", d.iban ? `IBAN ${mask(d.iban)}` : d.account_number ? `${mask(d.account_number)} · ${d.swift}` : ""], ["Payout currency", d.currency]])),
    ];
  }

  function profile(ctx) {
    const d = (ctx.app && ctx.app.details) || {};
    const editable = PPP.editable(ctx);
    const out = [];
    if (PPP.approved(ctx)) out.push(ui.gate({ tone: "success", text: "Your public partner page is live in the Pigeonpost partner directory.", action: ui.button("Open your public page", { href: `https://pigeonpost.dev/partners.html?publisher=${PPP.enc(ctx.pub.id)}`, icon: "external" }) }));
    if (!editable) {
      out.push(ui.card({ title: "Public profile", text: PPP.approved(ctx) ? "To change your public profile after approval, send the new text to support; it is reviewed like the application." : "Locked while Pigeonpost reviews your application." },
        ui.kv([["Public name", d.public_name], ["About", d.public_summary], ["Website", d.website ? el("a", { href: d.website, target: "_blank", rel: "noopener" }, d.website) : ""]])));
      return out;
    }
    out.push(ui.card({ title: "Public profile", text: "What buyers see on your partner page and next to your apps, once you are approved." },
      ui.form([
        { name: "public_name", label: "Public name", required: true, maxlength: 256, value: d.public_name || ctx.pub.display_name },
        { name: "website", label: "Website", type: "url", value: d.website, placeholder: "https://", pattern: "https://\\S+", patternHint: "Use an https:// address." },
        { name: "public_summary", label: "About your work", type: "textarea", rows: 4, wide: true, value: d.public_summary, maxlength: 2000 },
      ], {
        submit: "Save profile",
        onSubmit: async (v) => {
          const details = PPP.cleanDetails({ ...ctx.app.details, ...v });
          const r = await PP.postbox(`${PPP.appBase(ctx)}/dossier`, { method: "PUT", body: { revision: ctx.app.revision, details } });
          PPP.setApp(ctx, r.data);
          return "Saved.";
        },
      })));
    return out;
  }

  async function team(ctx) {
    const p = ctx.pub;
    if (p.owner_kind !== "org") {
      return [ui.card({ title: "Team", text: "A personal publisher is just you." },
        el("p", { class: "pp-muted" }, "To share the work with colleagues, start a business for your organization below. Its owner and admins act for it, and you can give members the publisher admin or finance role."))];
    }
    const r = await PP.postbox(`${PPP.base(ctx)}/roles`).catch((e) => { if (e instanceof PP.Redirecting) throw e; return { data: [], error: e }; });
    const rows = r.data || [];
    const manage = p.role === "org_admin" || p.role === "owner";
    const out = [ui.card({ title: "Team & roles", text: "The organization's owner and admins always act for this publisher. Publisher admins can submit apps, publish and set prices; publisher finance can only read." },
      ui.table({
        caption: "Publisher roles",
        empty: { icon: "people", title: "No publisher roles given yet", text: manage ? "Give a member a role below." : "An organization admin can give you or a colleague a role." },
        columns: [
          { key: "account_id", label: "Member", mono: true },
          { key: "role", label: "Role", render: (x) => PPP.ROLE_TEXT[x.role] || x.role },
          { key: "created_at", label: "Since", render: (x) => ui.when(x.created_at) },
          manage ? { key: "x", label: "", render: (x) => ui.button("Remove", { size: "sm", kind: "quiet", onclick: async () => {
            if (!(await ui.confirm({ title: "Remove this role?", text: `${x.account_id} will lose ${PPP.ROLE_TEXT[x.role] || x.role}.`, confirm: "Remove", danger: true }))) return;
            await PP.postbox(`${PPP.base(ctx)}/roles/${PPP.enc(x.id)}`, { method: "DELETE" });
            PP.toast("Role removed", "success");
            PP.shell.refresh();
          } }) } : null,
        ].filter(Boolean),
        rows,
      }))];
    if (manage) {
      out.push(ui.card({ title: "Give a role" }, ui.form([
        { name: "account_id", label: "Member's account ID", required: true, hint: "The colleague must already be an active member of the organization." },
        { name: "role", label: "Role", type: "select", required: true, value: "publisher_admin", options: [["publisher_admin", PPP.ROLE_TEXT.publisher_admin], ["publisher_finance", PPP.ROLE_TEXT.publisher_finance]] },
      ], { submit: "Give role", onSubmit: async (v) => {
        await PP.postbox(`${PPP.base(ctx)}/roles`, { method: "POST", body: { account_id: v.account_id, role: v.role } });
        PP.toast("Role given", "success");
        PP.shell.refresh();
      } })));
    }
    return out;
  }

  PPP.page({
    key: "business", title: "Business", group: "My business", icon: "building", order: 2,
    async render(view, ctx, params) {
      view.append(ui.header({ title: "Business", text: "Your business details, public profile and team.", actions: PPP.picker(ctx) }));
      if (!ctx.pub) { view.append(PPP.noPublisher(ctx, "Your business profile")); return; }
      const tab = ["details", "profile", "team"].includes(params[0]) ? params[0] : "details";
      view.append(ui.tabs([{ key: "details", label: "Details" }, { key: "profile", label: "Public profile" }, { key: "team", label: "Team & roles" }], tab, (k) => PP.go("business", k)));
      if (tab === "details") view.append(el("div", { class: "pp-cols" }, ...details(ctx)));
      if (tab === "profile") view.append(...profile(ctx));
      if (tab === "team") view.append(...(await team(ctx)));
      view.append(ui.section({ title: "Add another business", text: ctx.orgs.length ? "Apply for an organization you administer, or a second business of your own." : "Apply for a second business of your own. To apply for an organization, be its owner or admin." },
        ui.card({}, PPP.startForm(ctx, { compact: true }))));
    },
  });
})();
