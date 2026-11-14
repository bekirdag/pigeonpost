// Apps: templates, builds of a pinned commit, publisher-hosted integrations, releases in review
// (stages and redacted findings) and test Nests. Writes need an approved publisher and an admin
// role; finance roles read.
//   GET|POST /v1/publishers/{p}/templates               {slug, name}
//   GET|POST /v1/publishers/{p}/builds                  {template_id, version, source:{repository, commit}}
//   POST     /v1/publishers/{p}/integrations            {template_id, version, service_address, operations}
//   GET      /v1/publishers/{p}/reviews[/{r}[/findings]], POST …/{r}/withdraw
//   GET|POST /v1/publishers/{p}/test-nests, DELETE …/{t}, POST …/{t}/reset   (staging postbox only)
(function () {
  "use strict";
  const PP = window.PP, PPP = window.PPP, ui = PP.ui;
  const el = (...a) => PP.el(...a);

  const BUILD = { queued: ["Queued", "neutral"], building: ["Building", "info"], built: ["Built", "success"], failed: ["Failed", "danger"], cancelled: ["Cancelled", "neutral"] };
  const REVIEW = { submitted: ["Submitted", "info"], in_review: ["In review", "info"], changes_requested: ["Changes requested", "warning"], approved: ["Approved", "success"], rejected: ["Rejected", "danger"], withdrawn: ["Withdrawn", "neutral"] };
  const STAGE = { intake: "Intake", deterministic_scan: "Automated scan", ai_analysis: "AI review (advisory)", behavioral_test: "Behaviour test", human_review: "Staff review", attestation: "Attestation" };
  const STAGE_STATUS = { pending: ["Waiting", "neutral"], passed: ["Passed", "success"], failed: ["Failed", "danger"], blocked: ["Changes requested", "warning"] };
  const TEST = { active: ["Active", "success"], provisioning: ["Starting", "info"], expired: ["Expired", "neutral"], deleted: ["Deleted", "neutral"], failed: ["Failed", "danger"] };

  const EXAMPLE_OPS = JSON.stringify([{
    name: "research.report", version: "1.0", title: "Research report", description: "A PDF report on one topic.",
    input_schema: { fields: [{ name: "topic", label: "Topic", max_chars: 500, required: true, multiline: true }] },
    output: { kind: "file", media_type: "application/pdf" }, deadline_hours: 72,
  }], null, 2);

  async function load(ctx) {
    const base = PPP.base(ctx);
    const get = (sub) => PP.postbox(base + sub).then((r) => r.data || []).catch((e) => { if (e instanceof PP.Redirecting) throw e; return null; });
    const [templates, builds, reviews, listings, tests] = await Promise.all([get("/templates"), get("/builds"), get("/reviews"), get("/listings"), get("/test-nests")]);
    return { templates: templates || [], builds: builds || [], reviews: reviews || [], listings: listings || [], tests };
  }

  function releaseName(d, releaseId) {
    const b = d.builds.find((x) => x.release_id === releaseId);
    if (!b) { const l = d.listings.find((x) => x.release_id === releaseId); return l ? `${l.title} ${l.version}` : releaseId; }
    const t = d.templates.find((x) => x.id === b.template_id);
    return `${t ? t.name : "Release"} ${b.version}`;
  }

  function templatesTab(ctx, d, admin) {
    const out = [ui.section({ title: "Templates", text: "A template is one package you publish. Each build or integration is a version of it." },
      ui.table({ caption: "Templates", columns: [{ key: "name", label: "Name", sort: true }, { key: "slug", label: "Package name", mono: true }, { key: "created_at", label: "Created", render: (r) => ui.when(r.created_at) }], rows: d.templates, empty: { icon: "apps", title: "No templates yet", text: admin ? "Create your first one below." : "A publisher admin creates templates." } }))];
    if (admin) out.push(ui.card({ title: "New template" }, ui.form([
      { name: "name", label: "Name", required: true, maxlength: 80, placeholder: "Research assistant" },
      { name: "slug", label: "Package name", required: true, maxlength: 64, pattern: "[a-z0-9-]+", patternHint: "Lowercase letters, digits and hyphens.", hint: "Must match the name in nest-release.toml." },
    ], { submit: "Create template", onSubmit: async (v) => {
      await PP.postbox(`${PPP.base(ctx)}/templates`, { method: "POST", body: { slug: v.slug, name: v.name } });
      PP.toast("Template created", "success");
      PP.shell.refresh();
    } })));
    return out;
  }

  function buildsTab(ctx, d, admin) {
    const out = [ui.section({ title: "Builds", text: "Pigeonpost builds exactly the commit you name, then starts its review." },
      ui.table({
        caption: "Builds", search: "Search builds", sort: { key: "created_at", dir: "desc" },
        columns: [
          { key: "version", label: "Release", sort: true, render: (b) => { const t = d.templates.find((x) => x.id === b.template_id); return el("strong", {}, `${t ? t.name : b.template_id} ${b.version}`); } },
          { key: "status", label: "Status", render: (b) => ui.status(b.status, BUILD) },
          { key: "commit", label: "Commit", mono: true, render: (b) => (b.source && b.source.commit ? b.source.commit.slice(0, 12) : "—") },
          { key: "failure", label: "Note", render: (b) => (b.failure ? `${b.failure.code}${b.failure.reason ? `: ${b.failure.reason}` : ""}` : "") },
          { key: "created_at", label: "Requested", sort: true, render: (b) => ui.when(b.created_at) },
          { key: "review", label: "", render: (b) => (b.review_id ? ui.button("Open review", { size: "sm", kind: "quiet", href: PP.href("apps", "review", b.review_id) }) : null) },
        ],
        rows: d.builds, empty: { icon: "server", title: "No builds yet", text: admin ? "Submit a build of a pinned commit below." : "" },
      }))];
    if (admin && d.templates.length) {
      out.push(ui.card({ title: "Submit a build" }, ui.form([
        { name: "template_id", label: "Template", type: "select", required: true, value: d.templates[0].id, options: d.templates.map((t) => [t.id, t.name]) },
        { name: "version", label: "Version", required: true, placeholder: "1.0.0", pattern: "\\d+\\.\\d+\\.\\d+", patternHint: "Use major.minor.patch, such as 1.0.0." },
        { name: "repository", label: "Repository URL", type: "url", required: true, wide: true, pattern: "https://\\S+", patternHint: "An https:// address.", hint: "An https address Pigeonpost can read without signing in." },
        { name: "commit", label: "Commit", required: true, wide: true, pattern: "[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", patternHint: "The full 40- or 64-character commit hash.", hint: "The full hash, so the build is exactly what you tested." },
      ], { submit: "Submit build", onSubmit: async (v) => {
        await PP.postbox(`${PPP.base(ctx)}/builds`, { method: "POST", body: { template_id: v.template_id, version: v.version, source: { repository: v.repository, commit: v.commit.toLowerCase() } } });
        PP.toast("Build requested. Pigeonpost builds it and starts the review.", "success");
        PP.shell.refresh();
      } })));
    } else if (admin) out.push(ui.gate({ tone: "info", text: "Create a template first.", action: { label: "Templates", href: PP.href("apps", "templates") } }));
    return out;
  }

  function integrationsTab(ctx, d, admin) {
    const out = [ui.section({ title: "Hosted integrations", text: "A publisher-hosted app runs on your own servers. Jobs reach your service mailbox as Pigeonpost messages and results come back as PDFs. Pigeonpost reviews the integration (address and operations), not your code." },
      ui.card({ tone: "accent" }, el("p", {}, "Each operation is priced per job in US dollars and paid from the buyer's credit; Pigeonpost keeps 10%. A new address or operation is a new version and a new review."),
        el("p", {}, el("a", { href: "https://developers.pigeonpost.dev/nest-hosted.html", target: "_blank", rel: "noopener" }, "How publisher-hosted apps work →"))))];
    if (!admin) return out;
    if (!d.templates.length) { out.push(ui.gate({ tone: "info", text: "Create a template first.", action: { label: "Templates", href: PP.href("apps", "templates") } })); return out; }
    out.push(ui.card({ title: "Submit an integration" }, ui.form([
      { name: "template_id", label: "Template", type: "select", required: true, value: d.templates[0].id, options: d.templates.map((t) => [t.id, t.name]) },
      { name: "version", label: "Version", required: true, placeholder: "1.0.0", pattern: "\\d+\\.\\d+\\.\\d+", patternHint: "Use major.minor.patch." },
      { name: "service_address", label: "Service mailbox", required: true, placeholder: "/acme/research", pattern: "/\\S+", patternHint: "A Pigeonpost address such as /acme/research.", hint: "A mailbox your business owns. Not under /pigeonpost/." },
      { name: "operations", label: "Operations (JSON)", type: "textarea", rows: 14, wide: true, required: true, value: EXAMPLE_OPS, hint: "1 to 20 operations. Fields have a name, max_chars and required; the output is a PDF file.",
        validate: (v) => { try { const a = JSON.parse(v); return Array.isArray(a) && a.length ? "" : "A list of at least one operation."; } catch { return "That isn't valid JSON."; } } },
    ], { submit: "Submit for review", onSubmit: async (v) => {
      await PP.postbox(`${PPP.base(ctx)}/integrations`, { method: "POST", body: { template_id: v.template_id, version: v.version, service_address: v.service_address, operations: JSON.parse(v.operations) } });
      PP.toast("Integration submitted. Its conformance run and review start now.", "success");
      PP.go("apps", "reviews");
    } })));
    return out;
  }

  function reviewsTab(ctx, d) {
    return [ui.section({ title: "Reviews", text: "Every release is reviewed before anyone can install it: intake, automated scan, behaviour test, staff review and attestation." },
      ui.table({
        caption: "Reviews", filters: [{ key: "status", label: "Status", options: Object.entries(REVIEW).map(([k, v]) => [k, v[0]]) }],
        columns: [
          { key: "release_id", label: "Release", render: (r) => el("strong", {}, releaseName(d, r.release_id)) },
          { key: "kind", label: "Kind", render: (r) => (r.kind === "integration" ? "Hosted integration" : "Nest build") },
          { key: "status", label: "Status", render: (r) => ui.status(r.status, REVIEW) },
          { key: "updated_at", label: "Updated", sort: true, render: (r) => ui.when(r.updated_at) },
        ],
        rows: d.reviews, rowHref: (r) => PP.href("apps", "review", r.review_id),
        empty: { icon: "review", title: "Nothing in review", text: "Submit a build or an integration to start a review." },
      }))];
  }

  async function reviewDetail(view, ctx, d, id, admin) {
    const base = `${PPP.base(ctx)}/reviews/${PPP.enc(id)}`;
    const r = (await PP.postbox(base)).data;
    const name = releaseName(d, r.release_id);
    PP.shell.crumb(name);
    const allowed = r.allowed_actions || [];
    const findingsBox = el("div", { class: "ppp-findings" });
    view.append(ui.header({ eyebrow: "Review", title: name, text: `${(REVIEW[r.status] || [r.status])[0]} · revision ${r.revision}`,
      actions: [
        ui.button("All apps", { href: PP.href("apps", "reviews") }),
        admin && allowed.includes("withdraw") ? ui.button("Withdraw from review", { onclick: async () => {
          if (!(await ui.confirm({ title: "Withdraw this release from review?", confirm: "Withdraw", danger: true }))) return;
          await PP.postbox(`${base}/withdraw`, { method: "POST", body: {} });
          PP.toast("Withdrawn from review", "success");
          PP.shell.refresh();
        } }) : null,
        admin && allowed.includes("publish_listing") ? ui.button("Create a listing", { kind: "primary", href: PP.href("listings", "new", r.release_id) }) : null,
        admin && allowed.includes("resubmit") ? ui.button("Submit a fixed build", { kind: "primary", href: PP.href("apps", r.kind === "integration" ? "integrations" : "builds") }) : null,
      ] }));
    view.append(ui.card({ title: "Stages" }, el("ol", { class: "ppp-stages" }, (r.stages || []).map((g) => el("li", { class: `ppp-stage is-${g.status}` },
      el("span", { class: "ppp-stage-dot" }, g.status === "passed" ? PP.icon("check") : g.status === "failed" || g.status === "blocked" ? PP.icon("alert") : null),
      el("div", {}, el("strong", {}, STAGE[g.name] || g.name), g.reason ? el("p", { class: "pp-muted" }, g.reason) : null),
      ui.status(g.status, STAGE_STATUS))))));
    if (allowed.includes("view_findings")) {
      view.append(ui.card({ title: "Findings", text: "Redacted: behaviour-test findings show only which rule fired.", actions: ui.button("Show findings", { size: "sm", onclick: async () => {
        const f = (await PP.postbox(`${base}/findings`)).data || {};
        findingsBox.replaceChildren();
        if (!(f.stages || []).length) { findingsBox.append(el("p", { class: "pp-muted" }, "No findings recorded yet.")); return; }
        for (const g of f.stages) {
          findingsBox.append(el("div", { class: "ppp-finding" },
            el("p", {}, el("strong", {}, STAGE[g.stage] || g.stage), ` · ${g.decision || ""}`),
            g.reason ? el("p", { class: "pp-muted" }, g.reason) : null,
            g.stage === "ai_analysis" ? el("p", { class: "pp-faint" }, `${g.advisory_findings || 0} advisory note(s) for staff; they don't decide the review.`) : null,
            (g.findings || []).length ? el("ul", {}, g.findings.map((x) => el("li", {}, el("code", {}, x.rule), x.path ? ` in ${x.path}` : "", x.detail ? ` · ${x.detail}` : "", x.blocker ? el("strong", {}, " · blocks the release") : null))) : null));
        }
      } }) }, findingsBox));
    }
  }

  function testsTab(ctx, d, admin) {
    if (d.tests === null) {
      return [ui.empty({ icon: "nest", title: "Test Nests run on the staging postbox", text: "Try a release that passed intake in a private test Nest before it is approved. Test Nests aren't available on this postbox." })];
    }
    const candidates = d.builds.filter((b) => b.release_id);
    const out = [ui.section({ title: "Test Nests", text: "A private Nest that runs one of your releases with your own test jobs. No payment, no buyers; it expires after a week." },
      ui.table({
        caption: "Test Nests",
        columns: [
          { key: "label", label: "Name", render: (t) => el("strong", {}, t.label || t.id) },
          { key: "version", label: "Version" },
          { key: "status", label: "Status", render: (t) => ui.status(t.status, TEST) },
          { key: "expires_at", label: "Expires", render: (t) => ui.when(t.expires_at) },
          admin ? { key: "x", label: "", render: (t) => t.status === "active" ? el("span", { class: "ppp-row-actions" },
            ui.button("Reset", { size: "sm", kind: "quiet", onclick: async () => { await PP.postbox(`${PPP.base(ctx)}/test-nests/${PPP.enc(t.id)}/reset`, { method: "POST", body: {} }); PP.toast("Reset. The next job starts on a fresh Nest.", "success"); PP.shell.refresh(); } }),
            ui.button("Delete", { size: "sm", kind: "quiet", onclick: async () => {
              if (!(await ui.confirm({ title: "Delete this test Nest?", text: "Its data is removed.", confirm: "Delete", danger: true }))) return;
              await PP.postbox(`${PPP.base(ctx)}/test-nests/${PPP.enc(t.id)}`, { method: "DELETE" }); PP.shell.refresh();
            } })) : null } : null,
        ].filter(Boolean),
        rows: d.tests, empty: { icon: "nest", title: "No test Nests", text: admin && candidates.length ? "Start one below." : "Submit a build first; a release that passed intake can be tested." },
      }))];
    if (admin && candidates.length) {
      out.push(ui.card({ title: "Start a test Nest" }, ui.form([
        { name: "build_id", label: "Release", type: "select", required: true, value: candidates[0].id, options: candidates.map((b) => [b.id, releaseName(d, b.release_id)]) },
        { name: "label", label: "Name", maxlength: 80, placeholder: "Smoke test" },
        { name: "days", label: "Days", type: "number", min: 1, max: 7, value: 7 },
      ], { submit: "Start test Nest", onSubmit: async (v) => {
        const body = { build_id: v.build_id };
        if (v.label) body.label = v.label;
        if (v.days) body.days = Number(v.days);
        await PP.postbox(`${PPP.base(ctx)}/test-nests`, { method: "POST", body });
        PP.toast("Test Nest starting", "success");
        PP.shell.refresh();
      } })));
    }
    return out;
  }

  const TABS = [["templates", "Templates"], ["builds", "Builds"], ["integrations", "Integrations"], ["reviews", "Reviews"], ["tests", "Test Nests"]];

  PPP.page({
    key: "apps", title: "Apps", group: "My business", icon: "apps", order: 3,
    async render(view, ctx, params) {
      if (!ctx.pub) { view.append(ui.header({ title: "Apps" }), PPP.noPublisher(ctx, "Publishing apps")); return; }
      if (!PPP.approved(ctx)) {
        view.append(ui.header({ title: "Apps", text: "Build, review and release your apps." }),
          PPP.gate(ctx, "Finish your application first. You can submit apps once your business is approved.",
            ui.stats([{ label: "Templates", value: 0 }, { label: "Builds", value: 0 }, { label: "In review", value: 0 }, { label: "Published", value: 0 }]),
            ui.tabs(TABS.map(([key, label]) => ({ key, label })), "templates", () => {})));
        return;
      }
      const d = await load(ctx);
      const admin = PPP.isAdmin(ctx);
      if (params[0] === "review" && params[1]) { await reviewDetail(view, ctx, d, params[1], admin); return; }
      const tab = TABS.some(([k]) => k === params[0]) ? params[0] : "builds";
      view.append(ui.header({ title: "Apps", text: "Templates, builds, hosted integrations, reviews and test Nests.", actions: admin ? ui.button("New build", { kind: "primary", icon: "plus", href: PP.href("apps", "builds") }) : null }));
      if (!admin) view.append(ui.gate({ tone: "info", text: "Your role can read this publisher but not change it." }));
      const inReview = d.reviews.filter((r) => ["submitted", "in_review"].includes(r.status)).length;
      view.append(ui.stats([
        { label: "Templates", value: d.templates.length, href: PP.href("apps", "templates") },
        { label: "Builds", value: d.builds.length, href: PP.href("apps", "builds") },
        { label: "In review", value: inReview, href: PP.href("apps", "reviews"), tone: inReview ? "info" : null },
        { label: "Published listings", value: d.listings.filter((l) => l.status === "published").length, href: PP.href("listings") },
      ]));
      view.append(ui.tabs(TABS.map(([key, label]) => ({ key, label, count: key === "reviews" ? inReview : key === "tests" && d.tests ? d.tests.filter((t) => t.status === "active").length : 0 })), tab, (k) => PP.go("apps", k)));
      const parts = { templates: templatesTab, builds: buildsTab, integrations: integrationsTab, reviews: reviewsTab, tests: testsTab }[tab](ctx, d, admin);
      view.append(...parts);
    },
  });
})();
