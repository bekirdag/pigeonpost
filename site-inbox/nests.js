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

  let nests = new Map(); // address -> {nest_id, label}
  let pollTimer = null;
  let headPeer = null;
  let headGen = 0;

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
    nests = next;
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

  async function onMailbox() {
    syncNav();
    await refresh();
    if (headPeer) renderHead(headPeer);
  }

  // ---- the status header ------------------------------------------------------------------------

  function clearHead() {
    clearInterval(pollTimer);
    pollTimer = null;
    $("nest-status")?.remove();
  }

  function renderHead(peer) {
    headPeer = peer;
    const nest = enabled() && peer ? nests.get(peer) : null;
    if (!nest) { clearHead(); return; }
    let bar = $("nest-status");
    if (!bar) {
      bar = el("div", { id: "nest-status", class: "nest-status", role: "status", "aria-live": "polite" },
        el("span", { class: "nest-status-dot", "aria-hidden": "true" }),
        el("span", { id: "nest-status-text" }),
        el("span", { id: "nest-status-note", class: "nest-status-note" }),
        el("button", { type: "button", id: "nest-manage-btn", class: "linkish", onclick: () => openManage(peer) }, "Manage"));
      const head = $("thread-head");
      if (head) head.after(bar); else document.body.append(bar);
    }
    $("nest-manage-btn").onclick = () => openManage(peer);
    poll(nest);
    clearInterval(pollTimer);
    pollTimer = setInterval(() => poll(nest), host().pollMs ?? POLL_MS);
  }

  async function poll(nest) {
    const gen = ++headGen;
    try {
      const v = await request("GET", `/v1/nests/${encodeURIComponent(nest.nest_id)}`);
      if (gen !== headGen || !$("nest-status")) return;
      const [label, sentence] = STATUS[v.status] || STATUS.unavailable;
      $("nest-status").dataset.status = v.status || "unavailable";
      $("nest-status-text").textContent = label;
      $("nest-status-note").textContent = REASON[v.status_reason] || sentence;
    } catch (_) {
      if (gen !== headGen || !$("nest-status")) return;
      $("nest-status").dataset.status = "unavailable";
      $("nest-status-text").textContent = STATUS.unavailable[0];
      $("nest-status-note").textContent = "Couldn't reach your Nest's status.";
    }
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
    showManage(nest);
    $("nest-close").focus();
  }

  function closeManage() {
    const d = $("nest-dialog");
    if (!d) return;
    d.hidden = true;
    $("nest-body").textContent = "";
    if (opener && opener.focus) opener.focus();
  }

  function section(title, id) {
    return el("section", { class: "nest-section", "aria-labelledby": id }, el("h3", { id, text: title }));
  }

  async function showManage(nest) {
    const body = $("nest-body");
    body.textContent = "";
    const id = encodeURIComponent(nest.nest_id);
    const toast = (m) => host().toast(m);

    // Approvals first: they are what the Nest is waiting on.
    const appr = section("Approvals", "nest-appr-title");
    body.append(appr);
    try {
      const { approvals } = await request("GET", `/v1/nests/${id}/approvals`);
      const pending = (approvals || []).filter((a) => a.status === "pending");
      if (!pending.length) appr.append(el("p", { class: "muted", text: "Nothing is waiting for you." }));
      for (const a of pending) {
        const call = a.call?.args || {};
        appr.append(el("div", { class: "nest-card", "data-approval": a.approval_id },
          el("p", { text: a.tool === "send_message" ? `Send a message to ${call.to}:` : `Run ${a.tool}:` }),
          el("pre", { class: "nest-call", text: a.tool === "send_message" ? call.body : JSON.stringify(call, null, 2) }),
          el("div", { class: "nest-actions" },
            el("button", { type: "button", class: "btn", "data-approve": a.approval_id, onclick: () => decide(nest, a, "approve") }, "Approve"),
            el("button", { type: "button", class: "btn btn-secondary", "data-reject": a.approval_id, onclick: () => decide(nest, a, "reject") }, "Reject"))));
      }
    } catch (_) { appr.append(el("p", { text: "Couldn't load approvals." })); }

    // Files.
    const files = section("Files", "nest-files-title");
    body.append(files);
    const input = el("input", { type: "file", id: "nest-file-input", "aria-label": "Choose a file to add" });
    files.append(el("div", { class: "nest-actions" }, input,
      el("button", { type: "button", class: "btn btn-secondary", id: "nest-upload", onclick: () => upload(nest, input.files && input.files[0]) }, "Add file")));
    try {
      const v = await request("GET", `/v1/nests/${id}/files`);
      files.append(el("p", { class: "muted", text: `${Math.ceil(v.used_bytes / 1048576)} MB of ${Math.round(v.quota_bytes / 1073741824)} GB used` }));
      const list = el("ul", { class: "nest-files" });
      for (const f of v.files || []) {
        const cov = f.coverage?.units_total ? ` · text from ${f.coverage.units_text} of ${f.coverage.units_total}` : "";
        list.append(el("li", { "data-file": f.file_id },
          el("strong", { text: f.name }),
          el("span", { class: "muted", text: ` ${f.status}${cov}` }),
          f.reason ? el("span", { class: "muted", text: ` · ${f.reason}` }) : null,
          (f.warnings || []).length ? el("details", {}, el("summary", { text: `${f.warnings.length} note${f.warnings.length === 1 ? "" : "s"}` }), el("ul", {}, f.warnings.map((w) => el("li", { text: w })))) : null,
          el("button", { type: "button", class: "linkish", "data-delete-file": f.file_id, "aria-label": `Remove ${f.name}`, onclick: () => removeFile(nest, f) }, "Remove")));
      }
      files.append(list);
    } catch (_) { files.append(el("p", { text: "Couldn't load files." })); }

    // Usage.
    const usage = section("This month", "nest-usage-title");
    body.append(usage);
    try {
      const u = await request("GET", `/v1/nests/${id}/usage`);
      const lines = [];
      if (u.input_tokens?.limit) lines.push(`AI: ${u.input_tokens.used ?? 0} of ${u.input_tokens.limit} input and ${u.output_tokens?.used ?? 0} of ${u.output_tokens?.limit} output tokens`);
      else lines.push("No AI connected.");
      if (u.running_secs != null) lines.push(`Running: ${(u.running_secs / 3600).toFixed(1)} hours${u.package ? ` (your package includes ${u.package.active_hours_pooled}, shared)` : ""}`);
      usage.append(el("ul", {}, lines.map((l) => el("li", { text: l }))));
    } catch (_) { usage.append(el("p", { text: "Couldn't load usage." })); }

    // Controls.
    const ctl = section("Nest", "nest-ctl-title");
    body.append(ctl);
    const name = el("input", { id: "nest-rename", value: nest.label || "", maxlength: "64", "aria-label": "Nest name" });
    const confirmBox = el("input", { id: "nest-delete-confirm", "aria-label": "Type the Nest's name to delete it", autocomplete: "off" });
    ctl.append(
      el("div", { class: "nest-actions" },
        el("button", { type: "button", class: "btn btn-secondary", id: "nest-pause", onclick: () => lifecycle(nest, "pause") }, "Pause"),
        el("button", { type: "button", class: "btn btn-secondary", id: "nest-resume", onclick: () => lifecycle(nest, "resume") }, "Resume"),
        el("button", { type: "button", class: "btn btn-secondary", id: "nest-export", onclick: () => exportNest(nest) }, "Download everything")),
      el("div", { class: "nest-actions" }, name,
        el("button", { type: "button", class: "btn btn-secondary", id: "nest-rename-go", onclick: () => rename(nest, name.value) }, "Rename")),
      el("p", { class: "muted", text: "Deleting a Nest erases its files and conversations for good. Download everything first." }),
      el("div", { class: "nest-actions" }, confirmBox,
        el("button", { type: "button", class: "btn btn-danger", id: "nest-delete", onclick: () => removeNest(nest, confirmBox.value) }, "Delete Nest")));

    async function decide(n, a, what) {
      try {
        await request("POST", `/v1/nests/${encodeURIComponent(n.nest_id)}/approvals/${encodeURIComponent(a.approval_id)}/${what}`, { body: { args_hash: a.args_hash } });
        toast(what === "approve" ? "Approved and done." : "Rejected.");
      } catch (e) {
        toast({
          approval_changed: "That action changed since it was shown. Here is the current version.",
          approval_closed: "That approval was already decided, replaced or expired.",
          recipient_not_allowed: "A Nest may only message you or people you have allowed.",
        }[e.code] || `Couldn't do that: ${e.message}`);
      }
      showManage(n);
    }
    async function upload(n, file) {
      if (!file) { toast("Choose a file first."); return; }
      try {
        const v = await request("POST", `/v1/nests/${encodeURIComponent(n.nest_id)}/files`, { raw: file, headers: { "x-file-name": encodeName(file.name || "file") } });
        toast(v.status === "rejected" ? `Added, but no text could be read: ${v.reason || "unsupported file"}.` : "File added.");
      } catch (e) {
        toast({ storage_full: "This Nest's storage is full.", too_large: "Files can be at most 25 MB.", rate_limited: "Too many uploads this hour." }[e.code] || `Couldn't add the file: ${e.message}`);
      }
      showManage(n);
    }
    async function removeFile(n, f) {
      try { await request("DELETE", `/v1/nests/${encodeURIComponent(n.nest_id)}/files/${encodeURIComponent(f.file_id)}`); toast("File removed."); }
      catch (e) { toast(`Couldn't remove it: ${e.message}`); }
      showManage(n);
    }
    async function lifecycle(n, what) {
      try { await request("POST", `/v1/nests/${encodeURIComponent(n.nest_id)}/${what}`); toast(what === "pause" ? "Paused." : "Resumed."); }
      catch (e) {
        toast({ needs_payment: "This Nest comes back once its package is paid for.", over_capacity: "Your package has fewer Nests than you have. Delete one or upgrade." }[e.code] || `Couldn't ${what}: ${e.message}`);
      }
      if (headPeer) renderHead(headPeer);
    }
    async function rename(n, label) {
      try { await request("PATCH", `/v1/nests/${encodeURIComponent(n.nest_id)}`, { body: { label: label.trim() } }); n.label = label.trim(); $("nest-title").textContent = n.label; toast("Renamed."); }
      catch (e) { toast(`Couldn't rename: ${e.message}`); }
    }
    async function exportNest(n) {
      try {
        const res = await request("GET", `/v1/nests/${encodeURIComponent(n.nest_id)}/export`);
        if (!res.ok) throw Object.assign(new Error("http_" + res.status), { status: res.status });
        const blob = await res.blob();
        const a = el("a", { href: URL.createObjectURL(blob), download: "nest-export.zip" });
        document.body.append(a); a.click(); a.remove();
      } catch (e) { toast(e.status === 413 ? "This Nest is too large to download here. Contact support." : "Couldn't download it."); }
    }
    async function removeNest(n, typed) {
      if (typed.trim() !== (n.label || "")) { toast("Type the Nest's name exactly to delete it."); $("nest-delete-confirm").focus(); return; }
      try {
        await request("DELETE", `/v1/nests/${encodeURIComponent(n.nest_id)}`, { body: { confirm: typed.trim() } });
        toast("Nest deleted.");
        closeManage();
        await refresh();
        if (headPeer) renderHead(headPeer);
      } catch (e) { toast(`Couldn't delete it: ${e.message}`); }
    }
  }

  window.PigeonpostNests = { onMailbox, renderHead, refresh, encodeName, isNest: (peer) => nests.has(peer) };
})();
