// management.pigeonpost.dev — Marketplace: release reviews (stages, scanner evidence, decisions
// over the exact release digest), listings and buyer reports, hosted jobs, and apps & releases
// (security flags, quarantine/lift/revoke, publisher quotas and kill switch, source builds).
(function () {
  "use strict";
  const PP = window.PP, M = window.PPM, ui = PP.ui;
  const el = (...a) => PP.el(...a);
  const enc = encodeURIComponent;

  const STAGES = {
    intake: "Intake", deterministic_scan: "Automated scan", ai_analysis: "AI review (advisory)",
    behavioral_test: "Behaviour test", human_review: "Staff review", attestation: "Attestation",
  };
  const DECISIONS = [["approved", "Approve and attest"], ["changes_requested", "Ask for changes"], ["rejected", "Reject"]];
  const SECURITY = [["quarantine", "Quarantine"], ["lift", "Lift quarantine"], ["revoke", "Revoke (permanent)"]];
  const SECURITY_DONE = { quarantine: "Release quarantined. Its jobs and workers are stopped.", lift: "Quarantine lifted.", revoke: "Release revoked for good." };

  // ---- Reviews -------------------------------------------------------------------------------

  M.page({
    key: "reviews", title: "Reviews", group: "Marketplace", icon: "review", allow: M.allow(["reviewer"]),
    count: (ctx) => (ctx.counts && ctx.counts.open_reviews) || null,
    render: (view, ctx, [id]) => M.guard(view, () => (id ? reviewDetail(view, id) : reviewQueue(view))),
  });

  let reviewFilter = "open";
  M.reviewRows = [];
  async function reviewQueue(view) {
    view.append(ui.header({ title: "Reviews", text: "Release and integration reviews. Every decision is made over the exact release digest the scanners checked." }));
    const host = el("div");
    view.append(el("div", { class: "pm-toolbar" }, M.chips({ label: "Status", value: reviewFilter, options: [["open", "Open"], ["blocked", "Blocked"], ["decided", "Decided"], ["withdrawn", "Withdrawn"]], onChange: (v) => { reviewFilter = v; load(); } })), host);
    const load = async () => {
      host.replaceChildren(ui.loading(3));
      let r;
      try { r = await M.get(`/v1/staff/reviews?status=${enc(reviewFilter)}`); } catch (e) { if (!(e instanceof PP.Redirecting)) host.replaceChildren(M.errorBox(e, load)); return; }
      M.reviewRows = r.data || [];
      host.replaceChildren(ui.table({
        rows: M.reviewRows, pageSize: 50, caption: "Reviews",
        rowHref: (x) => PP.href("reviews", x.id),
        empty: { icon: "review", title: reviewFilter === "open" ? "Nothing waiting for review" : "Nothing here", text: "Submitted releases appear here once intake has run." },
        columns: [
          { key: "publisher_name", label: "Release", sort: true, render: (x) => el("div", { class: "pm-stack" }, el("strong", {}, `${x.publisher_name || x.publisher_id} ${x.version || ""}`), M.mono(x.id)) },
          { key: "stages", label: "Checks", render: (x) => `${(x.stages_recorded || []).length} of 4 recorded` },
          { key: "revision", label: "Revision", num: true },
          { key: "flags", label: "", render: (x) => el("span", { class: "pm-badges" }, x.first_party ? ui.badge("First party", "info") : null, x.conflict ? ui.badge("Conflict: you can't decide", "danger") : null) },
        ],
      }));
    };
    await load();
  }

  async function reviewDetail(view, id) {
    PP.shell.crumb(id);
    view.append(ui.header({ eyebrow: "Review", title: id, text: "Stages with their evidence digests, the release under review, and the decision.", actions: ui.button("Back to reviews", { href: PP.href("reviews") }) }));
    const [reviewers, queue] = await Promise.all([M.reviewerCount(), M.reviewRows.length ? Promise.resolve({ data: M.reviewRows }) : M.get(`/v1/staff/reviews?status=${enc(reviewFilter)}`).catch(() => ({ data: [] }))]);
    const row = (queue.data || []).find((x) => x.id === id) || {};
    await M.detail(view, `/v1/staff/reviews/${enc(id)}`, (box, r) => {
      const conflict = Boolean(row.conflict);
      const left = el("div", { class: "pm-col" }), right = el("div", { class: "pm-col" });
      box.append(el("div", { class: "pm-split" }, left, right));
      left.append(ui.card({ title: "Release" }, M.kv([
        ["Publisher", row.publisher_name || r.publisher_id || "—"], ["Version", r.version], ["Status", M.status(r.status || row.status)],
        ["Release digest", r.release_digest ? ui.copyable(r.release_digest) : "—"], ["Image", r.image ? M.mono(r.image) : "—"],
        ["SBOM digest", r.sbom_digest ? M.mono(r.sbom_digest) : "—"], ["Source digest", r.source_digest ? M.mono(r.source_digest) : "—"],
        ["Security state", M.status(r.security_state || "normal")],
      ])));
      if (r.integration) {
        const x = r.integration, prices = Array.isArray(x.prices) ? x.prices : [];
        left.append(ui.card({ title: "Publisher-hosted integration" }, M.kv([
          ["Service address", el("span", {}, M.mono(x.service_address), x.service_owned ? null : el("strong", { class: "pm-warn" }, " · no longer the publisher's mailbox"))],
          ["Operations", (x.operations || []).map((o) => o.title || o.name).join(", ") || "none"],
          ["Per-job prices", prices.length ? prices.map((p) => `${p.operation} ${M.usd(p.amount_cents)}`).join(", ") : "none set (free)"],
        ])));
      }
      left.append(ui.card({ title: "Stages and scanner evidence" }, (r.stages || []).length ? el("ol", { class: "pm-timeline" }, r.stages.map((g) => el("li", { "data-stage": g.stage },
        el("div", { class: "pm-timeline-head" }, el("strong", {}, STAGES[g.stage] || g.stage), M.status(g.decision === "pass" ? "approved" : g.decision === "fail" ? "failed" : g.decision, g.decision)),
        g.reason ? el("p", {}, g.reason) : null,
        g.evidence_digest ? el("p", { class: "pp-faint" }, "Evidence ", M.mono(g.evidence_digest)) : null,
        el("p", { class: "pp-faint" }, `${g.reviewer || ""} · ${M.time(g.created_at)}`)))) : ui.empty({ icon: "review", title: "No stages recorded yet" })));
      if (conflict) {
        right.append(ui.gate({ text: "You have a stake in this publisher or release, so you can't decide it or change its security state.", tone: "danger" }));
        return;
      }
      if (r.status === "open" || r.status === "blocked" || (!r.status && ["open", "blocked"].includes(row.status || reviewFilter))) right.append(ui.card({ title: "Decision", tone: "accent" }, reviewDecision(r, reviewers)));
      right.append(ui.card({ title: "Release security" }, securityForm(r.release_id, () => PP.shell.refresh())));
    }, { notFound: "No review with that id" });
  }

  function reviewDecision(r, reviewers) {
    let decision = "";
    const wrap = el("div", { class: "pm-decision" });
    const form = M.form([{ name: "reason", label: "Reason (the publisher sees it)", type: "textarea", required: true, maxlength: 2000, rows: 3, wide: true }], {
      submit: "Record decision",
      onSubmit: async (v) => {
        if (!decision) throw new Error("Choose a decision.");
        if (decision === "approved" && !(await ui.confirm({ title: "Approve and attest?", text: `The attestation is written over release digest ${r.release_digest}. The publisher can then list it.`, confirm: "Approve and attest" }))) return "Not recorded.";
        await M.post(`/v1/staff/reviews/${enc(r.id)}/decisions`, { decision, reason: v.reason, release_digest: r.release_digest });
        PP.toast(decision === "approved" ? "Approved and attested. The publisher can now list it." : "Decision recorded.", "success");
        PP.go("reviews");
        return "Recorded.";
      },
    });
    form.prepend(M.chips({ label: "Decision", value: decision, options: DECISIONS, onChange: (v) => { decision = v; } }),
      el("p", { class: "pp-muted pm-note" }, "You are deciding on the release digest shown on the left. An approval writes the attestation over it."));
    wrap.append(M.singleNote(reviewers), form);
    return wrap;
  }

  /// Quarantine, lift or revoke one release (revoke needs REVOKE typed).
  function securityForm(releaseId, after) {
    let action = "";
    const form = M.form([
      { name: "release", label: "Release id", required: true, value: releaseId || "", placeholder: "rel_…", disabled: Boolean(releaseId) },
      { name: "reason", label: "Reason", type: "textarea", required: true, maxlength: 2000, rows: 2, wide: true },
      { name: "typed", label: "To revoke, type REVOKE", hint: "Only needed for a revocation." },
    ], {
      submit: "Apply", danger: true,
      onSubmit: async (v) => {
        if (!action) throw new Error("Choose what to do with the release.");
        if (action === "revoke" && v.typed !== "REVOKE") throw new Error("Type REVOKE to revoke a release. It can't be undone.");
        const id = releaseId || v.release;
        await M.post(`/v1/staff/releases/${enc(id)}/security`, { action, reason: v.reason });
        PP.toast(SECURITY_DONE[action], "success");
        if (after) after();
        return SECURITY_DONE[action];
      },
    });
    form.prepend(M.chips({ label: "Action", value: action, options: SECURITY, onChange: (v) => { action = v; } }));
    return form;
  }
  M.securityForm = securityForm;

  // ---- Listings & reports --------------------------------------------------------------------

  M.page({
    key: "listings", title: "Listings & reports", group: "Marketplace", icon: "tag", allow: M.allow(["reviewer", "ops"]),
    count: (ctx) => (ctx.counts && ctx.counts.open_reports) || null,
    render: (view, ctx, [tab0]) => M.guard(view, () => {
      view.append(ui.header({ title: "Listings & reports", text: "Moderate listings, answer buyer reports, and stop a listing's workers with the kill switch." }));
      const TABS = [{ key: "reports", label: "Buyer reports" }, { key: "listings", label: "Listings" }];
      let bar = el("div");
      const panel = el("div", { class: "pm-tab-panel" });
      view.append(bar, panel);
      const pick = (t) => {
        bar.replaceWith(bar = ui.tabs(TABS, t, pick));
        panel.replaceChildren();
        if (t === "listings") M.list(panel, {
          search: false, caption: "Listings",
          chips: { name: "status", label: "Status", value: "", options: [["", "Any status"], ["published", "Published"], ["suspended", "Suspended"], ["draft", "Draft"], ["withdrawn", "Withdrawn"]] },
          fetch: (p) => M.get(`/v1/staff/listings?${p}`), empty: { icon: "tag", title: "No listings" },
          columns: [
            { key: "title", label: "Listing", sort: true, render: (l) => el("div", { class: "pm-stack" }, el("strong", {}, l.title), M.mono(l.id)) },
            { key: "publisher", label: "Publisher", render: (l) => (l.publisher && l.publisher.display_name) || l.publisher_id },
            { key: "status", label: "Status", sort: true, render: (l) => M.status(l.status) },
            { key: "security_state", label: "Security", render: (l) => (l.security_state && l.security_state !== "normal" ? M.status(l.security_state) : "—") },
            { key: "open_reports", label: "Reports", num: true, sort: true },
            { key: "id", label: "", render: (l) => listingActions(l) },
          ],
        });
        else M.list(panel, {
          search: false, caption: "Reports",
          chips: { name: "status", label: "Status", value: "open", options: [["open", "Open"], ["closed", "Closed"]] },
          fetch: (p) => M.get(`/v1/staff/listing-reports?${p}`), empty: { icon: "check", title: "No reports in this state" },
          columns: [
            { key: "created_at", label: "Reported", render: (r) => M.time(r.created_at) },
            { key: "listing", label: "Listing", render: (r) => (r.listing ? r.listing.title : r.listing_id) },
            { key: "reason", label: "Reason", render: (r) => ui.badge(r.reason, "warning") },
            { key: "detail", label: "Detail" },
            { key: "id", label: "", render: (r) => (r.status === "open" ? M.reasonAction("Close", async (res) => { await M.post(`/v1/staff/listing-reports/${enc(r.id)}/close`, { resolution: res }); return "Closed."; }, { field: "Resolution", placeholder: "What was done", max: 500 }) : el("span", { class: "pp-muted" }, r.resolution || "")) },
          ],
        });
      };
      pick(tab0 === "listings" ? "listings" : "reports");
    }),
  });

  function listingActions(l) {
    const id = enc(l.id);
    return el("div", { class: "pm-actions" },
      l.status === "suspended"
        ? M.reasonAction("Reinstate", async (reason) => { await M.post(`/v1/staff/listings/${id}/moderation`, { action: "reinstate", reason }); return "Reinstated."; })
        : M.reasonAction("Suspend", async (reason) => { await M.post(`/v1/staff/listings/${id}/moderation`, { action: "suspend", reason }); return "Suspended. It is out of the catalog."; }, { danger: true }),
      M.reasonAction("Kill switch", async (reason) => { await M.post(`/v1/staff/marketplace/listings/${id}/kill`, { action: "kill", reason }); return "Killed. Its workers are stopping."; }, { danger: true }));
  }

  // ---- Hosted jobs ---------------------------------------------------------------------------

  M.page({
    key: "jobs", title: "Hosted jobs", group: "Marketplace", icon: "jobs", allow: M.allow(["reviewer", "ops"]),
    render: (view) => M.guard(view, () => {
      view.append(ui.header({ title: "Hosted jobs", text: "Publisher-hosted jobs: state, delivery and charge. Never their input or results." }));
      M.list(view, {
        search: false, caption: "Hosted jobs",
        chips: { name: "state", label: "State", value: "", options: [["", "Every state"], ["queued", "Queued"], ["running", "Running"], ["succeeded", "Succeeded"], ["failed", "Failed"], ["expired", "Expired"], ["cancelled", "Cancelled"]] },
        fetch: (p) => { p.delete("limit"); p.delete("cursor"); return M.get(`/v1/staff/hosted-jobs?${p}`); },
        empty: { icon: "jobs", title: "No hosted jobs" },
        columns: [
          { key: "created_at", label: "Created", render: (r) => M.time(r.created_at), sort: (r) => String(r.created_at || "") },
          { key: "listing_title", label: "Listing", sort: true },
          { key: "operation", label: "Operation", mono: true },
          { key: "execution", label: "State", render: (r) => M.status(r.execution), sort: true },
          { key: "delivery", label: "Delivery", render: (r) => (r.delivery ? M.status(r.delivery) : "—") },
          { key: "charge_state", label: "Charge", render: (r) => (r.charge_state ? M.status(r.charge_state) : "free") },
          { key: "error_code", label: "Error", mono: true },
        ],
      });
    }),
  });

  // ---- Apps & releases -----------------------------------------------------------------------

  M.page({
    key: "releases", title: "Apps & releases", group: "Marketplace", icon: "apps", allow: M.allow(["reviewer", "ops"]),
    render: (view, ctx) => M.guard(view, async () => {
      view.append(ui.header({ title: "Apps & releases", text: "Release security, re-scan findings, publisher quotas and kill switch, and reproducible source builds." }));
      if (M.has(ctx, "reviewer")) {
        const flags = el("div");
        view.append(ui.section({ title: "Security flags", text: "Blocking findings from the periodic re-scan of published releases. Quarantine first, then acknowledge with what you decided." }, flags));
        let all = false;
        const load = async () => {
          flags.replaceChildren(ui.loading(2));
          let r;
          try { r = await M.get(`/v1/staff/marketplace/security-flags${all ? "?all=true" : ""}`); } catch (e) { if (!(e instanceof PP.Redirecting)) flags.replaceChildren(M.errorBox(e, load)); return; }
          flags.replaceChildren(
            el("div", { class: "pm-toolbar" }, M.chips({ label: "Show", value: all ? "all" : "open", options: [["open", "Needs a look"], ["all", "All, including acknowledged"]], onChange: (v) => { all = v === "all"; load(); } })),
            ui.table({
              rows: r.data || [], pageSize: 50, caption: "Security flags",
              empty: { icon: "security", title: "No open security flags", text: "Published releases are re-scanned every 30 days." },
              columns: [
                { key: "created_at", label: "Raised", render: (f) => M.time(f.created_at) },
                { key: "release_id", label: "Release", render: (f) => el("div", { class: "pm-stack" }, el("strong", {}, `${f.publisher_id} ${f.version}`), M.mono(f.release_id)) },
                { key: "findings", label: "Findings", render: (f) => `${Array.isArray(f.findings) ? f.findings.length : 0} blocking` },
                { key: "security_state", label: "Security", render: (f) => M.status(f.security_state) },
                { key: "flag_id", label: "", render: (f) => (f.acknowledged_at ? el("span", { class: "pp-muted" }, `Acknowledged ${M.day(f.acknowledged_at)}${f.note ? ": " + f.note : ""}`)
                  : M.reasonAction("Acknowledge", async (note) => { await M.post(`/v1/staff/marketplace/security-flags/${enc(f.flag_id)}/acknowledge`, { note }); load(); return "Acknowledged."; }, { field: "What staff decided" })) },
              ],
            }));
        };
        load();
        view.append(ui.section({ title: "Release security", text: "Quarantine stops a release's jobs and workers and can be lifted; revoke is permanent." }, ui.card({}, securityForm(null))));
      }
      view.append(ui.section({ title: "Publisher controls" }, el("div", { class: "pp-grid pm-grid-2" },
        ui.card({ title: "Quotas", text: "Builds, workers, broker tokens and job rate for one publisher." }, M.form([
          { name: "pid", label: "Publisher id", required: true, placeholder: "pub_…", wide: true },
          { name: "builds", label: "Builds per day", type: "number", min: "0", step: "1", required: true, value: "5" },
          { name: "workers", label: "Workers", type: "number", min: "0", step: "1", required: true, value: "2" },
          { name: "tokens", label: "Broker tokens a month", type: "number", min: "0", step: "1", required: true, value: "1000000" },
          { name: "jpm", label: "Jobs per minute", type: "number", min: "0", step: "1", required: true, value: "60" },
          { name: "reason", label: "Reason", required: true, maxlength: 2000, wide: true },
        ], { submit: "Set quotas", cls: "pm-form-grid", onSubmit: async (v) => {
          const n = (x) => parseInt(x, 10);
          await M.post(`/v1/staff/marketplace/publishers/${enc(v.pid)}/quotas`, { builds_per_day: n(v.builds), max_workers: n(v.workers), monthly_broker_tokens: n(v.tokens), jobs_per_minute: n(v.jpm), reason: v.reason }, "PUT");
          return "Quotas set.";
        } })),
        ui.card({ title: "Kill switch", text: "Stop every listing of a publisher, or restore them.", tone: "danger" }, M.form([
          { name: "pid", label: "Publisher id", required: true, placeholder: "pub_…" },
          { name: "action", label: "Action", type: "select", value: "kill", options: [["kill", "Kill every listing of this publisher"], ["restore", "Restore"]] },
          { name: "reason", label: "Reason", required: true, maxlength: 2000 },
        ], { submit: "Apply", danger: true, onSubmit: async (v) => {
          await M.post(`/v1/staff/marketplace/publishers/${enc(v.pid)}/kill`, { action: v.action, reason: v.reason });
          return v.action === "kill" ? "Publisher killed." : "Publisher restored.";
        } })))));
      const out = el("div");
      view.append(ui.section({ title: "Source builds", text: "Ask the build plane for a reproducible build of a pinned commit, or look one up." }, el("div", { class: "pp-grid pm-grid-2" },
        ui.card({ title: "Request a build" }, M.form([
          { name: "pid", label: "Publisher id", required: true, placeholder: "pub_…" },
          { name: "repo", label: "Repository", required: true, placeholder: "https://github.com/…" },
          { name: "commit", label: "Commit", required: true, placeholder: "40 or 64 hex characters", pattern: "[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", patternHint: "A full commit hash: 40 or 64 hex characters." },
        ], { submit: "Request build", onSubmit: async (v) => {
          const r = await M.post("/v1/staff/marketplace/source-builds", { publisher_id: v.pid, repo_url: v.repo, commit: v.commit });
          return `Build ${(r.data && (r.data.id || r.data.build_id)) || ""} requested.`;
        } })),
        ui.card({ title: "Look up a build" }, ui.form([{ name: "id", label: "Build id", required: true }], { submit: "Look up", cls: "pm-form-inline", onSubmit: async (v) => {
          out.replaceChildren(ui.loading(2));
          try {
            const b = (await M.get(`/v1/staff/marketplace/source-builds/${enc(v.id)}`)).data || {};
            out.replaceChildren(M.kv(Object.entries(b).filter(([, x]) => typeof x !== "object").map(([k, x]) => [k.replace(/_/g, " "), x, k])));
          } catch (e) { if (!(e instanceof PP.Redirecting)) out.replaceChildren(e.status === 404 ? ui.empty({ title: "No such build" }) : M.errorBox(e)); }
        } }), out))));
    }),
  });
})();
