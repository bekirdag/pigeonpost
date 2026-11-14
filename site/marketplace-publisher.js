// Publisher console on the account page (build plan R-47, R-48; readiness W3 item 7): apply as a
// publisher for your account or an organization, the team's publisher roles, templates, builds of a
// pinned commit, review stages and redacted findings, and listings (edit, publish with step-up,
// withdraw). Earnings and payouts come with paid launch.
//
// Calls (all through deps.pb):
//   GET  /v1/publishers                                   publishers this person acts for, with role
//   POST /v1/publisher-applications                       {display_name, org_id?}
//   GET  /v1/orgs                                         orgs this person can apply for
//   GET|POST /v1/publishers/{p}/roles, DELETE …/roles/{id}  org publishers' team
//   GET|POST /v1/publishers/{p}/templates                 {slug, name}
//   GET|POST /v1/publishers/{p}/builds                    {template_id, version, source:{repository, commit}}
//   GET  /v1/publishers/{p}/reviews[/{r}[/findings]]      NestReviewStatus and redacted findings
//   POST /v1/publishers/{p}/reviews/{r}/withdraw
//   GET|POST /v1/publishers/{p}/listings, PATCH …/{l}, POST …/{l}/publish (step-up), POST …/{l}/withdraw
//   PUT|DELETE /v1/publishers/{p}/listings/{l}/logo   the listing's logo (the image as the body)
//   PUT|DELETE /v1/publishers/{p}/listings/{l}/price (step-up) {amount_minor, currency, term}: a
//     price the financial authority cannot invoice is refused with 422 price_not_invoiceable and
//     the nearest prices it can (details.nearest), which the form offers
//   PUT|DELETE /v1/publishers/{p}/listings/{l}/operations/{op}/price (step-up) {amount_cents,
//     currency: USD}: a publisher-hosted listing is priced per job of each operation instead
//     (credits v1); the operations and their prices come from GET /v1/marketplace/listings/{l}
(function () {
  "use strict";

  const MP = (window.PPMarketplace = window.PPMarketplace || {});
  const el = (...a) => MP.el(...a);

  const ADMIN_ROLES = ["owner", "org_admin", "publisher_admin"];
  const ORG_APPLY_ROLES = ["owner", "admin"];
  const ROLE_TEXT = {
    owner: "Owner", org_admin: "Organization admin", publisher_admin: "Publisher admin", publisher_finance: "Publisher finance (read only)",
  };
  const PUB_STATUS = {
    applied: "Application received. Pigeonpost reviews every publisher before it can submit anything.",
    approved: "Certified publisher.",
    rejected: "Application not approved.",
    revoked: "Certification revoked. Your listings are suspended.",
  };
  const STAGE_TEXT = {
    intake: "Intake", deterministic_scan: "Automated scan", ai_analysis: "AI review (advisory)",
    behavioral_test: "Behaviour test", human_review: "Staff review", attestation: "Attestation",
  };
  const STAGE_STATUS = { pending: "Waiting", passed: "Passed", failed: "Failed", blocked: "Changes requested" };
  const REVIEW_STATUS = {
    submitted: "Submitted", in_review: "In review", changes_requested: "Changes requested",
    approved: "Approved", rejected: "Rejected", withdrawn: "Withdrawn",
  };
  const BUILD_STATUS = { queued: "Queued", building: "Building", built: "Built", failed: "Failed", cancelled: "Cancelled" };
  const LISTING_STATUS = { draft: "Draft", published: "Published", withdrawn: "Withdrawn", suspended: "Suspended by Pigeonpost" };
  const VISIBILITY = [
    ["public", "Public", "In the Services catalog for everyone."],
    ["unlisted", "Unlisted", "Not in the catalog; anyone with the link can see it."],
    ["private", "Private", "Only your publisher team can see it."],
  ];

  MP.publisher = function (root, deps) {
    const st = { publishers: null, orgs: [], current: null, data: null, review: null, findings: null, error: null, gen: 0, editing: null, preset: null };
    const status = MP.statusRegion("mp-pub-status");
    const pub = () => (st.publishers || []).find((p) => p.id === st.current) || null;
    const isAdmin = () => ADMIN_ROLES.includes(pub()?.role);
    const base = () => `/v1/publishers/${encodeURIComponent(st.current)}`;

    async function load() {
      const gen = ++st.gen;
      try {
        const [list, orgs] = await Promise.all([
          deps.pb("/v1/publishers"),
          deps.pb("/v1/orgs").then((v) => (v.orgs || []).filter((o) => (o.my_roles || []).some((r) => ORG_APPLY_ROLES.includes(r)))).catch(() => []),
        ]);
        if (gen !== st.gen) return;
        st.publishers = list.data || [];
        st.orgs = orgs;
        st.error = null;
        if (!pub()) st.current = st.publishers[0]?.id || null;
      } catch (e) {
        if (gen !== st.gen) return;
        st.error = e;
      }
      draw();
      if (st.current) loadPublisher();
    }

    async function loadPublisher() {
      const p = pub();
      if (!p) return;
      const gen = ++st.gen;
      st.data = null;
      if (p.status === "applied" || p.status === "rejected") { draw(); return; }
      const get = (sub) => deps.pb(`${base()}${sub}`).then((v) => v.data || []).catch(() => []);
      const [roles, templates, builds, reviews, listings] = await Promise.all([
        p.owner_kind === "org" ? get("/roles") : Promise.resolve([]),
        get("/templates"), get("/builds"), get("/reviews"), get("/listings"),
      ]);
      if (gen !== st.gen) return;
      st.data = { roles, templates, builds, reviews, listings };
      draw();
    }

    // ---- actions -------------------------------------------------------------------------------

    async function act(fn, done, after) {
      try {
        const r = await fn();
        status.say(done);
        if (after) await after(r);
      } catch (e) {
        MP.report(deps, status, e, "publisher");
      }
    }
    const post = (path, body, method) => deps.pb(path, { method: method || "POST", body: body === undefined ? "{}" : JSON.stringify(body) });

    function apply(name, org) {
      const body = { display_name: name.trim() };
      if (org) body.org_id = org;
      return act(() => post("/v1/publisher-applications", body), "Draft created. Complete the application steps before submitting for review.", load);
    }

    async function openReview(id) {
      try {
        st.review = (await deps.pb(`${base()}/reviews/${encodeURIComponent(id)}`)).data;
        st.findings = null;
      } catch (e) { MP.report(deps, status, e, "publisher"); }
      draw();
      root.querySelector("#mp-review-title")?.focus();
    }

    async function showFindings() {
      try { st.findings = (await deps.pb(`${base()}/reviews/${encodeURIComponent(st.review.review_id)}/findings`)).data; }
      catch (e) { MP.report(deps, status, e, "publisher"); }
      draw();
    }

    // ---- sections -----------------------------------------------------------------------------

    function applyForm() {
      const name = el("input", { id: "mp-apply-name", required: true, minlength: "2", maxlength: "80", autocomplete: "organization" });
      const owner = st.orgs.length
        ? el("select", { id: "mp-apply-owner" }, el("option", { value: "", text: "My own account" }), st.orgs.map((o) => el("option", { value: o.id, text: o.name })))
        : null;
      return el("form", { class: "ac-form", id: "mp-apply", onsubmit: (e) => { e.preventDefault(); apply(name.value, owner ? owner.value : ""); } },
        el("fieldset", {}, el("legend", { text: "Apply to publish" }),
          el("label", { for: "mp-apply-name" }, "Publisher name buyers will see"), name,
          owner ? el("label", { for: "mp-apply-owner" }, "Publish as") : null, owner,
          el("button", { type: "submit", class: "btn btn-secondary" }, "Start application")));
    }

    function header(p) {
      const box = el("div", { class: "mp-pub-head" });
      if (st.publishers.length > 1) {
        box.append(el("label", { for: "mp-pub-select" }, "Publisher "),
          el("select", { id: "mp-pub-select", onchange: (e) => { st.current = e.target.value; st.review = null; st.findings = null; draw(); loadPublisher(); } },
            st.publishers.map((x) => el("option", { value: x.id, text: x.display_name, selected: x.id === st.current }))));
      }
      box.append(el("p", { id: "mp-pub-state" },
        el("strong", { text: p.display_name }),
        ` · ${p.owner_kind === "org" ? "organization" : "personal"} publisher · your role: ${ROLE_TEXT[p.role] || p.role}`),
        el("p", { class: p.status === "approved" ? "muted" : "pp-warn", text: PUB_STATUS[p.status] || p.status }),
        p.decision_reason && p.status !== "approved" ? el("p", { class: "muted", text: `Reason: ${p.decision_reason}` }) : null);
      return box;
    }

    function teamSection(p, d) {
      const s = el("section", { class: "mp-section", id: "mp-team", "aria-labelledby": "mp-team-h" }, el("h4", { id: "mp-team-h", text: "Team" }));
      if (p.owner_kind !== "org") {
        s.append(el("p", { class: "muted", text: "A personal publisher is just you. Apply for an organization to share the work." }));
        return s;
      }
      s.append(el("p", { class: "muted", text: "The organization's owner and admins always act for it. Publisher admins can submit and publish; publisher finance can only read." }));
      if (d.roles.length) {
        s.append(el("ul", { class: "mp-list", "aria-label": "Publisher roles" }, d.roles.map((r) => el("li", { class: "mp-row", "data-role": r.id },
          el("span", { class: "mono", text: r.account_id }), el("span", { text: ROLE_TEXT[r.role] || r.role }),
          p.role === "org_admin" ? el("button", { type: "button", class: "btn btn-small", "aria-label": `Remove ${ROLE_TEXT[r.role] || r.role} from ${r.account_id}`, onclick: () => act(() => deps.pb(`${base()}/roles/${encodeURIComponent(r.id)}`, { method: "DELETE" }), "Role removed.", loadPublisher) }, "Remove") : null))));
      } else {
        s.append(el("p", { class: "muted", text: "No publisher roles given yet." }));
      }
      if (p.role === "org_admin") {
        const who = el("input", { id: "mp-role-account", required: true, autocomplete: "off", spellcheck: "false" });
        const role = el("select", { id: "mp-role-role" }, el("option", { value: "publisher_admin", text: ROLE_TEXT.publisher_admin }), el("option", { value: "publisher_finance", text: ROLE_TEXT.publisher_finance }));
        s.append(el("form", { class: "ac-form", id: "mp-role-grant", onsubmit: (e) => { e.preventDefault(); act(() => post(`${base()}/roles`, { account_id: who.value.trim(), role: role.value }), "Role given.", loadPublisher); } },
          el("fieldset", {}, el("legend", { text: "Give a publisher role" }),
            el("label", { for: "mp-role-account" }, "Member's account ID"), who,
            el("label", { for: "mp-role-role" }, "Role"), role,
            el("button", { type: "submit", class: "btn btn-small" }, "Give role"))));
      }
      return s;
    }

    function templatesSection(d, admin) {
      const s = el("section", { class: "mp-section", id: "mp-templates", "aria-labelledby": "mp-templates-h" }, el("h4", { id: "mp-templates-h", text: "Templates" }),
        el("p", { class: "muted", text: "A template is one package you publish. Each build is a version of it." }));
      if (d.templates.length) s.append(el("ul", { class: "mp-list", "aria-label": "Templates" }, d.templates.map((t) => el("li", { class: "mp-row", "data-template": t.id }, el("strong", { text: t.name }), el("code", { text: t.slug })))));
      if (admin) {
        const slug = el("input", { id: "mp-tpl-slug", required: true, maxlength: "64", pattern: "[a-z0-9-]+", autocomplete: "off", spellcheck: "false", "aria-describedby": "mp-tpl-slug-hint" });
        const name = el("input", { id: "mp-tpl-name", required: true, minlength: "2", maxlength: "80" });
        s.append(el("form", { class: "ac-form", id: "mp-tpl-create", onsubmit: (e) => { e.preventDefault(); act(() => post(`${base()}/templates`, { slug: slug.value.trim(), name: name.value.trim() }), "Template created.", loadPublisher); } },
          el("fieldset", {}, el("legend", { text: "New template" }),
            el("label", { for: "mp-tpl-name" }, "Name"), name,
            el("label", { for: "mp-tpl-slug" }, "Package name"), slug,
            el("span", { class: "mp-hint", id: "mp-tpl-slug-hint", text: "Lowercase letters, digits and hyphens. It must match the name in nest-release.toml." }),
            el("button", { type: "submit", class: "btn btn-small" }, "Create template"))));
      }
      return s;
    }

    function buildsSection(d, admin) {
      const s = el("section", { class: "mp-section", id: "mp-builds", "aria-labelledby": "mp-builds-h" }, el("h4", { id: "mp-builds-h", text: "Builds" }));
      if (d.builds.length) {
        s.append(el("ul", { class: "mp-list", "aria-label": "Builds" }, d.builds.map((b) => {
          const t = d.templates.find((x) => x.id === b.template_id);
          return el("li", { class: "mp-row", "data-build": b.id },
            el("strong", { text: `${t ? t.name : b.template_id} ${b.version}` }),
            el("span", { text: BUILD_STATUS[b.status] || b.status }),
            el("code", { class: "muted", text: b.source ? b.source.commit.slice(0, 12) : "" }),
            b.failure ? el("span", { class: "pp-warn", text: `${b.failure.code}${b.failure.reason ? ": " + b.failure.reason : ""}` }) : null,
            b.review_id ? el("button", { type: "button", class: "btn btn-small", "aria-label": `Open the review of ${b.version}`, onclick: () => openReview(b.review_id) }, "Open review") : null);
        })));
      } else {
        s.append(el("p", { class: "muted", text: "No builds yet." }));
      }
      if (admin && d.templates.length) {
        const tpl = el("select", { id: "mp-build-template" }, d.templates.map((t) => el("option", { value: t.id, text: t.name })));
        const version = el("input", { id: "mp-build-version", required: true, pattern: "\\d+\\.\\d+\\.\\d+", placeholder: "1.0.0", autocomplete: "off" });
        const repo = el("input", { id: "mp-build-repo", type: "url", required: true, pattern: "https://.+", maxlength: "300", autocomplete: "off", spellcheck: "false", "aria-describedby": "mp-build-repo-hint" });
        const commit = el("input", { id: "mp-build-commit", required: true, pattern: "[0-9a-f]{40}|[0-9a-f]{64}", autocomplete: "off", spellcheck: "false", "aria-describedby": "mp-build-commit-hint" });
        s.append(el("form", { class: "ac-form", id: "mp-build-create", onsubmit: (e) => {
          e.preventDefault();
          const body = { template_id: tpl.value, version: version.value.trim(), source: { repository: repo.value.trim(), commit: commit.value.trim().toLowerCase() } };
          act(() => post(`${base()}/builds`, body), "Build requested. Pigeonpost builds it and starts the review.", loadPublisher);
        } }, el("fieldset", {}, el("legend", { text: "Submit a build" }),
          el("label", { for: "mp-build-template" }, "Template"), tpl,
          el("label", { for: "mp-build-version" }, "Version"), version,
          el("label", { for: "mp-build-repo" }, "Repository URL"), repo,
          el("span", { class: "mp-hint", id: "mp-build-repo-hint", text: "An https address Pigeonpost can read without signing in." }),
          el("label", { for: "mp-build-commit" }, "Commit"), commit,
          el("span", { class: "mp-hint", id: "mp-build-commit-hint", text: "The full commit hash, so the build is exactly what you tested." }),
          el("button", { type: "submit", class: "btn btn-secondary" }, "Submit build"))));
      } else if (admin) {
        s.append(el("p", { class: "muted", text: "Create a template first." }));
      }
      return s;
    }

    function reviewsSection(d) {
      const s = el("section", { class: "mp-section", id: "mp-reviews", "aria-labelledby": "mp-reviews-h" }, el("h4", { id: "mp-reviews-h", text: "Reviews" }));
      if (!d.reviews.length) { s.append(el("p", { class: "muted", text: "Nothing in review." })); return s; }
      s.append(el("ul", { class: "mp-list", "aria-label": "Reviews" }, d.reviews.map((r) => el("li", { class: "mp-row", "data-review": r.review_id },
        el("strong", { text: releaseName(d, r.release_id) }), el("span", { text: REVIEW_STATUS[r.status] || r.status }),
        el("button", { type: "button", class: "btn btn-small", "aria-label": `Open the review of ${releaseName(d, r.release_id)}`, onclick: () => openReview(r.review_id) }, "Open")))));
      return s;
    }

    function releaseName(d, releaseId) {
      const b = d.builds.find((x) => x.release_id === releaseId);
      if (!b) return releaseId;
      const t = d.templates.find((x) => x.id === b.template_id);
      return `${t ? t.name : "Release"} ${b.version}`;
    }

    function reviewDetail(d, admin) {
      const r = st.review;
      const s = el("section", { class: "mp-section mp-review", id: "mp-review", "aria-labelledby": "mp-review-title" },
        el("h4", { id: "mp-review-title", tabindex: "-1", text: `Review: ${releaseName(d, r.release_id)}` }),
        el("p", { id: "mp-review-state", text: `${REVIEW_STATUS[r.status] || r.status} · revision ${r.revision}` }),
        el("ol", { class: "mp-stages", "aria-label": "Review stages" }, (r.stages || []).map((g) => el("li", { "data-stage": g.name, class: `mp-stage mp-stage-${g.status}` },
          el("strong", { text: STAGE_TEXT[g.name] || g.name }), `: ${STAGE_STATUS[g.status] || g.status}`,
          g.reason ? el("span", { class: "muted", text: ` · ${g.reason}` }) : null))));
      const allowed = r.allowed_actions || [];
      const row = el("div", { class: "mp-actions" });
      if (allowed.includes("view_findings")) row.append(el("button", { type: "button", class: "btn btn-small", id: "mp-review-findings", onclick: showFindings }, "Show findings"));
      if (admin && allowed.includes("withdraw")) {
        row.append(el("button", { type: "button", class: "btn btn-small", id: "mp-review-withdraw", onclick: () => act(() => post(`${base()}/reviews/${encodeURIComponent(r.review_id)}/withdraw`), "Withdrawn from review.", async (v) => { st.review = v.data || st.review; await loadPublisher(); }) }, "Withdraw from review"));
      }
      if (admin && allowed.includes("publish_listing")) {
        row.append(el("button", { type: "button", class: "btn btn-secondary", id: "mp-review-list", onclick: () => { st.preset = r.release_id; draw(); root.querySelector("#mp-listing-title")?.focus(); } }, "Create a listing"));
      }
      if (admin && allowed.includes("resubmit")) {
        row.append(el("button", { type: "button", class: "btn btn-small", id: "mp-review-resubmit", onclick: () => root.querySelector("#mp-build-version")?.focus() }, "Submit a fixed build"));
      }
      row.append(el("button", { type: "button", class: "btn btn-small", onclick: () => { st.review = null; st.findings = null; draw(); } }, "Close"));
      s.append(row);
      if (st.findings) s.append(findingsBlock(st.findings));
      return s;
    }

    function findingsBlock(f) {
      const box = el("div", { class: "mp-findings", id: "mp-findings" }, el("h5", { text: "Findings" }));
      if (!(f.stages || []).length) { box.append(el("p", { class: "muted", text: "No findings recorded yet." })); return box; }
      for (const g of f.stages) {
        const items = g.findings || [];
        box.append(el("div", { class: "mp-finding-stage", "data-stage": g.stage },
          el("strong", { text: STAGE_TEXT[g.stage] || g.stage }), ` · ${g.decision}`,
          g.reason ? el("p", { class: "muted", text: g.reason }) : null,
          g.stage === "ai_analysis" ? el("p", { class: "muted", text: `${g.advisory_findings || 0} advisory note${g.advisory_findings === 1 ? "" : "s"} for staff. They don't decide the review.` }) : null,
          items.length ? el("ul", {}, items.map((x) => el("li", {},
            el("code", { text: x.rule }), x.path ? ` in ${x.path}` : "", x.detail ? ` · ${x.detail}` : "", x.blocker ? el("strong", { text: " · blocks the release" }) : null))) : null));
      }
      box.append(el("p", { class: "ac-note", text: "Behaviour-test findings show only which rule fired; the test's details stay with Pigeonpost." }));
      return box;
    }

    function visibilityField(prefix, current) {
      return el("fieldset", { class: "mp-visibility" }, el("legend", { text: "Who can see it" }),
        VISIBILITY.map(([v, label, what]) => el("label", { class: "mp-check", for: `${prefix}-${v}` },
          el("input", { type: "radio", name: `${prefix}-visibility`, id: `${prefix}-${v}`, value: v, checked: (current || "public") === v }), ` ${label}: `, el("span", { class: "muted", text: what }))));
    }
    const chosen = (form, prefix) => form.querySelector(`input[name="${prefix}-visibility"]:checked`)?.value || "public";

    function listingsSection(d, admin) {
      const s = el("section", { class: "mp-section", id: "mp-listings", "aria-labelledby": "mp-listings-h" }, el("h4", { id: "mp-listings-h", text: "Listings" }),
        el("p", { class: "muted", text: "An approved release is not public until you publish a listing for it. Publishing asks you to confirm it's you." }));
      if (d.listings.length) {
        s.append(el("ul", { class: "mp-list", "aria-label": "Listings" }, d.listings.map((l) => listingRow(l, admin))));
      } else {
        s.append(el("p", { class: "muted", text: "No listings yet." }));
      }
      const approved = d.reviews.filter((r) => r.status === "approved");
      if (admin && approved.length) s.append(createListingForm(d, approved));
      return s;
    }

    function listingRow(l, admin) {
      const lpath = `${base()}/listings/${encodeURIComponent(l.id)}`;
      const li = el("li", { class: "mp-row mp-listing", "data-listing": l.id },
        MP.logo ? MP.logo(l, { size: 40 }) : null,
        el("strong", { text: l.title }),
        el("span", { text: `${LISTING_STATUS[l.status] || l.status} · ${l.visibility} · version ${l.version}` }),
        l.moderation_reason ? el("span", { class: "pp-warn", text: `Pigeonpost: ${l.moderation_reason}` }) : null);
      if (!admin) return li;
      const row = el("span", { class: "ac-row-actions" });
      row.append(el("button", { type: "button", class: "btn btn-small", "aria-label": `Edit ${l.title}`, "aria-expanded": String(st.editing === l.id), onclick: () => { st.editing = st.editing === l.id ? null : l.id; draw(); } }, "Edit"));
      row.append(el("button", { type: "button", class: "btn btn-small", "data-price": l.id, "aria-label": `Price of ${l.title}`, "aria-expanded": String(st.pricing === l.id), onclick: () => { st.pricing = st.pricing === l.id ? null : l.id; st.nearest = null; draw(); } }, "Price"));
      if (l.status === "draft" || l.status === "withdrawn") {
        row.append(el("button", { type: "button", class: "btn btn-small", "data-publish": l.id, "aria-label": `Publish ${l.title}`, onclick: () => act(() => post(`${lpath}/publish`, {}), `${l.title} is published.`, loadPublisher) }, "Publish"));
      }
      if (l.status === "published") {
        row.append(el("button", { type: "button", class: "btn btn-small", "data-withdraw": l.id, "aria-label": `Withdraw ${l.title}`, onclick: () => act(() => post(`${lpath}/withdraw`), `${l.title} is withdrawn. Existing installations keep working.`, loadPublisher) }, "Withdraw"));
      }
      li.append(row);
      if (st.editing === l.id) li.append(editListingForm(l, lpath));
      if (st.pricing === l.id) li.append(l.hosting === "publisher" ? jobPriceForm(l, lpath) : priceForm(l, lpath));
      return li;
    }

    /// A publisher-hosted listing's per-job prices (credits v1): one row per operation, in USD.
    /// Buyers pay from their prepaid credit; Piyote keeps 10% of each job.
    function jobPriceForm(l, lpath) {
      const p = `mp-jobprice-${l.id}`;
      const box = el("div", { class: "mp-edit", id: p },
        el("p", { class: "muted", text: "Each operation is priced per job in US dollars and paid from the buyer's credit. Pigeonpost keeps 10% of each job; the rest is paid to you. Changing a price asks you to confirm it's you." }));
      if (!st.jobOps || st.jobOps.id !== l.id) {
        st.jobOps = { id: l.id, ops: null };
        deps.pb(`/v1/marketplace/listings/${encodeURIComponent(l.id)}`)
          .then((v) => { st.jobOps = { id: l.id, ops: (v.data && v.data.operations) || [] }; draw(); })
          .catch(() => { st.jobOps = { id: l.id, ops: [] }; draw(); });
        box.append(el("p", { class: "muted", text: "Loading…" }));
        return box;
      }
      if (!st.jobOps.ops || !st.jobOps.ops.length) { box.append(el("p", { class: "muted", text: st.jobOps.ops ? "This listing has no operations." : "Loading…" })); return box; }
      st.jobOps.ops.forEach((op, i) => {
        const id = `${p}-${i}`;
        const cur = op.price && Number(op.price.amount_cents) > 0 ? op.price : null;
        const amount = el("input", { id: `${id}-amount`, type: "number", min: "0.01", step: "0.01", required: true, value: cur ? (cur.amount_cents / 100).toFixed(2) : "" });
        const opPath = `${lpath}/operations/${encodeURIComponent(op.name)}/price`;
        const after = async () => { st.jobOps = null; draw(); };
        box.append(el("form", { class: "ac-form", id, "data-operation": op.name, onsubmit: (e) => {
          e.preventDefault();
          const cents = Math.round(Number(amount.value) * 100);
          if (!Number.isInteger(cents) || cents <= 0) { status.say("Enter a price above zero."); return; }
          act(() => post(opPath, { amount_cents: cents, currency: "USD" }, "PUT"), "Price saved. It goes on sale once Pigeonpost's billing holds it.", after);
        } }, el("fieldset", {}, el("legend", { text: `${op.title || op.name}: price per job` }),
          el("p", { class: "muted", text: `Now: ${cur ? `${MP.money(cur.amount_cents, "USD")} per job${cur.on_sale ? "" : " (not on sale yet)"}` : "Free"}.` }),
          el("label", { for: `${id}-amount` }, "Price per job (USD)"), amount,
          el("button", { type: "submit", class: "btn btn-small" }, "Save price"),
          cur ? el("button", { type: "button", class: "btn btn-small", id: `${id}-free`, onclick: () => act(() => post(opPath, undefined, "DELETE"), "This operation is free again.", after) }, "Make it free") : null)));
      });
      return box;
    }

    /// The listing's price (payments): tax-inclusive, in TRY, USD or EUR, one-off or monthly.
    function priceForm(l, lpath) {
      const p = `mp-price-${l.id}`;
      const cur = l.price || {};
      const priced = Number(cur.amount_minor) > 0;
      const amount = el("input", { id: `${p}-amount`, type: "number", min: "0.01", step: "0.01", required: true,
        value: priced ? (Number(cur.amount_minor) / 100).toFixed(2) : "" });
      const currency = el("select", { id: `${p}-currency` }, ["TRY", "USD", "EUR"].map((c) => el("option", { value: c, text: c, selected: (cur.currency || "TRY") === c })));
      const term = el("select", { id: `${p}-term` },
        el("option", { value: "one_off", text: "One-off", selected: cur.term !== "monthly" }),
        el("option", { value: "monthly", text: "Monthly", selected: cur.term === "monthly" }));
      const send = (minor) => act(async () => {
        try {
          return await post(`${lpath}/price`, { amount_minor: minor, currency: currency.value, term: term.value }, "PUT");
        } catch (e) {
          if (e.code === "price_not_invoiceable") {
            const near = (e.body && (e.body.nearest || (e.body.details || {}).nearest)) || [];
            st.nearest = { id: l.id, prices: near.filter((n) => Number.isInteger(n) && n > 0).slice(0, 4) };
            draw();
          }
          throw e;
        }
      }, "Price saved. It goes on sale once Pigeonpost's billing holds it.", async () => { st.pricing = null; st.nearest = null; await loadPublisher(); });
      const nearest = st.nearest && st.nearest.id === l.id && st.nearest.prices.length
        ? el("p", { id: `${p}-nearest`, role: "status" }, "That amount can't be invoiced. Nearest prices: ",
          ...st.nearest.prices.map((n) => el("button", { type: "button", class: "btn btn-small", "data-nearest": String(n), onclick: () => { amount.value = (n / 100).toFixed(2); send(n); } }, MP.money(n, currency.value))))
        : null;
      return el("form", { class: "ac-form mp-edit", id: p, onsubmit: (e) => {
        e.preventDefault();
        const minor = Math.round(Number(amount.value) * 100);
        if (!Number.isInteger(minor) || minor <= 0) { status.say("Enter a price above zero."); return; }
        send(minor);
      } }, el("fieldset", {}, el("legend", { text: `Price of ${l.title}` }),
        el("p", { class: "muted", text: `Now: ${MP.priceText(l.price)}. Prices include tax. Changing it asks you to confirm it's you.` }),
        el("label", { for: `${p}-amount` }, "Amount"), amount,
        el("label", { for: `${p}-currency` }, "Currency"), currency,
        el("label", { for: `${p}-term` }, "Billing"), term,
        nearest,
        el("button", { type: "submit", class: "btn btn-small" }, "Save price"),
        priced ? el("button", { type: "button", class: "btn btn-small", id: `${p}-free`, onclick: () => act(() => post(`${lpath}/price`, undefined, "DELETE"), "The listing is free again.", async () => { st.pricing = null; await loadPublisher(); }) }, "Make it free") : null));
    }

    function editListingForm(l, lpath) {
      const p = `mp-edit-${l.id}`;
      const title = el("input", { id: `${p}-title`, required: true, minlength: "2", maxlength: "120", value: l.title });
      const summary = el("input", { id: `${p}-summary`, required: true, minlength: "2", maxlength: "300", value: l.summary });
      const description = el("textarea", { id: `${p}-description`, maxlength: "5000", rows: "4" });
      description.value = l.description || "";
      const form = el("form", { class: "ac-form mp-edit", id: p, onsubmit: (e) => {
        e.preventDefault();
        const patch = {};
        if (title.value.trim() !== l.title) patch.title = title.value.trim();
        if (summary.value.trim() !== l.summary) patch.summary = summary.value.trim();
        if (description.value !== (l.description || "")) patch.description = description.value;
        const vis = chosen(form, p);
        if (vis !== l.visibility) patch.visibility = vis;
        if (!Object.keys(patch).length) { status.say("Nothing changed."); return; }
        act(() => post(lpath, patch, "PATCH"), "Listing saved.", async () => { st.editing = null; await loadPublisher(); });
      } }, el("fieldset", {}, el("legend", { text: `Edit ${l.title}` }),
        el("label", { for: `${p}-title` }, "Title"), title,
        el("label", { for: `${p}-summary` }, "Summary"), summary,
        el("label", { for: `${p}-description` }, "Description"), description,
        visibilityField(p, l.visibility),
        el("button", { type: "submit", class: "btn btn-small" }, "Save")));
      return el("div", {}, form, logoForm(l, lpath));
    }

    /// The listing's logo (PUT/DELETE …/logo): a square PNG, JPEG or WebP up to 1 MB. Without one,
    /// the listing shows its initials on a colour of its own.
    function logoForm(l, lpath) {
      const p = `mp-logo-${l.id}`;
      const file = el("input", { type: "file", id: `${p}-file`, accept: "image/png,image/jpeg,image/webp", "aria-describedby": `${p}-hint` });
      return el("form", { class: "ac-form mp-logo-form", id: p, onsubmit: (e) => {
        e.preventDefault();
        const f = file.files && file.files[0];
        if (!f) { status.say("Choose an image first."); return; }
        if (!["image/png", "image/jpeg", "image/webp"].includes(f.type)) { status.say("A logo is a PNG, JPEG or WebP image. SVG isn't accepted."); return; }
        if (f.size > 1024 * 1024) { status.say("A logo is at most 1 MB."); return; }
        act(() => deps.pb(`${lpath}/logo`, { method: "PUT", body: f, contentType: f.type }), "Logo saved.", loadPublisher);
      } }, el("fieldset", {}, el("legend", { text: "Logo" }),
        el("div", { class: "mp-logo-preview" }, MP.logo(l, { size: 64, alt: `${l.title} logo` })),
        el("label", { for: `${p}-file` }, "Upload a logo"), file,
        el("span", { class: "mp-hint", id: `${p}-hint`, text: "Square PNG, JPEG or WebP, at least 256 × 256 (512 × 512 looks best), up to 1 MB." }),
        el("span", { class: "mp-actions" },
          el("button", { type: "submit", class: "btn btn-small" }, "Upload"),
          l.logo ? el("button", { type: "button", class: "btn btn-small", id: `${p}-remove`, onclick: () => act(() => deps.pb(`${lpath}/logo`, { method: "DELETE" }), "Logo removed.", loadPublisher) }, "Use the default") : null)));
    }

    function createListingForm(d, approved) {
      const p = "mp-listing";
      const release = el("select", { id: `${p}-release` }, approved.map((r) => el("option", { value: r.release_id, text: releaseName(d, r.release_id), selected: st.preset === r.release_id })));
      const title = el("input", { id: `${p}-title`, required: true, minlength: "2", maxlength: "120" });
      const summary = el("input", { id: `${p}-summary`, required: true, minlength: "2", maxlength: "300" });
      const description = el("textarea", { id: `${p}-description`, maxlength: "5000", rows: "4" });
      const form = el("form", { class: "ac-form", id: `${p}-create`, onsubmit: (e) => {
        e.preventDefault();
        const body = { release_id: release.value, title: title.value.trim(), summary: summary.value.trim(), visibility: chosen(form, p) };
        if (description.value.trim()) body.description = description.value;
        act(() => post(`${base()}/listings`, body), "Listing saved as a draft. Publish it when it's ready.", async () => { st.preset = null; await loadPublisher(); });
      } }, el("fieldset", {}, el("legend", { text: "New listing" }),
        el("label", { for: `${p}-release` }, "Approved release"), release,
        el("label", { for: `${p}-title` }, "Title"), title,
        el("label", { for: `${p}-summary` }, "Summary"), summary,
        el("label", { for: `${p}-description` }, "Description"), description,
        visibilityField(p, "public"),
        el("button", { type: "submit", class: "btn btn-secondary" }, "Save draft")));
      return form;
    }

    function laterSections() {
      return [
        el("section", { class: "mp-section", id: "mp-installs", "aria-labelledby": "mp-installs-h" }, el("h4", { id: "mp-installs-h", text: "Installations" }),
          el("p", { class: "muted", text: "How many accounts installed each listing will show here." })),
        el("section", { class: "mp-section", id: "mp-earnings", "aria-labelledby": "mp-earnings-h" }, el("h4", { id: "mp-earnings-h", text: "Earnings and payouts" }),
          el("p", { class: "muted", text: "Set a price on a listing to sell it. Earnings and payouts will show here." })),
      ];
    }

    function draw() {
      root.textContent = "";
      root.append(el("h3", { id: "mp-pub-title", text: "Publish on the marketplace" }), status);
      if (st.error) { root.append(el("p", { class: "muted", text: MP.errorText(st.error) })); return; }
      if (!st.publishers) { root.append(el("p", { class: "muted", text: "Loading…" })); return; }
      if (!st.publishers.length) {
        root.append(el("p", { text: "Publish services that run on Nests. Pigeonpost reviews every publisher and every release before anyone can install it." }), applyForm());
        return;
      }
      const p = pub();
      root.append(header(p));
      if (p.status === "rejected") root.append(applyForm());
      if (p.status === "applied") {
        const onboarding = el("div", {id:"publisher-onboarding"}); root.append(onboarding);
        if (MP.onboarding) MP.onboarding(onboarding, deps, p);
        return;
      }
      if (p.status === "rejected") return;
      const d = st.data;
      if (!d) { root.append(el("p", { class: "muted", text: "Loading…" })); return; }
      const admin = isAdmin() && p.status === "approved";
      if (!admin && p.status === "approved") root.append(el("p", { class: "muted", id: "mp-readonly", text: "Your role can read this publisher but not change it." }));
      root.append(teamSection(p, d));
      if (st.review) root.append(reviewDetail(d, admin));
      root.append(templatesSection(d, admin), buildsSection(d, admin), reviewsSection(d), listingsSection(d, admin), ...laterSections());
      if (st.orgs.length) {
        root.append(el("details", { class: "mp-more" }, el("summary", { text: "Apply for another publisher" }), applyForm()));
      }
    }

    draw();
    load();
    return { reload: load };
  };
})();
