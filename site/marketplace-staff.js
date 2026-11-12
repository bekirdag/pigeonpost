// Staff dashboard on the account page (build plan R-60, R-65, R-69; readiness W4): the publisher
// applications queue, the review queue with stage evidence and each row's conflict flag, staff
// decisions over the exact release digest, release quarantine/lift/revoke, publisher revocation
// and listing moderation.
//
// Every staff route needs a live staff role and a recent strong sign-in (step-up). The card stays
// hidden for everyone else: the first probe answers 403 staff_required (or 404 while the
// marketplace is off) and nothing is drawn. A staff member without a fresh strong sign-in gets one
// button that sends them through it.
//
// The owner is the only reviewer for now (owner decision D3): when the staff role list shows one
// reviewer, every decision is labelled a single-person approval.
//
// Calls (all through deps.pb):
//   GET  /v1/staff/reviews?status=                     queue; rows carry `conflict` and `first_party`
//   GET  /v1/staff/reviews/{review}                    stages with evidence digests, release digests
//   POST /v1/staff/reviews/{review}/decisions          {decision, reason, release_digest}
//   POST /v1/staff/releases/{release}/security         {action: quarantine|lift|revoke, reason}
//   GET  /v1/staff/publisher-applications?status=      rows carry `conflict`
//   POST /v1/staff/publisher-applications/{id}/decisions   {decision: approved|rejected, reason}
//   POST /v1/staff/publishers/{publisher}/revocation   {reason}
//   POST /v1/staff/listings/{listing}/moderation       {action: suspend|reinstate, reason}
//   GET  /v1/staff/roles                               (platform owner only) to count reviewers
//   GET  /v1/marketplace/listings                      public listings, to pick one to moderate
(function () {
  "use strict";

  const MP = (window.PPMarketplace = window.PPMarketplace || {});
  const el = (...a) => MP.el(...a);

  const REVIEW_FILTERS = [["open", "Open"], ["blocked", "Blocked"], ["decided", "Decided"], ["withdrawn", "Withdrawn"]];
  const APP_FILTERS = [["applied", "Waiting"], ["approved", "Certified"], ["rejected", "Rejected"], ["revoked", "Revoked"]];
  const DECISIONS = [["approved", "Approve and attest"], ["changes_requested", "Ask for changes"], ["rejected", "Reject"]];
  const SECURITY = [["quarantine", "Quarantine (stops jobs and workers; can be lifted)"], ["lift", "Lift a quarantine"], ["revoke", "Revoke (permanent)"]];
  const STAGE_TEXT = {
    intake: "Intake", deterministic_scan: "Automated scan", ai_analysis: "AI review (advisory)",
    behavioral_test: "Behaviour test", human_review: "Staff review", attestation: "Attestation",
  };

  // `opts` (the back office, site/staff.js): `only` picks the sections to draw ("reviews",
  // "applications", "moderation"; default all three), and `embedded` keeps the card visible with a
  // sentence when access is refused, since the back office screen has its own heading.
  MP.staff = function (root, deps, opts) {
    const o = opts || {};
    const only = new Set(o.only || ["reviews", "applications", "moderation"]);
    const st = {
      access: "checking", reviewFilter: "open", appFilter: "applied", reviews: [], apps: [], reviewers: null,
      review: null, listings: [], revoking: null, gen: 0,
    };
    const status = MP.statusRegion("mp-staff-status");
    const single = () => st.reviewers !== null && st.reviewers <= 1;

    async function load() {
      const gen = ++st.gen;
      try {
        // The first section drawn is the access probe; the others fall back to empty.
        const appsPath = `/v1/staff/publisher-applications?status=${st.appFilter}`;
        const reviews = only.has("reviews") ? await deps.pb(`/v1/staff/reviews?status=${st.reviewFilter}`) : { data: [] };
        const apps0 = !only.has("reviews") && only.has("applications") ? await deps.pb(appsPath) : null;
        const [apps, roles, listings] = await Promise.all([
          apps0 || (only.has("applications") ? deps.pb(appsPath).catch(() => ({ data: [] })) : { data: [] }),
          deps.pb("/v1/staff/roles").catch(() => null),
          only.has("moderation") ? deps.pb("/v1/marketplace/listings").catch(() => ({ data: [] })) : { data: [] },
        ]);
        if (gen !== st.gen) return;
        st.reviews = reviews.data || [];
        st.apps = apps.data || [];
        st.listings = listings.data || [];
        if (roles) st.reviewers = new Set((roles.data || []).filter((r) => r.role === "reviewer").map((r) => r.account_id)).size;
        st.access = "ok";
      } catch (e) {
        if (gen !== st.gen) return;
        st.access = e.code === "step_up_required" ? "step_up" : "none";
        st.stepUpError = e;
      }
      draw();
    }

    async function act(fn, done, after) {
      try {
        const r = await fn();
        status.say(done);
        if (after) await after(r);
      } catch (e) { MP.report(deps, status, e, "staff"); }
    }
    const post = (path, body) => deps.pb(path, { method: "POST", body: JSON.stringify(body) });

    async function openReview(id) {
      try { st.review = (await deps.pb(`/v1/staff/reviews/${encodeURIComponent(id)}`)).data; }
      catch (e) { MP.report(deps, status, e, "staff"); }
      draw();
      root.querySelector("#mp-sr-title")?.focus();
    }

    function singleNote(id) {
      return single()
        ? el("p", { class: "pp-warn", id, text: "Single-person approval: you are the only reviewer, so nobody else checks this decision. It is logged as a single-person override." })
        : el("p", { class: "muted", id, text: "Your decision is logged under your name." });
    }

    function filterSelect(id, label, options, value, onchange) {
      return el("label", { class: "mp-filter-select", for: id }, `${label} `,
        el("select", { id, onchange: (e) => onchange(e.target.value) }, options.map(([v, t]) => el("option", { value: v, text: t, selected: v === value }))));
    }

    function reason(id, label) {
      return [el("label", { for: id }, label), el("textarea", { id, required: true, maxlength: "2000", rows: "2" })];
    }

    // ---- applications -------------------------------------------------------------------------------

    function applicationsSection() {
      const s = el("section", { class: "mp-section", id: "mp-staff-apps", "aria-labelledby": "mp-staff-apps-h" },
        el("h4", { id: "mp-staff-apps-h", text: "Publisher applications" }),
        filterSelect("mp-staff-app-filter", "Show", APP_FILTERS, st.appFilter, (v) => { st.appFilter = v; load(); }));
      if (!st.apps.length) { s.append(el("p", { class: "muted", text: "Nothing here." })); return s; }
      s.append(el("ul", { class: "mp-list", "aria-label": "Publisher applications" }, st.apps.map(appRow)));
      return s;
    }

    function appRow(a) {
      const li = el("li", { class: "mp-row", "data-application": a.id },
        el("strong", { text: a.display_name }),
        el("span", { class: "muted", text: `${a.owner_kind === "org" ? "Organization" : "Person"} · applied ${MP.when(a.created_at)}` }),
        a.conflict ? el("span", { class: "pp-warn", "data-conflict": "true", text: "You have a stake in this publisher" }) : null);
      if (a.conflict) return li;
      if (a.status === "applied") {
        const p = `mp-app-${a.id}`;
        const form = el("form", { class: "ac-form", id: p, onsubmit: (e) => {
          e.preventDefault();
          const decision = form.querySelector(`input[name="${p}-d"]:checked`)?.value;
          if (!decision) { status.say("Choose certify or reject."); return; }
          act(() => post(`/v1/staff/publisher-applications/${encodeURIComponent(a.id)}/decisions`, { decision, reason: form.querySelector(`#${p}-reason`).value.trim() }),
            decision === "approved" ? `${a.display_name} is certified.` : `${a.display_name} is rejected.`, load);
        } }, el("fieldset", {}, el("legend", { text: `Decide ${a.display_name}` }),
          el("label", { class: "mp-check", for: `${p}-ok` }, el("input", { type: "radio", name: `${p}-d`, id: `${p}-ok`, value: "approved" }), " Certify"),
          el("label", { class: "mp-check", for: `${p}-no` }, el("input", { type: "radio", name: `${p}-d`, id: `${p}-no`, value: "rejected" }), " Reject"),
          ...reason(`${p}-reason`, "Reason (the applicant sees it)"),
          singleNote(`${p}-single`),
          el("button", { type: "submit", class: "btn btn-small" }, "Record decision")));
        li.append(form);
      } else if (a.status === "approved") {
        li.append(el("button", { type: "button", class: "btn btn-small", "aria-expanded": String(st.revoking === a.id), "aria-label": `Revoke ${a.display_name}`, onclick: () => { st.revoking = st.revoking === a.id ? null : a.id; draw(); } }, "Revoke"));
        if (st.revoking === a.id) li.append(revokeForm(a));
      }
      return li;
    }

    function revokeForm(a) {
      const p = `mp-revoke-${a.id}`;
      const typed = el("input", { id: `${p}-confirm`, autocomplete: "off", spellcheck: "false" });
      const go = el("button", { type: "submit", class: "btn btn-small", disabled: true }, "Revoke publisher");
      typed.addEventListener("input", () => { go.disabled = typed.value !== "REVOKE"; });
      return el("form", { class: "ac-form", id: p, onsubmit: (e) => {
        e.preventDefault();
        if (typed.value !== "REVOKE") return;
        act(() => post(`/v1/staff/publishers/${encodeURIComponent(a.id)}/revocation`, { reason: e.target.querySelector(`#${p}-reason`).value.trim() }),
          `${a.display_name} is revoked. Its listings are suspended and queued builds cancelled.`, async () => { st.revoking = null; await load(); });
      } }, el("fieldset", {}, el("legend", { text: `Revoke ${a.display_name}` }),
        ...reason(`${p}-reason`, "Reason"),
        el("label", { for: `${p}-confirm` }, "Type REVOKE to confirm"), typed,
        singleNote(`${p}-single`), go));
    }

    // ---- reviews ------------------------------------------------------------------------------------

    function reviewsSection() {
      const s = el("section", { class: "mp-section", id: "mp-staff-reviews", "aria-labelledby": "mp-staff-reviews-h" },
        el("h4", { id: "mp-staff-reviews-h", text: "Review queue" }),
        filterSelect("mp-staff-review-filter", "Show", REVIEW_FILTERS, st.reviewFilter, (v) => { st.reviewFilter = v; st.review = null; load(); }));
      if (!st.reviews.length) { s.append(el("p", { class: "muted", text: "Nothing here." })); return s; }
      s.append(el("ul", { class: "mp-list", "aria-label": "Reviews" }, st.reviews.map((r) => el("li", { class: "mp-row", "data-review": r.id },
        el("strong", { text: `${r.publisher_name || r.publisher_id} ${r.version}` }),
        el("span", { class: "muted", text: `${(r.stages_recorded || []).length} of 4 checks recorded · revision ${r.revision}` }),
        r.first_party ? el("span", { class: "mp-badge", text: "First party" }) : null,
        r.conflict ? el("span", { class: "pp-warn", "data-conflict": "true", text: "Conflict: you can't decide this" }) : null,
        el("button", { type: "button", class: "btn btn-small", "aria-label": `Open the review of ${r.publisher_name || r.publisher_id} ${r.version}`, onclick: () => openReview(r.id) }, "Open")))));
      return s;
    }

    /// A publisher-hosted integration: its service mailbox, operations and what buyers will pay.
    function integrationBlock(x) {
      const prices = Array.isArray(x.prices) ? x.prices : [];
      return el("div", { class: "mp-integration", id: "mp-sr-integration" },
        el("h5", { text: "Publisher-hosted integration" }),
        el("p", { text: `Service address ${x.service_address || ""}` }, x.service_owned ? null : el("strong", { class: "pp-warn", text: " · no longer the publisher's mailbox" })),
        el("p", { class: "muted", text: `Operations: ${(x.operations || []).map((o) => o.title || o.name).join(", ") || "none"}` }),
        el("p", { id: "mp-sr-prices", text: prices.length
          ? `Per-job prices (paid from buyers' credit): ${prices.map((p) => `${p.operation} ${MP.money(p.amount_cents, "USD")}`).join(", ")}`
          : "Per-job prices: none set (free)." }));
    }

    function reviewDetail() {
      const r = st.review;
      const row = st.reviews.find((x) => x.id === r.id) || {};
      const conflict = Boolean(row.conflict);
      const s = el("section", { class: "mp-section mp-review", id: "mp-sr", "aria-labelledby": "mp-sr-title" },
        el("h4", { id: "mp-sr-title", tabindex: "-1", text: `Review ${r.id}` }),
        el("dl", { class: "mp-evidence" },
          el("dt", { text: "Version" }), el("dd", { text: r.version || "" }),
          el("dt", { text: "Release digest" }), el("dd", {}, el("code", { id: "mp-sr-digest", text: r.release_digest || "" })),
          el("dt", { text: "Image" }), el("dd", {}, el("code", { text: r.image || "" })),
          el("dt", { text: "SBOM digest" }), el("dd", {}, el("code", { text: r.sbom_digest || "" })),
          el("dt", { text: "Source digest" }), el("dd", {}, el("code", { text: r.source_digest || "" })),
          el("dt", { text: "Security state" }), el("dd", { id: "mp-sr-security", text: r.security_state || "" })),
        r.integration ? integrationBlock(r.integration) : null,
        el("h5", { text: "Stages and evidence" }),
        (r.stages || []).length ? el("ol", { class: "mp-stages", "aria-label": "Recorded stages" }, r.stages.map((g) => el("li", { "data-stage": g.stage },
          el("strong", { text: STAGE_TEXT[g.stage] || g.stage }), `: ${g.decision}`,
          g.reason ? el("span", { class: "muted", text: ` · ${g.reason}` }) : null,
          g.evidence_digest ? el("span", {}, " · evidence ", el("code", { text: g.evidence_digest })) : null,
          el("span", { class: "muted", text: ` · ${g.reviewer} · ${MP.when(g.created_at)}` })))) : el("p", { class: "muted", text: "No stages recorded yet." }));
      if (conflict) {
        s.append(el("p", { class: "pp-warn", role: "alert", id: "mp-sr-conflict", text: "You have a stake in this publisher or release, so you can't decide it or change its security state." }));
      } else {
        if (r.status === "open" || r.status === "blocked") s.append(decisionForm(r));
        s.append(securityForm(r));
      }
      s.append(el("button", { type: "button", class: "btn btn-small", onclick: () => { st.review = null; draw(); } }, "Close"));
      return s;
    }

    function decisionForm(r) {
      const p = "mp-sr-decision";
      const form = el("form", { class: "ac-form", id: p, onsubmit: (e) => {
        e.preventDefault();
        const decision = form.querySelector(`input[name="${p}-d"]:checked`)?.value;
        if (!decision) { status.say("Choose a decision."); return; }
        act(() => post(`/v1/staff/reviews/${encodeURIComponent(r.id)}/decisions`, { decision, reason: form.querySelector(`#${p}-reason`).value.trim(), release_digest: r.release_digest }),
          decision === "approved" ? "Approved and attested. The publisher can now list it." : "Decision recorded.", async () => { st.review = null; await load(); });
      } }, el("fieldset", {}, el("legend", { text: "Decision" }),
        DECISIONS.map(([v, t]) => el("label", { class: "mp-check", for: `${p}-${v}` }, el("input", { type: "radio", name: `${p}-d`, id: `${p}-${v}`, value: v }), ` ${t}`)),
        ...reason(`${p}-reason`, "Reason (the publisher sees it)"),
        el("p", { class: "muted", text: "You are deciding on the release digest shown above. An approval writes the attestation over it." }),
        singleNote(`${p}-single`),
        el("button", { type: "submit", class: "btn btn-secondary" }, "Record decision")));
      return form;
    }

    function securityForm(r) {
      const p = "mp-sr-security-form";
      const typed = el("input", { id: `${p}-confirm`, autocomplete: "off", spellcheck: "false", "aria-describedby": `${p}-confirm-hint` });
      const form = el("form", { class: "ac-form", id: p, onsubmit: (e) => {
        e.preventDefault();
        const action = form.querySelector(`input[name="${p}-a"]:checked`)?.value;
        if (!action) { status.say("Choose what to do with the release."); return; }
        if (action === "revoke" && typed.value !== "REVOKE") { status.say("Type REVOKE to revoke a release. It can't be undone."); return; }
        act(() => post(`/v1/staff/releases/${encodeURIComponent(r.release_id)}/security`, { action, reason: form.querySelector(`#${p}-reason`).value.trim() }),
          { quarantine: "Release quarantined. Its jobs and workers are stopped.", lift: "Quarantine lifted.", revoke: "Release revoked for good." }[action],
          () => openReview(r.id));
      } }, el("fieldset", {}, el("legend", { text: "Release security" }),
        SECURITY.map(([v, t]) => el("label", { class: "mp-check", for: `${p}-${v}` }, el("input", { type: "radio", name: `${p}-a`, id: `${p}-${v}`, value: v }), ` ${t}`)),
        ...reason(`${p}-reason`, "Reason"),
        el("label", { for: `${p}-confirm` }, "To revoke, type REVOKE"), typed,
        el("span", { class: "mp-hint", id: `${p}-confirm-hint`, text: "Only needed for a revocation." }),
        el("button", { type: "submit", class: "btn btn-small" }, "Apply")));
      return form;
    }

    // ---- listing moderation ------------------------------------------------------------------------

    function moderationSection() {
      const p = "mp-mod";
      const id = el("input", { id: `${p}-listing`, required: true, list: `${p}-options`, autocomplete: "off", spellcheck: "false", "aria-describedby": `${p}-hint` });
      const form = el("form", { class: "ac-form", id: p, onsubmit: (e) => {
        e.preventDefault();
        const action = form.querySelector(`input[name="${p}-a"]:checked`)?.value;
        if (!action) { status.say("Choose suspend or reinstate."); return; }
        act(() => post(`/v1/staff/listings/${encodeURIComponent(id.value.trim())}/moderation`, { action, reason: form.querySelector(`#${p}-reason`).value.trim() }),
          action === "suspend" ? "Listing suspended. It is out of the catalog and takes no new installs." : "Listing reinstated.", load);
      } }, el("fieldset", {}, el("legend", { text: "Suspend or reinstate a listing" }),
        el("label", { for: `${p}-listing` }, "Listing ID"), id,
        el("datalist", { id: `${p}-options` }, st.listings.map((l) => el("option", { value: l.id, text: l.title }))),
        el("span", { class: "mp-hint", id: `${p}-hint`, text: "Public listings are suggested as you type. Suspended or unlisted ones need their ID." }),
        el("label", { class: "mp-check", for: `${p}-suspend` }, el("input", { type: "radio", name: `${p}-a`, id: `${p}-suspend`, value: "suspend" }), " Suspend"),
        el("label", { class: "mp-check", for: `${p}-reinstate` }, el("input", { type: "radio", name: `${p}-a`, id: `${p}-reinstate`, value: "reinstate" }), " Reinstate"),
        ...reason(`${p}-reason`, "Reason (the publisher sees it)"),
        el("button", { type: "submit", class: "btn btn-small" }, "Apply")));
      return el("section", { class: "mp-section", id: "mp-staff-moderation", "aria-labelledby": "mp-staff-moderation-h" },
        el("h4", { id: "mp-staff-moderation-h", text: "Listing moderation" }), form);
    }

    function draw() {
      root.textContent = "";
      if (o.embedded && st.access === "checking") { root.append(el("p", { class: "muted", role: "status", text: "Loading…" })); return; }
      if (o.embedded && st.access === "none") { root.append(el("p", { class: "muted", text: MP.errorText(st.stepUpError) })); return; }
      if (st.access === "checking" || st.access === "none") { root.hidden = true; return; }
      root.hidden = false;
      if (!o.embedded) root.append(el("h3", { id: "mp-staff-title", text: "Staff: marketplace review" }));
      root.append(status);
      if (st.access === "step_up") {
        root.append(el("p", { text: "Staff tools need a recent two-step sign-in." }));
        MP.report(deps, status, st.stepUpError, "staff");
        return;
      }
      if (single()) root.append(el("p", { class: "pp-warn", id: "mp-staff-single", text: "You are the only reviewer. Every decision here is a single-person approval and is logged as one." }));
      if (st.review) root.append(reviewDetail());
      if (only.has("reviews")) root.append(reviewsSection());
      if (only.has("applications")) root.append(applicationsSection());
      if (only.has("moderation")) root.append(moderationSection());
    }

    draw();
    load();
    return { reload: load };
  };
})();
