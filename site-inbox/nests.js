// Nests Phase B3–B5 in the inbox: the status header above a conversation with a Nest, and the
// Nest's management dialog (files, approvals, usage, pause/resume, rename, export, delete).
//
// Everything here appears only when the postbox reports the `nests` feature. A conversation with a
// Nest is ordinary chat: the composer is unchanged. Approving an action posts back the exact
// `args_hash` that was shown, so a call that changed since cannot ride on an old click.
(function () {
  "use strict";
  const host = () => window.PigeonpostNestsHost || null;
  const $ = (id) => document.getElementById(id);
  const enabled = () => Boolean(host() && host().features().has("nests"));
  const PRODUCTS_URL = "https://pigeonpost.dev/account#products";
  const POLL_MS = 20000;
  const STATUS = {
    ready: ["Ready", "Your Nest is ready."],
    working: ["Working…", "Your Nest is working on a message."],
    needs_you: ["Needs you", "This Nest is paused until its package is paid for."],
    paused: ["Paused", "This Nest is paused. Messages wait until it is resumed."],
    unavailable: ["Unavailable", "This Nest can't answer right now."],
  };
  const REASON = {
    payment: "This Nest is paused until its package is paid for.",
    over_capacity: "Your package has fewer Nests than you have. Delete one or upgrade.",
    setting_up: "Setting up — this takes a few seconds.",
    failed: "Setup failed.",
  };

  const MAX_UPLOAD = 25 * 1024 * 1024; // the postbox's own limit (nest_files.rs)

  let nests = new Map(); // address -> {nest_id, label}
  let pollTimer = null;
  let headPeer = null;
  let headNest = null; // the Nest the status bar is polling; a redraw of the same thread keeps it
  let headGen = 0;
  let refreshGen = 0;
  let manageGen = 0;

  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? "" : String(v));
    }
    for (const c of children.flat()) if (c != null && c !== false) node.append(c);
    return node;
  }

  async function request(method, path, { body, raw, headers } = {}, renewed = false) {
    const h = host();
    const res = await fetch(h.postbox() + path, {
      method,
      headers: Object.assign({ authorization: "Bearer " + h.token() }, body ? { "content-type": "application/json" } : {}, headers || {}),
      body: body ? JSON.stringify(body) : raw,
    });
    if (res.status === 401 && !renewed && (await h.renew())) return request(method, path, { body, raw, headers }, true);
    if (raw === undefined && method === "GET" && /\/export$/.test(path)) return res;
    const value = await res.json().catch(() => ({}));
    if (!res.ok) throw Object.assign(new Error(value.detail || value.error || "http_" + res.status), { status: res.status, code: value.error, body: value });
    return value;
  }

  /// Percent-encode a display name for the `x-file-name` header (UTF-8, header-safe).
  function encodeName(name) {
    return Array.from(new TextEncoder().encode(name))
      .map((b) => ((b >= 0x30 && b <= 0x39) || (b >= 0x41 && b <= 0x5a) || (b >= 0x61 && b <= 0x7a) || "-_.~".includes(String.fromCharCode(b)) ? String.fromCharCode(b) : "%" + b.toString(16).toUpperCase().padStart(2, "0")))
      .join("");
  }

  // ---- which conversations are Nests ------------------------------------------------------------

  async function refresh() {
    const gen = ++refreshGen;
    if (!enabled()) { nests = new Map(); return; }
    const next = new Map();
    try {
      const mine = await request("GET", "/v1/nests");
      for (const n of mine.nests || []) if (n.address) next.set(n.address, { nest_id: n.nest_id, label: n.label });
    } catch (_) { /* flag off for this account, or unreachable */ }
    try {
      const orgs = (await request("GET", "/v1/orgs")).orgs || [];
      for (const o of orgs) {
        try {
          const v = await request("GET", `/v1/orgs/${encodeURIComponent(o.id)}/nests`);
          for (const n of v.nests || []) if (n.address) next.set(n.address, { nest_id: n.nest_id, label: n.label, org: o.id });
        } catch (_) { /* not a member with Nests */ }
      }
    } catch (_) { /* no orgs */ }
    if (gen === refreshGen) nests = next; // a slower, older answer never replaces a newer one
  }

  function syncNav() {
    const nav = $("side-nav");
    if (!nav) return;
    let link = $("products-link");
    if (enabled() && !link) {
      link = el("a", { id: "products-link", class: "side-nav-btn", href: PRODUCTS_URL, target: "_blank", rel: "noopener" }, el("span", { text: "Products" }));
      nav.append(link);
      nav.hidden = false;
    }
    if (link) link.hidden = !enabled();
  }

  /// The Products page links to a Nest's conversation as `#peer=<address>`. Only the account's own
  /// Nests open this way, so a crafted link cannot start a conversation with anyone else.
  function followDeepLink() {
    const m = /^#peer=(.+)$/.exec(window.location.hash || "");
    if (!m) return;
    let peer;
    try { peer = decodeURIComponent(m[1]); } catch (_) { return; }
    if (!nests.has(peer)) return;
    window.history.replaceState(window.history.state, "", window.location.pathname + window.location.search);
    host().openConversation?.(peer);
  }

  async function onMailbox() {
    syncNav();
    await refresh();
    if (headPeer) renderHead(headPeer);
    if (enabled()) followDeepLink();
  }

  // ---- the status header ------------------------------------------------------------------------

  function clearHead() {
    clearInterval(pollTimer);
    pollTimer = null;
    headNest = null;
    headGen++;
    $("nest-status")?.remove();
  }

  function setText(id, value) {
    const node = $(id);
    if (node.textContent !== value) node.textContent = value; // the bar is a live region
  }

  function renderHead(peer) {
    headPeer = peer;
    const nest = enabled() && peer ? nests.get(peer) : null;
    if (!nest) { clearHead(); return; }
    // The inbox redraws a thread on every new message; that must not mean a new poll each time.
    if ($("nest-status") && headNest === nest && pollTimer) return;
    headNest = nest;
    if (!$("nest-status")) {
      const open = () => openManage(headPeer);
      const bar = el("div", { id: "nest-status", class: "nest-status", role: "status", "aria-live": "polite" },
        el("span", { class: "nest-status-dot", "aria-hidden": "true" }),
        el("span", { id: "nest-status-text" }),
        el("span", { id: "nest-status-note", class: "nest-status-note" }),
        el("a", { id: "nest-status-pay", class: "linkish", href: PRODUCTS_URL, target: "_blank", rel: "noopener", hidden: true, text: "See packages" }),
        el("button", { type: "button", id: "nest-approvals-btn", class: "linkish", hidden: true, onclick: open }),
        el("button", { type: "button", id: "nest-manage-btn", class: "linkish", onclick: open }, "Manage"));
      const head = $("thread-head");
      if (head) head.after(bar); else document.body.append(bar);
    }
    poll(nest);
    clearInterval(pollTimer);
    pollTimer = setInterval(() => { if (!document.hidden) poll(nest); }, host().pollMs ?? POLL_MS);
  }

  function statusNote(v) {
    const base = REASON[v.status_reason] || (STATUS[v.status] || STATUS.unavailable)[1];
    if (v.status !== "needs_you" || !v.delete_after) return base;
    return `${base} It will be deleted on ${new Date(v.delete_after * 1000).toLocaleDateString()} unless it is paid for. Download anything you need before then.`;
  }

  async function poll(nest) {
    const gen = ++headGen;
    const id = encodeURIComponent(nest.nest_id);
    const [v, cards] = await Promise.all([
      request("GET", `/v1/nests/${id}`).catch(() => null),
      request("GET", `/v1/nests/${id}/approvals`).catch(() => null),
    ]);
    if (gen !== headGen || !$("nest-status")) return;
    const status = v ? v.status || "unavailable" : "unavailable";
    $("nest-status").dataset.status = status;
    setText("nest-status-text", (STATUS[status] || STATUS.unavailable)[0]);
    setText("nest-status-note", v ? statusNote(v) : "Couldn't reach your Nest's status.");
    $("nest-status-pay").hidden = !(v && (v.status_reason === "payment" || v.status_reason === "over_capacity"));
    const pending = cards ? (cards.approvals || []).filter((a) => a.status === "pending").length : 0;
    const btn = $("nest-approvals-btn");
    btn.hidden = !pending;
    setText("nest-approvals-btn", pending ? `${pending} action${pending === 1 ? "" : "s"} to approve` : "");
  }

  // ---- management dialog ------------------------------------------------------------------------

  function dialog() {
    let d = $("nest-dialog");
    if (d) return d;
    d = el("div", { class: "notes-dialog", id: "nest-dialog", role: "dialog", "aria-modal": "true", "aria-labelledby": "nest-title", hidden: true },
      el("div", { class: "notes-panel" },
        el("header", { class: "notes-head" },
          el("h2", { id: "nest-title", text: "Nest" }),
          el("button", { type: "button", class: "linkish", id: "nest-close", text: "Close", onclick: () => closeManage() })),
        el("div", { class: "notes-body", id: "nest-body" })));
    d.addEventListener("click", (e) => { if (e.target === d) closeManage(); });
    document.addEventListener("keydown", (e) => { if (e.key === "Escape" && !d.hidden) closeManage(); });
    document.body.append(d);
    return d;
  }

  let opener = null;
  function openManage(peer) {
    const nest = nests.get(peer);
    if (!nest) return;
    opener = document.activeElement;
    const d = dialog();
    d.hidden = false;
    $("nest-title").textContent = nest.label || "Nest";
    $("nest-body").textContent = "";
    $("nest-body").append(el("p", { class: "muted", role: "status", text: "Loading…" }));
    showManage(nest);
    $("nest-close").focus();
  }

  function closeManage() {
    const d = $("nest-dialog");
    if (!d) return;
    manageGen++; // whatever is still loading must not draw into a closed dialog
    d.hidden = true;
    $("nest-body").textContent = "";
    if (opener && opener.focus) opener.focus();
  }

  function section(title, id) {
    return el("section", { class: "nest-section", "aria-labelledby": id }, el("h3", { id, text: title }));
  }

  function size(bytes) {
    const b = Number(bytes) || 0;
    if (b >= 1073741824) return `${(b / 1073741824).toFixed(1).replace(/\.0$/, "")} GB`;
    if (b >= 1048576) return `${(b / 1048576).toFixed(1).replace(/\.0$/, "")} MB`;
    return `${Math.ceil(b / 1024)} KB`;
  }
  const count = (n) => Number(n ?? 0).toLocaleString();
  const FILE_STATUS = { uploading: "Adding…", parsed: "Ready", partial: "Partly read", rejected: "No text could be read" };

  async function showManage(nest) {
    const gen = ++manageGen;
    const id = encodeURIComponent(nest.nest_id);
    const get = (path) => request("GET", `/v1/nests/${id}${path}`).catch(() => null);
    const [detail, ap, fl, us] = await Promise.all([get(""), get("/approvals"), get("/files"), get("/usage")]);
    const body = $("nest-body");
    // Everything is drawn in one step, and only by the newest load, so sections never double up.
    if (gen !== manageGen || $("nest-dialog").hidden) return;
    body.textContent = "";
    const toast = (m) => host().toast(m);
    if (detail?.label) { nest.label = detail.label; $("nest-title").textContent = detail.label; }

    /// One action at a time: every button waits, so a double click cannot act twice.
    async function guard(fn) {
      const buttons = [...body.querySelectorAll("button")];
      buttons.forEach((b) => { b.disabled = true; });
      try { await fn(); } finally { buttons.forEach((b) => { b.disabled = false; }); }
    }
    const redraw = async () => {
      await showManage(nest);
      if (headNest === nest) poll(nest);
    };

    // Approvals first: they are what the Nest is waiting on.
    const appr = section("Approvals", "nest-appr-title");
    body.append(appr);
    if (!ap) appr.append(el("p", { text: "Couldn't load approvals." }));
    else {
      const pending = (ap.approvals || []).filter((a) => a.status === "pending");
      if (!pending.length) appr.append(el("p", { class: "muted", text: "Nothing is waiting for you." }));
      for (const a of pending) {
        const call = a.call?.args || {};
        // Written by the model, possibly from a file's text: shown verbatim, never interpreted.
        appr.append(el("div", { class: "nest-card", "data-approval": a.approval_id },
          el("p", { text: a.tool === "send_message" ? `Send a message to ${call.to}:` : `Run ${a.tool}:` }),
          el("pre", { class: "nest-call", text: a.tool === "send_message" ? call.body : JSON.stringify(call, null, 2) }),
          a.expires_at ? el("p", { class: "muted", text: `Expires ${new Date(a.expires_at * 1000).toLocaleString()}` }) : null,
          el("div", { class: "nest-actions" },
            el("button", { type: "button", class: "btn-primary", "data-approve": a.approval_id, "aria-label": `Approve: message to ${call.to || a.tool}`, onclick: () => guard(() => decide(a, "approve")) }, "Approve"),
            el("button", { type: "button", class: "btn-ghost", "data-reject": a.approval_id, "aria-label": `Reject: message to ${call.to || a.tool}`, onclick: () => guard(() => decide(a, "reject")) }, "Reject"))));
      }
    }

    // Files.
    const files = section("Files", "nest-files-title");
    body.append(files);
    const input = el("input", { type: "file", id: "nest-file-input", "aria-label": "Choose a file to add" });
    files.append(el("div", { class: "nest-actions" }, input,
      el("button", { type: "button", class: "btn-ghost", id: "nest-upload", onclick: () => guard(() => upload(input.files && input.files[0])) }, "Add file")));
    if (!fl) files.append(el("p", { text: "Couldn't load files." }));
    else {
      files.append(el("p", { class: "muted", id: "nest-files-used", text: `${size(fl.used_bytes)} of ${size(fl.quota_bytes)} used` }));
      const list = el("ul", { class: "nest-files" });
      for (const f of fl.files || []) {
        const cov = f.coverage?.units_total ? ` · text found in ${f.coverage.units_text} of ${f.coverage.units_total} parts` : "";
        list.append(el("li", { "data-file": f.file_id },
          el("strong", { text: f.name }),
          el("span", { class: "muted", text: ` ${size(f.bytes)} · ${FILE_STATUS[f.status] || f.status}${cov}` }),
          f.reason ? el("span", { class: "muted", text: ` · ${f.reason}` }) : null,
          (f.warnings || []).length ? el("details", {}, el("summary", { text: `${f.warnings.length} note${f.warnings.length === 1 ? "" : "s"}` }), el("ul", {}, f.warnings.map((w) => el("li", { text: String(w) })))) : null,
          " ",
          el("button", { type: "button", class: "linkish", "data-delete-file": f.file_id, "aria-label": `Remove ${f.name}`, onclick: () => guard(() => removeFile(f)) }, "Remove")));
      }
      files.append(list);
    }

    // Usage.
    const usage = section("This month", "nest-usage-title");
    body.append(usage);
    if (!us) usage.append(el("p", { text: "Couldn't load usage." }));
    else {
      const lines = [];
      if (us.input_tokens?.limit) lines.push(`AI: ${count(us.input_tokens.used)} of ${count(us.input_tokens.limit)} input and ${count(us.output_tokens?.used)} of ${count(us.output_tokens?.limit)} output tokens`);
      else lines.push("No AI connected.");
      if (us.requests != null) lines.push(`Requests: ${count(us.requests)}`);
      if (us.running_secs != null) lines.push(`Running: ${(us.running_secs / 3600).toFixed(1)} hours${us.package ? ` (your package includes ${us.package.active_hours_pooled}, shared)` : ""}`);
      if (us.files_bytes != null) lines.push(`Files stored: ${size(us.files_bytes)}`);
      usage.append(el("ul", { id: "nest-usage" }, lines.map((l) => el("li", { text: l }))));
    }

    // Controls: only for someone who manages this Nest. The server enforces it; an operator of a
    // company Nest just isn't shown buttons that would answer 404.
    if (detail?.role === "owner") body.append(controls(detail));
    else if (!detail) body.append(el("p", { text: "Couldn't load this Nest's controls." }));
    if (document.activeElement === document.body || !document.activeElement) $("nest-close").focus();

    function controls(d) {
      const ctl = section("Nest", "nest-ctl-title");
      const live = d.state === "ready" || d.state === "running" || d.state === "stopped";
      const heldForFunding = d.paused_reason === "unfunded" || d.paused_reason === "over_capacity";
      const name = el("input", { type: "text", id: "nest-rename", value: nest.label || "", maxlength: "64", "aria-label": "Nest name" });
      const confirmBox = el("input", { type: "text", id: "nest-delete-confirm", "aria-label": "Type the Nest's name to delete it", autocomplete: "off" });
      ctl.append(
        el("div", { class: "nest-actions" },
          live ? el("button", { type: "button", class: "btn-ghost", id: "nest-pause", onclick: () => guard(() => lifecycle("pause")) }, "Pause") : null,
          d.state === "paused" && !heldForFunding ? el("button", { type: "button", class: "btn-ghost", id: "nest-resume", onclick: () => guard(() => lifecycle("resume")) }, "Resume") : null,
          el("button", { type: "button", class: "btn-ghost", id: "nest-export", onclick: () => guard(exportNest) }, "Download everything")),
        heldForFunding ? el("p", { class: "muted", id: "nest-held", text: d.paused_reason === "unfunded" ? "This Nest comes back on its own once its package is paid for." : "This Nest comes back on its own once your package has room for it." }) : null,
        el("div", { class: "nest-actions" }, name,
          el("button", { type: "button", class: "btn-ghost", id: "nest-rename-go", onclick: () => guard(() => rename(name.value)) }, "Rename")),
        el("p", { class: "muted", text: `Deleting a Nest erases its files and conversations for good, and it won't come back. Download everything first. To confirm, type ${nest.label}.` }),
        el("div", { class: "nest-actions" }, confirmBox,
          el("button", { type: "button", class: "btn-danger", id: "nest-delete", onclick: () => guard(() => removeNest(confirmBox.value)) }, "Delete Nest")));
      return ctl;
    }

    async function decide(a, what) {
      try {
        await request("POST", `/v1/nests/${id}/approvals/${encodeURIComponent(a.approval_id)}/${what}`, { body: { args_hash: a.args_hash } });
        toast(what === "approve" ? "Approved and done." : "Rejected.");
      } catch (e) {
        toast({
          approval_changed: "That action changed since it was shown. Here is the current version.",
          approval_closed: "That approval was already decided, replaced or expired.",
          recipient_not_allowed: "A Nest may only message you or people you have allowed.",
          rate_limited: "This Nest has sent its maximum of messages for today.",
          tool_failed: "Approved, but the message could not be sent.",
        }[e.code] || `Couldn't do that: ${e.message}`);
      }
      await redraw();
    }
    async function upload(file) {
      if (!file) { toast("Choose a file first."); return; }
      if (!file.size) { toast("That file is empty."); return; }
      if (file.size > MAX_UPLOAD) { toast("Files can be at most 25 MB."); return; }
      try {
        const v = await request("POST", `/v1/nests/${id}/files`, { raw: file, headers: { "x-file-name": encodeName(file.name || "file") } });
        toast(v.status === "rejected" ? `Added, but no text could be read: ${v.reason || "unsupported file"}.`
          : v.status === "partial" ? "Added, but only part of it could be read." : "File added.");
      } catch (e) {
        toast(e.status === 413 ? "Files can be at most 25 MB." : {
          storage_full: "This Nest's storage is full.",
          too_many_files: "This Nest holds its maximum number of files. Remove one first.",
          rate_limited: "Too many uploads this hour.",
          upload_failed: "The file wasn't stored. Try again.",
        }[e.code] || `Couldn't add the file: ${e.message}`);
      }
      await redraw();
    }
    async function removeFile(f) {
      try { await request("DELETE", `/v1/nests/${id}/files/${encodeURIComponent(f.file_id)}`); toast("File removed."); }
      catch (e) { toast(`Couldn't remove it: ${e.message}`); }
      await redraw();
    }
    async function lifecycle(what) {
      try { await request("POST", `/v1/nests/${id}/${what}`); toast(what === "pause" ? "Paused." : "Resumed."); }
      catch (e) {
        toast({ needs_payment: "This Nest comes back once its package is paid for.", over_capacity: "Your package has fewer Nests than you have. Delete one or upgrade." }[e.code] || `Couldn't ${what}: ${e.message}`);
      }
      await redraw();
    }
    async function rename(value) {
      const label = value.trim();
      if (!label) { toast("Give it a name first."); return; }
      try { await request("PATCH", `/v1/nests/${id}`, { body: { label } }); toast("Renamed."); }
      catch (e) { toast(e.code === "bad_label" ? "Names are 1 to 64 printable characters." : `Couldn't rename: ${e.message}`); return; }
      nest.label = label;
      await redraw();
    }
    async function exportNest() {
      toast("Preparing your download…");
      try {
        const res = await request("GET", `/v1/nests/${id}/export`);
        if (!res.ok) throw Object.assign(new Error("http_" + res.status), { status: res.status });
        const url = URL.createObjectURL(await res.blob());
        const safe = (nest.label || "").replace(/[^A-Za-z0-9_-]/g, "").slice(0, 40) || "export";
        const a = el("a", { href: url, download: `nest-${safe}.zip` });
        document.body.append(a); a.click(); a.remove();
        setTimeout(() => URL.revokeObjectURL(url), 60000);
      } catch (e) { toast(e.status === 413 ? "This Nest is too large to download here. Contact support." : "Couldn't download it."); }
    }
    async function removeNest(typed) {
      const label = typed.trim();
      if (label !== (nest.label || "")) { toast("Type the Nest's name exactly to delete it."); $("nest-delete-confirm").focus(); return; }
      try {
        await request("DELETE", `/v1/nests/${id}`, { body: { confirm: label } });
      } catch (e) { toast(`Couldn't delete it: ${e.message}`); return; }
      toast("Nest deleted.");
      closeManage();
      await refresh();
      if (headPeer) renderHead(headPeer);
    }
  }

  window.PigeonpostNests = { onMailbox, renderHead, refresh, encodeName, isNest: (peer) => nests.has(peer) };
})();
