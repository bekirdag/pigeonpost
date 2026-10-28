// Pigeonpost inbox — persistent end-to-end encrypted groups and disappearing messages (Phase 3a).
//
// Everything a group shows is decrypted here, in this browser, by the MLS library in
// vendor/groupcrypto (groups-core.js drives it). The postbox stores ciphertext and membership
// metadata; it cannot read a group, and nothing in this file sends plaintext to it. Server notices
// (who joined, who changed a setting) are metadata and are rendered as such; membership changes are
// also checked against the verified commit, and a mismatch is shown as a warning.
//
// Hidden entirely unless the postbox enables `groups` for the account *and* the crypto library
// loads and verifies. There is no unencrypted fallback.
//
// Untrusted text (names, messages, filenames) is always inserted with textContent.
(function () {
  "use strict";

  const Core = window.PigeonpostGroupsCore;
  const host = () => window.PigeonpostGroupsHost;
  const $ = (id) => document.getElementById(id);
  const MAX_FILE = 25 * 1024 * 1024;

  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      else if (k === "dataset") Object.assign(node.dataset, v);
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else if (k === "hidden") node.hidden = Boolean(v);
      else node.setAttribute(k, v === true ? "" : String(v));
    }
    for (const c of children.flat()) if (c != null && c !== false) node.append(c instanceof Node ? c : document.createTextNode(String(c)));
    return node;
  }
  const svg = (paths, vb = "0 0 20 20") => {
    const ns = "http://www.w3.org/2000/svg";
    const s = document.createElementNS(ns, "svg");
    s.setAttribute("viewBox", vb);
    s.setAttribute("aria-hidden", "true");
    for (const d of paths) {
      const p = document.createElementNS(ns, "path");
      p.setAttribute("d", d);
      p.setAttribute("fill", "none");
      p.setAttribute("stroke", "currentColor");
      p.setAttribute("stroke-width", "1.6");
      p.setAttribute("stroke-linecap", "round");
      p.setAttribute("stroke-linejoin", "round");
      s.append(p);
    }
    return s;
  };
  const ICON = {
    group: ["M7 9a2.5 2.5 0 100-5 2.5 2.5 0 000 5zM2.5 16c0-2.5 2-4.2 4.5-4.2s4.5 1.7 4.5 4.2M13.5 9.2a2.2 2.2 0 100-4.4M14.5 11.9c1.8.4 3 1.8 3 4.1"],
    timer: ["M10 17a6 6 0 100-12 6 6 0 000 12zM10 8v3.2l2 1.3M8 2.5h4"],
    mute: ["M4 8h3l4-3.5v11L7 12H4zM14 8l4 4M18 8l-4 4"],
    phone: ["M6.6 2.9 4.5 3.3c-.8.2-1.4.9-1.3 1.7.5 6.1 5.7 11.3 11.8 11.8.8.1 1.5-.5 1.7-1.3l.4-2.1c.1-.6-.2-1.2-.8-1.4l-2.4-1c-.5-.2-1.1-.1-1.5.3l-.9.9a9 9 0 0 1-4-4l.9-.9c.4-.4.5-1 .3-1.5l-1-2.4c-.2-.6-.8-.9-1.4-.8Z"],
    video: ["M4.5 5h7a2 2 0 012 2v6a2 2 0 01-2 2h-7a2 2 0 01-2-2V7a2 2 0 012-2zM13.5 9l4-2.3v6.6l-4-2.3"],
    info: ["M10 17.2a7.2 7.2 0 100-14.4 7.2 7.2 0 000 14.4zM10 9v4.5M10 6.4v.1"],
    back: ["M12.5 4L6 10l6.5 6"],
    close: ["M5 5l10 10M15 5L5 15"],
    attach: ["M13.5 6.5l-5.9 5.9a1.8 1.8 0 002.5 2.5l6.2-6.2a3.3 3.3 0 00-4.7-4.7L4.6 10.9a4.8 4.8 0 006.8 6.8l5.3-5.3"],
    send: ["M3 10l14-6-6 14-2.2-5.8L3 10z"],
    lock: ["M5.5 9h9v7.5h-9zM7.2 9V6.8a2.8 2.8 0 015.6 0V9"],
    live: ["M10 11.6a1.6 1.6 0 100-3.2 1.6 1.6 0 000 3.2zM6.6 13.4a4.8 4.8 0 010-6.8M13.4 6.6a4.8 4.8 0 010 6.8M4.3 15.7a8 8 0 010-11.4M15.7 4.3a8 8 0 010 11.4"],
  };

  // ---- state ----------------------------------------------------------------------------------

  const G = {
    enabled: false,      // `groups` is on and the library loaded
    disappearing: false, // `disappearing` is on
    gc: null,
    loading: null,
    clients: new Map(),  // mailbox → core client
    mailbox: null,
    groups: [],
    open: null,          // group id on screen
    members: new Map(),  // gid → [{address, handle, role, state, revision}]
    handles: new Map(),  // address → handle
    devices: new Map(),  // mailbox → [{device_id, fingerprint_digits}]
    invitesOpen: false,
    syncing: new Set(),
    poll: null,
    callPoll: null,
    callState: null,
    persistent: true,
    generation: 0,
    error: null,
    receipts: new Map(), // gid → {members:[{address, handle, delivered_seq, seen_seq}], read_receipts} (Phase 1e)
    receiptsPoll: null,
    compose: null,       // {kind: "reply"|"edit", gid, x} while replying to or editing one message (Phase 1e)
    streaming: false,    // Phase 6: `streaming` is on
    streams: new Map(),  // gid → the live stream (or null), Phase 6
    dock: null,          // Phase 6: {container} while the chat is docked beside a live stream
  };
  const client = () => G.clients.get(G.mailbox) || null;
  const me = () => G.mailbox;

  // ---- api ------------------------------------------------------------------------------------

  async function rawApi(mailbox, path, o = {}, renewed) {
    const h = host();
    const headers = { authorization: "Bearer " + h.token(), accept: o.raw ? "application/octet-stream" : "application/json" };
    if (mailbox) headers["x-pigeonpost-identity"] = mailbox;
    if (o.device) headers["x-pigeonpost-device"] = o.device;
    let body;
    if (o.rawBody) { headers["content-type"] = "application/octet-stream"; body = o.rawBody; }
    else if (o.body) { headers["content-type"] = "application/json"; body = JSON.stringify(o.body); }
    const res = await fetch(h.postbox() + path, { method: o.method || "GET", headers, body });
    if (res.status === 401 && !renewed && (await h.renew())) return rawApi(mailbox, path, o, true);
    if (o.raw && res.ok) return res.arrayBuffer();
    const value = await res.json().catch(() => ({}));
    if (!res.ok) throw new Core.GroupApiError(res.status, value.error, value);
    return value;
  }
  const api = (path, o) => rawApi(me(), path, o);
  const groupPath = (gid, rest = "") => "/v1/groups/" + encodeURIComponent(gid) + rest;

  function explain(e) {
    const code = e && e.code;
    const known = {
      not_admin: "Only admins can do that.", owner_only: "Only the owner can do that.", human_required: "This needs you to be signed in yourself, not an agent key.",
      posting_restricted: "Only admins can send messages here.", banned: "That mailbox is banned from this group.", group_full: "The group is full.",
      invalid_link: "This invite link is no longer valid.", invite_required: "This group is invite only.", already_member: "Already a member.",
      group_deleted: "This group was deleted.", revision_conflict: "Someone else changed this at the same time. Try again.", not_member: "You are not a member of this group.",
      rate_limited: "Too many requests. Wait a moment and try again.", quota_exceeded: "This group is out of storage.", feature_disabled: "Groups are not enabled for this account.",
      not_found: "Not found.",
    };
    return known[code] || (e && e.body && e.body.message) || "Something went wrong.";
  }

  // ---- loading the library and the device -------------------------------------------------------

  async function loadCrypto() {
    if (G.gc) return G.gc;
    if (!G.loading) {
      G.loading = (async () => {
        if (window.PigeonpostGroupCryptoLoader) return window.PigeonpostGroupCryptoLoader();
        const url = new URL("vendor/groupcrypto/loader.js?v=5209113c6b17f23137fbac8ea2c90ee62f7b6d156e7827f3d95c9e70bea0578f", document.baseURI).href;
        const mod = await import(url);
        return mod.loadGroupCrypto();
      })();
    }
    try {
      G.gc = await G.loading;
    } catch (e) {
      G.loading = null;
      throw e;
    }
    return G.gc;
  }

  function storage() {
    try {
      if (window.indexedDB && window.crypto && window.crypto.subtle) return Core.idbStorage(window.indexedDB, window.crypto.subtle);
    } catch (_) { /* fall through */ }
    return Core.memoryStorage();
  }

  async function clientFor(mailbox) {
    if (G.clients.has(mailbox)) return G.clients.get(mailbox);
    const gc = await loadCrypto();
    const c = Core.createClient({
      gc, mailbox, storage: storage(),
      api: (path, o) => rawApi(mailbox, path, o),
      onEvent: (ev) => onCoreEvent(mailbox, ev),
    });
    const info = await c.init();
    G.persistent = info.persistent;
    G.clients.set(mailbox, c);
    c.topUpKeyPackages().catch(() => {});
    return c;
  }

  function onCoreEvent(mailbox, ev) {
    if (mailbox !== me()) return;
    if (ev.type === "history" && ev.groupId === G.open) renderView();
    if (ev.type === "removed" || ev.type === "resync") refresh();
    if (ev.type === "history") renderList();
  }

  // Called by app.js when the mailbox or the capabilities change.
  async function onMailbox() {
    const h = host();
    const mb = h && h.mailbox();
    const features = h ? h.features() : new Set();
    const gen = ++G.generation;
    G.disappearing = features.has("disappearing");
    G.streaming = features.has("streaming") && Boolean(window.PigeonpostLive);
    if (!mb || !features.has("groups")) {
      teardown();
      renderAll();
      return;
    }
    if (G.mailbox !== mb.address) { G.open = null; G.groups = []; G.members.clear(); }
    G.mailbox = mb.address;
    G.handles.set(mb.address, mb.handle);
    try {
      await clientFor(mb.address);
    } catch (e) {
      // No library, no groups: never a weaker fallback.
      G.enabled = false;
      G.error = "unavailable";
      renderAll();
      return;
    }
    if (gen !== G.generation) return;
    G.enabled = true;
    G.error = null;
    ensureDom();
    renderAll();
    await client().processWelcomes().catch(() => {});
    await refresh();
    pickUpJoinLink();
    pickUpGroupLink();
    clearInterval(G.poll);
    G.poll = setInterval(() => { if (G.enabled) { refresh(); client()?.processWelcomes().catch(() => {}); client()?.pruneExpired().then((n) => { if (n) renderAll(); }); } }, 20000);
  }

  function teardown() {
    G.enabled = false;
    G.open = null;
    G.groups = [];
    clearInterval(G.poll);
    clearInterval(G.callPoll);
    for (const id of ["groups-section", "new-group-btn", "group-view"]) { const n = $(id); if (n) n.hidden = true; }
  }

  function reset() {
    teardown();
    G.clients.clear();
    G.mailbox = null;
    G.members.clear();
    G.devices.clear();
  }

  // ---- refreshing ----------------------------------------------------------------------------------

  let refreshing = null;
  async function refresh() {
    if (!G.enabled) return;
    if (refreshing) return refreshing;
    refreshing = (async () => {
      const mailbox = me();
      try {
        const body = await api("/v1/groups?state=active,invited,keying");
        if (mailbox !== me()) return;
        G.groups = (body.groups || []).filter((g) => g && g.id);
      } catch (e) {
        if (e.status === 404) { G.groups = []; }
        return;
      }
      renderAll();
      for (const g of G.groups) {
        if (g.deleted_at) continue;
        const state = g.my && g.my.state;
        if (state === "keying" && g.activation_state === "provisioning" && g.created_by === mailbox && g.my.role === "owner") {
          await client().activate(g.id).catch(() => {});
        } else if (state === "active") {
          syncGroup(g.id, { pending: true });
        }
      }
    })().finally(() => { refreshing = null; });
    return refreshing;
  }

  async function syncGroup(gid, { pending = false } = {}) {
    const c = client();
    if (!c) return;
    if (G.syncing.has(gid)) return;
    G.syncing.add(gid);
    try {
      if (!c.inGroup(gid)) return;
      await c.sync(gid).catch(() => {});
      if (pending) await c.processPending(gid).catch(() => {});
      if (gid === G.open) markRead(gid);
    } finally {
      G.syncing.delete(gid);
      renderList();
      if (gid === G.open) renderView();
    }
  }

  function markRead(gid) {
    const g = groupById(gid);
    if (!g || !g.my || document.hidden) return;
    const last = Math.max(0, ...client().history(gid).map((x) => (Number.isInteger(x.seq) ? x.seq : 0)), g.last_seq || 0);
    if (last > (g.my.read_cursor || 0)) {
      g.my.read_cursor = last;
      g.my.unread = 0;
      api(groupPath(gid, "/read"), { method: "POST", body: { seq: last } }).catch(() => {});
    }
  }

  const groupById = (gid) => G.groups.find((g) => g.id === gid) || null;

  async function loadMembers(gid) {
    try {
      const body = await api(groupPath(gid, "/members"));
      const list = body.members || [];
      for (const m of list) if (m.handle) G.handles.set(m.address, m.handle);
      G.members.set(gid, list);
      return list;
    } catch (_) {
      return G.members.get(gid) || [];
    }
  }

  async function loadDevices(mailbox) {
    try {
      const body = await api("/v1/mls/devices?mailbox=" + encodeURIComponent(mailbox));
      G.devices.set(mailbox, body.devices || []);
    } catch (_) { /* not visible */ }
    return G.devices.get(mailbox) || [];
  }

  // ---- events --------------------------------------------------------------------------------------

  const syncTimers = new Map();
  function soon(key, fn, ms = 150) {
    clearTimeout(syncTimers.get(key));
    syncTimers.set(key, setTimeout(fn, ms));
  }

  function onEvent(type, frame) {
    if (!G.enabled && type !== "message.expired") return;
    const data = (frame && frame.data && typeof frame.data === "object") ? frame.data : (frame || {});
    const mailbox = frame && frame.mailbox;
    if (mailbox && mailbox !== me()) return;
    const gid = data.group_id;
    switch (type) {
      case "group.message":
      case "group.handshake":
        if (gid) soon("sync:" + gid, () => syncGroup(gid));
        if (gid && !groupById(gid)) soon("refresh", refresh);
        if (gid && gid === G.open) soon("receipts:" + gid, () => loadReceipts(gid), 1500); // Phase 1e
        break;
      case "group.crypto_pending":
        if (gid) soon("pending:" + gid, () => syncGroup(gid, { pending: true }), 50);
        break;
      case "group.stream": // Phase 6
        onStreamEvent(data);
        break;
      case "group.member":
        if (gid) G.members.delete(gid);
        window.PigeonpostLive?.onGroupMember(data); // the streamer re-reads the stream's key directory
        soon("refresh", refresh);
        if (gid && gid === G.open) soon("info:" + gid, () => { loadMembers(gid).then(renderInfo); });
        break;
      case "group.settings":
      case "group.deleted":
        soon("refresh", refresh);
        break;
      case "group.read": {
        const g = groupById(gid);
        if (g && g.my && data.seq > (g.my.read_cursor || 0)) { g.my.read_cursor = data.seq; g.my.unread = 0; renderList(); }
        if (gid && gid === G.open) soon("receipts:" + gid, () => loadReceipts(gid), 800); // Phase 1e
        break;
      }
      case "group.device":
        if (data.mailbox) G.devices.delete(data.mailbox);
        if (G.open && !$("group-info-sheet")?.hidden) renderInfo();
        break;
      case "mls.welcome":
        client()?.processWelcomes().then((joined) => { if (joined.length) refresh(); }).catch(() => {});
        break;
      case "mls.key_packages_low":
        client()?.topUpKeyPackages().catch(() => {});
        break;
      case "message.expired":
        if (gid && client()) client().dropSeqs(gid, data.seqs || []).then(renderAll).catch(() => {});
        break;
      default:
    }
  }

  // ---- DOM scaffolding -----------------------------------------------------------------------------

  function ensureDom() {
    if (!$("new-group-btn")) {
      const btn = el("button", { id: "new-group-btn", class: "new-group-btn", type: "button", title: "New group", "aria-label": "New group", onclick: openCreate },
        svg(ICON.group), el("span", { class: "new-group-label", text: "New group" }));
      const anchor = $("new-btn");
      if (anchor && anchor.parentNode) anchor.after(btn);
    }
    if (!$("groups-section")) {
      const section = el("section", { id: "groups-section", class: "groups-section", "aria-label": "Groups" },
        el("ul", { class: "threads groups-list", id: "groups-list", role: "list" }));
      const threads = $("threads");
      if (threads && threads.parentNode) threads.before(section);
    }
    if (!$("group-view")) buildView();
    if (!$("group-create-sheet")) buildCreateSheet();
    if (!$("group-info-sheet")) buildInfoSheet();
    if (!$("group-join-sheet")) buildJoinSheet();
    if (!$("group-confirm-sheet")) buildConfirmSheet();
  }

  // Our own small sheet manager: app.js's stack is private to it.
  let sheetsOpen = [];
  function showSheet(id, focus) {
    const s = $(id);
    s.hidden = false;
    sheetsOpen = sheetsOpen.filter((x) => x !== id).concat(id);
    const app = $("app");
    if (app) app.inert = true;
    s.style.zIndex = String(60 + sheetsOpen.length);
    const target = focus ? $(focus) : s.querySelector("input, select, textarea, button");
    if (target) target.focus({ preventScroll: true });
  }
  function hideSheet(id) {
    const s = $(id);
    if (!s) return;
    s.hidden = true;
    sheetsOpen = sheetsOpen.filter((x) => x !== id);
    for (const x of sheetsOpen) $(x).inert = x !== sheetsOpen.at(-1);
    if (!sheetsOpen.length) {
      const app = $("app");
      if (app && !document.querySelector(".sheet-wrap:not([hidden]):not(.group-sheet)")) app.inert = false;
    }
  }
  function sheet(id, title, body, foot, wide) {
    const titleId = id + "-title";
    const wrap = el("div", { class: "sheet-wrap group-sheet", id, hidden: true },
      el("div", { class: "sheet" + (wide ? " sheet-wide" : ""), role: "dialog", "aria-modal": "true", "aria-labelledby": titleId },
        el("header", { class: "sheet-head" },
          el("h2", { id: titleId, text: title }),
          el("button", { class: "icon-btn", type: "button", "aria-label": "Close", onclick: () => hideSheet(id) }, svg(ICON.close))),
        el("div", { class: "sheet-body" }, body),
        foot ? el("footer", { class: "sheet-foot" }, foot) : null));
    wrap.addEventListener("click", (e) => { if (e.target === wrap) hideSheet(id); });
    wrap.addEventListener("keydown", (e) => { if (e.key === "Escape") { e.stopPropagation(); hideSheet(id); } });
    document.body.append(wrap);
    return wrap;
  }

  // ---- the list ----------------------------------------------------------------------------------

  function initials(name) {
    const parts = String(name || "").trim().split(/\s+/).filter(Boolean);
    const s = (parts[0] ? parts[0][0] : "") + (parts[1] ? parts[1][0] : (parts[0] && parts[0][1]) || "");
    return (s || "··").toUpperCase();
  }
  function tone(text) {
    let h = 0;
    for (const ch of String(text)) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
    return (h % 5) + 1;
  }
  function groupAvatar(g, cls = "avatar") {
    return el("span", { class: cls + " group-avatar", "aria-hidden": "true", dataset: { tone: String(tone(g.id)) }, text: initials(g.name) });
  }

  function nameOf(address) {
    if (!address) return "Someone";
    if (address === me()) return "You";
    const handle = G.handles.get(address);
    const h = host();
    return h ? h.displayName(handle || address) : (handle || address);
  }

  function previewOf(gid) {
    const c = client();
    if (!c) return "";
    const list = c.history(gid);
    for (let i = list.length - 1; i >= 0; i--) {
      const x = list[i];
      if (x.kind !== "message" || x.replaced_by) continue;
      if (x.deleted) return "Message deleted";
      if (x.undecryptable) return "Encrypted message";
      const who = x.sender === me() ? "You" : nameOf(x.sender).split(/[ /]/).filter(Boolean)[0] || "Someone";
      const what = x.text || (x.files && x.files.length ? "📎 " + x.files[0].name : "");
      return who + ": " + what;
    }
    return "";
  }

  function visibleInList() {
    const h = host();
    return G.enabled && h && h.leftView() === "inbox";
  }

  function renderList() {
    const section = $("groups-section");
    if (!section) return;
    const btn = $("new-group-btn");
    if (btn) btn.hidden = !G.enabled;
    section.hidden = !visibleInList();
    if (section.hidden) return;
    const list = $("groups-list");
    list.textContent = "";
    const filter = (host().filter() || "").toLowerCase();
    const invited = G.groups.filter((g) => g.my && g.my.state === "invited" && !g.deleted_at);
    if (invited.length && !filter) {
      list.append(el("li", {}, el("button", {
        type: "button", class: "thread-row invitations-row", id: "group-invitations-btn", "aria-expanded": String(G.invitesOpen),
        onclick: () => { G.invitesOpen = !G.invitesOpen; renderList(); },
      }, el("span", { class: "avatar group-avatar", "aria-hidden": "true", dataset: { tone: "2" } }, svg(ICON.group)),
        el("div", { class: "tr-text" }, el("div", { class: "tr-top" }, el("span", { class: "tr-name", text: `Invitations (${invited.length})` })),
          el("div", { class: "tr-bottom" }, el("span", { class: "tr-preview", text: G.invitesOpen ? "Choose to join or decline" : "Group invitations waiting for you" }))))));
      if (G.invitesOpen) for (const g of invited) list.append(invitationRow(g));
    }
    const rows = G.groups.filter((g) => g.my && g.my.state !== "invited" && !g.deleted_at && (!filter || String(g.name).toLowerCase().includes(filter)))
      .sort((a, b) => (b.last_at || 0) - (a.last_at || 0));
    for (const g of rows) list.append(groupRow(g));
    if (rows.length || invited.length) { const empty = $("threads-empty"); if (empty) empty.hidden = true; }
  }

  function invitationRow(g) {
    const accept = el("button", { type: "button", class: "btn-primary btn-small", text: "Join", "aria-label": `Join ${g.name}` });
    const decline = el("button", { type: "button", class: "btn-ghost btn-small", text: "Decline", "aria-label": `Decline ${g.name}` });
    const answer = async (yes) => {
      accept.disabled = decline.disabled = true;
      try {
        await api(groupPath(g.id, "/invitation"), { method: "POST", body: { accept: yes } });
        host().toast(yes ? `Joined ${g.name}. A member's device will add you in a moment.` : "Invitation declined.");
      } catch (e) {
        host().toast(explain(e));
      }
      refresh();
    };
    accept.onclick = () => answer(true);
    decline.onclick = () => answer(false);
    const from = g.my && g.my.invited_by ? "From " + nameOf(g.my.invited_by) : "";
    return el("li", { class: "invitation" },
      el("div", { class: "thread-row invitation-card" }, groupAvatar(g),
        el("div", { class: "tr-text" },
          el("div", { class: "tr-top" }, el("span", { class: "tr-name", text: g.name })),
          el("div", { class: "tr-bottom" }, el("span", { class: "tr-preview", text: [from, g.my && g.my.filtered ? "someone you don't know" : "", `${g.member_count || 1} members`].filter(Boolean).join(" · ") }))),
        el("span", { class: "invitation-actions" }, decline, accept)));
  }

  function groupRow(g) {
    const state = g.my && g.my.state;
    const muted = g.my && g.my.muted_until && g.my.muted_until > Date.now() / 1000;
    const preview = state === "keying" ? "Waiting for a member's device to add you…" : (previewOf(g.id) || (g.activation_state === "provisioning" ? "Setting up encryption…" : "No messages yet"));
    const badges = el("span", { class: "tr-badges" });
    if (g.ephemeral_secs) badges.append(el("span", { class: "group-badge-icon", title: "Disappearing messages: " + Core.timerLabel(g.ephemeral_secs) }, svg(ICON.timer)));
    if (muted) badges.append(el("span", { class: "group-badge-icon", title: "Muted" }, svg(ICON.mute)));
    const unread = g.my && g.my.unread;
    if (unread && g.id !== G.open) badges.append(el("span", { class: "dot", text: String(unread) }));
    const h = host();
    return el("li", {}, el("button", {
      type: "button", class: "thread-row group-row", "aria-current": g.id === G.open ? "true" : null, dataset: { group: g.id },
      onclick: () => openGroup(g.id),
    }, groupAvatar(g),
    el("div", { class: "tr-text" },
      el("div", { class: "tr-top" }, el("span", { class: "tr-name", text: g.name }), el("span", { class: "tr-time", text: g.last_at && h ? h.listTime(g.last_at) : "" })),
      el("div", { class: "tr-bottom" }, el("span", { class: "tr-preview", text: preview }), badges))));
  }

  // ---- the conversation view ---------------------------------------------------------------------

  function buildView() {
    const pane = $("pane-thread");
    if (!pane) return;
    const view = el("div", { id: "group-view", class: "group-view", hidden: true },
      el("header", { class: "thread-head group-head" },
        el("button", { class: "icon-btn back group-back", type: "button", "aria-label": "Back to conversations", onclick: () => close() }, svg(ICON.back)),
        el("span", { class: "avatar group-avatar", id: "group-head-avatar", "aria-hidden": "true" }),
        el("button", { class: "thread-who", id: "group-head-who", type: "button", title: "Group info", "aria-haspopup": "dialog", onclick: () => openInfo() },
          el("span", { class: "thread-name", id: "group-head-name" }),
          el("span", { class: "thread-sub", id: "group-head-sub" })),
        el("button", { class: "btn-ghost call-join-btn", id: "group-call-join", type: "button", hidden: true, onclick: () => joinCall() }, "Call in progress — Join"),
        el("button", { class: "icon-btn", id: "group-live-btn", type: "button", title: "Go live", "aria-label": "Go live", hidden: true, onclick: () => openGoLive() }, svg(ICON.live)),
        el("button", { class: "icon-btn", id: "group-call-audio", type: "button", title: "Group voice call", "aria-label": "Group voice call", onclick: () => startCall("audio") }, svg(ICON.phone)),
        el("button", { class: "icon-btn", id: "group-call-video", type: "button", title: "Group video call", "aria-label": "Group video call", onclick: () => startCall("video") }, svg(ICON.video)),
        el("button", { class: "icon-btn", id: "group-info-btn", type: "button", title: "Group info", "aria-label": "Group info", "aria-haspopup": "dialog", onclick: () => openInfo() }, svg(ICON.info))),
      el("button", { class: "group-live-banner", id: "group-live-banner", type: "button", hidden: true, onclick: () => watchStream() },
        el("span", { class: "live-badge", text: "LIVE" }), el("span", { class: "group-live-text", id: "group-live-text" }), el("span", { class: "group-live-watch", id: "group-live-watch", text: "Watch" })),
      el("div", { class: "thread-scroll", id: "group-scroll" },
        el("p", { class: "group-e2ee-note", id: "group-e2ee-note" }, svg(ICON.lock), el("span", { text: "Messages in this group are end-to-end encrypted. The postbox cannot read them." })),
        el("p", { class: "group-empty", id: "group-empty", hidden: true }),
        el("ol", { class: "messages group-messages", id: "group-messages", role: "list" })),
      el("p", { class: "group-bar", id: "group-announce", role: "status", hidden: true, text: "Only admins can send messages" }),
      el("p", { class: "group-bar", id: "group-status", role: "status", hidden: true }),
      el("form", { class: "composer", id: "group-composer", onsubmit: (e) => { e.preventDefault(); sendFromComposer(); } },
        el("ul", { class: "pending-files", id: "group-files", hidden: true }),
        el("div", { class: "compose-context", id: "group-compose-context", hidden: true },
          el("span", { class: "compose-context-icon", id: "group-compose-context-icon", "aria-hidden": "true" }),
          el("span", { class: "compose-context-text" },
            el("span", { class: "compose-context-title", id: "group-compose-context-title" }),
            el("span", { class: "compose-context-line", id: "group-compose-context-line" })),
          el("button", { class: "icon-btn compose-context-close", id: "group-compose-context-close", type: "button", "aria-label": "Cancel", onclick: () => { cancelCompose(); $("group-compose").focus(); } }, svg(ICON.close))),
        el("input", { type: "file", id: "group-file-input", multiple: true, hidden: true, "aria-hidden": "true", tabindex: "-1", onchange: (e) => { stageFiles(e.target.files); e.target.value = ""; } }),
        el("button", { class: "attach", type: "button", "aria-label": "Attach a file", onclick: () => $("group-file-input").click() }, svg(ICON.attach)),
        el("textarea", { id: "group-compose", rows: "1", placeholder: "Write to the group", "aria-label": "Message to the group", onkeydown: (e) => {
          if (e.key === "Escape" && G.compose) { e.preventDefault(); e.stopPropagation(); cancelCompose(); return; }
          if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); sendFromComposer(); }
        } }),
        el("button", { class: "send", id: "group-send", type: "submit", "aria-label": "Send" }, svg(ICON.send))));
    pane.append(view);
  }

  function openGroup(gid) {
    const h = host();
    h.closeConversation();
    G.open = gid;
    const view = $("group-view");
    if (view) view.hidden = false;
    $("pane-thread").dataset.open = "true";
    if (h.onPhone()) history.pushState({ group: gid }, "");
    renderAll();
    loadMembers(gid).then(() => renderView());
    syncGroup(gid, { pending: true });
    if (G.compose && G.compose.gid !== gid) cancelCompose();
    loadReceipts(gid);
    clearInterval(G.receiptsPoll);
    G.receiptsPoll = setInterval(() => { if (G.open) loadReceipts(G.open); }, 10000);
    pollCall();
    pollStream(gid);
    clearInterval(G.callPoll);
    G.callPoll = setInterval(() => { pollCall(); pollStream(G.open); }, 15000);
    if (!h.onPhone()) setTimeout(() => { const c = $("group-compose"); if (c && !c.closest("[hidden]")) c.focus({ preventScroll: true }); }, 0);
  }

  function close() {
    if (!G.open) return;
    G.open = null;
    clearInterval(G.callPoll);
    clearInterval(G.receiptsPoll);
    if (G.compose) cancelCompose();
    G.callState = null;
    const view = $("group-view");
    if (view) view.hidden = true;
    hideSheet("group-info-sheet");
    const h = host();
    if (h) h.rerender();
  }

  window.addEventListener("popstate", () => { if (G.open && host() && host().onPhone()) close(); });

  function canPost(g) {
    const role = g.my && g.my.role;
    if (!g.my || g.my.state !== "active") return false;
    if (role === "restricted") return false;
    if (g.post_policy === "admins") return role === "owner" || role === "admin";
    return true;
  }
  const isAdmin = (g) => g && g.my && (g.my.role === "owner" || g.my.role === "admin");

  function renderView() {
    const view = $("group-view");
    if (!view) return;
    const g = G.open && groupById(G.open);
    view.hidden = !g;
    if (!g) return;
    const head = $("group-head-avatar");
    head.textContent = initials(g.name);
    head.dataset.tone = String(tone(g.id));
    $("group-head-name").textContent = g.name;
    const sub = [`${g.member_count || 1} ${g.member_count === 1 ? "member" : "members"}`];
    if (g.ephemeral_secs) sub.push("⏱ " + Core.timerLabel(g.ephemeral_secs));
    $("group-head-sub").textContent = sub.join(" · ");
    const active = g.my && g.my.state === "active";
    for (const id of ["group-call-audio", "group-call-video"]) $(id).hidden = !active || !window.PigeonpostCalls;
    $("group-call-join").hidden = !(active && G.callState && G.callState.active);
    renderLive(g, active);

    const status = $("group-status");
    const c = client();
    let statusText = "";
    if (g.my && g.my.state === "keying") statusText = "You have joined. Waiting for a member's device to add you to the encryption — this happens automatically when one is online.";
    else if (g.activation_state === "provisioning") statusText = "Setting up encryption…";
    else if (active && c && !c.inGroup(g.id)) statusText = "This browser is being added to the group's encryption. New messages will appear once it is.";
    if (!G.persistent) statusText = (statusText ? statusText + " " : "") + "This browser cannot keep group keys between visits.";
    status.textContent = statusText;
    status.hidden = !statusText;
    const posting = canPost(g) && c && c.inGroup(g.id);
    $("group-announce").hidden = !(active && g.post_policy === "admins" && !canPost(g));
    if (active && g.my.role === "restricted") { $("group-announce").hidden = false; $("group-announce").textContent = "You can read this group but not send messages."; }
    else $("group-announce").textContent = "Only admins can send messages";
    $("group-composer").hidden = !posting;

    renderMessages(g);
  }

  function noticeText(x) {
    const m = x.metadata || {};
    const actor = x.actor ? nameOf(x.actor) : "Someone";
    const who = m.address ? nameOf(m.address) : "someone";
    switch (x.notice) {
      case "group_created": return `${actor} created the group`;
      case "member_invited": return `${actor} invited ${who}`;
      case "member_left": return `${who} left`;
      case "member_removed": return `${actor} removed ${who}`;
      case "member_banned": return `${actor} removed and banned ${who}`;
      case "role_changed": return m.role === "admin" ? `${actor} made ${who} an admin` : m.role === "restricted" ? `${actor} restricted ${who}` : `${who} is now a member`;
      case "settings_changed": {
        const ch = m.changed || [];
        if (ch.includes("post_policy")) return m.post_policy === "admins" ? `${actor} set posting to admins only` : `${actor} let everyone post`;
        if (ch.includes("name")) return `${actor} renamed the group to “${m.name || ""}”`;
        if (ch.includes("join_policy")) return m.join_policy === "public" ? `${actor} made the group public` : `${actor} made the group invite only`;
        return `${actor} changed the group settings`;
      }
      case "timer_changed": return m.ephemeral_secs ? `${actor} set messages to disappear after ${Core.timerLabel(m.ephemeral_secs)}` : `${actor} turned off disappearing messages`;
      case "ownership_transferred": return `${actor} made ${who} the owner`;
      case "call_started": return `${actor} started a ${m.kind === "video" ? "video" : "voice"} call`;
      case "call_ended": return m.duration ? `Call ended · ${Math.round(m.duration / 60)} min` : "Call ended";
      case "stream": { // Phase 6: rendered here from the notice's metadata
        const mins = Math.max(1, Math.round((m.duration || 0) / 60));
        const viewers = Number(m.viewers) || 0;
        return `${actor} streamed for ${mins} min · ${viewers} ${viewers === 1 ? "viewer" : "viewers"}`;
      }
      case "link_reset": return `${actor} reset the invite link`;
      case "you_joined": return "You joined this group. Earlier messages are not available on this device.";
      case "device_removed": return `${who}'s device was removed`;
      case "member_joined": return null; // shown from the verified commit
      case "device_added": return null;
      default: return null;
    }
  }

  // Lines derived from verified commits: who really joined, and which existing members added a new
  // device. The latter is the visible defence against a postbox minting devices (P-2 §9, §12).
  function commitLines(list, x) {
    const out = [];
    const knownBefore = new Set([me()]);
    for (const y of list) {
      if (y === x) break;
      if (y.kind === "commit") { knownBefore.add(y.committer.mailbox); for (const d of y.added) knownBefore.add(d.mailbox); }
      if (y.kind === "message" && y.sender) knownBefore.add(y.sender);
    }
    const byMailbox = new Map();
    for (const d of x.added) { if (!byMailbox.has(d.mailbox)) byMailbox.set(d.mailbox, []); byMailbox.get(d.mailbox).push(d); }
    for (const [mailbox, devices] of byMailbox) {
      if (knownBefore.has(mailbox) || mailbox === x.committer.mailbox) {
        for (const d of devices) {
          if (mailbox === me() && d.device_id === client().deviceId()) continue;
          out.push({ text: `${nameOf(mailbox)} added a new device`, fingerprint: d.fingerprint, security: true });
        }
      } else {
        out.push({ text: `${nameOf(mailbox)} joined`, verified: true });
      }
    }
    return out;
  }

  function fingerprintDigits(hexFp) {
    try { return G.gc.fingerprintDigits(new Uint8Array(String(hexFp).match(/../g).map((b) => parseInt(b, 16)))); } catch (_) { return String(hexFp); }
  }

  function renderMessages(g) {
    const ol = $("group-messages");
    const c = client();
    if (!ol || !c) return;
    const now = Date.now() / 1000;
    const list = c.history(g.id).filter((x) => !(x.expires_at && x.expires_at <= now));
    ol.textContent = "";
    let lastSender = null;
    let lastDay = null;
    const h = host();
    const messages = list.filter((x) => x.kind === "message");
    const empty = $("group-empty");
    empty.hidden = messages.length > 0;
    empty.textContent = g.ephemeral_secs ? `Messages disappear after ${Core.timerLabel(g.ephemeral_secs)}.` : "No messages yet. Say hello.";
    for (const x of list) {
      const at = x.created_at || 0;
      const day = at ? new Date(at * 1000).toDateString() : null;
      if (day && day !== lastDay) {
        lastDay = day;
        ol.append(el("li", { class: "system-line day-line" }, el("span", { text: new Date(at * 1000).toLocaleDateString([], { weekday: "long", day: "numeric", month: "long" }) })));
        lastSender = null;
      }
      if (x.kind === "notice") {
        const t = noticeText(x);
        if (t) { ol.append(el("li", { class: "system-line group-notice" }, el("span", { text: t }))); lastSender = null; }
        continue;
      }
      if (x.kind === "commit") {
        for (const line of commitLines(list, x)) {
          const li = el("li", { class: "system-line group-notice" + (line.security ? " security" : "") }, el("span", { text: line.text }));
          if (line.fingerprint) li.append(el("span", { class: "fingerprint", text: " · " + fingerprintDigits(line.fingerprint) }));
          ol.append(li);
        }
        lastSender = null;
        continue;
      }
      if (x.kind === "warning") {
        const text = {
          membership_mismatch: "Security warning: a membership change did not match the postbox's record. Compare safety numbers in group info.",
          commit_rejected: "Security warning: this device refused an invalid group change.",
          sender_mismatch: "Security warning: a message's sender did not match its device.",
          resynced: "This device lost the group's keys and is being added again. Earlier messages stay unreadable here.",
        }[x.warning] || "Security warning.";
        ol.append(el("li", { class: "system-line group-warning", role: "alert" }, el("span", { text })));
        lastSender = null;
        continue;
      }
      if (x.kind !== "message" || x.replaced_by) continue;
      const mine = x.sender === me();
      const li = el("li", { class: (mine ? "mine" : "") + (x.expires_at ? " ephemeral" : ""), dataset: { seq: String(x.seq == null ? "" : x.seq) } });
      const bubble = el("div", { class: "bubble" });
      if (!mine && x.sender !== lastSender) bubble.append(el("div", { class: "group-sender", text: nameOf(x.sender) }));
      lastSender = x.sender;
      // Phase 1e: the quoted parent of a reply, which goes to it when clicked.
      if (x.reply_to != null && !x.deleted) bubble.append(groupQuote(list, x.reply_to));
      if (x.deleted) bubble.append(el("div", { class: "text muted-text", text: "This message was deleted" }));
      else if (x.undecryptable) bubble.append(el("div", { class: "text muted-text", text: "Encrypted message — not readable on this device" }));
      else {
        if (x.text) bubble.append(el("div", { class: "text", text: x.text }));
        if (x.files && x.files.length) {
          const files = el("div", { class: "files" });
          for (const f of x.files) files.append(el("button", { type: "button", class: "file", onclick: () => downloadFile(g.id, f) }, svg(ICON.attach), el("span", { text: f.name })));
          bubble.append(files);
        }
      }
      const meta = el("div", { class: "meta" });
      if (x.expires_at) meta.append(el("span", { class: "timer-icon", title: "Disappears " + new Date(x.expires_at * 1000).toLocaleString() }, svg(ICON.timer)));
      if (!x.deleted && x.seq != null && (mine || isAdmin(g)) && !x.pending) {
        meta.append(el("button", { type: "button", class: "linkish msg-delete", text: "Delete", "aria-label": "Delete this message for everyone", onclick: () => deleteMessage(g.id, x.seq) }));
      }
      meta.append(el("span", { class: "meta-spacer" }));
      if (x.edit_of) meta.append(el("span", { class: "edited", text: "edited" }));
      meta.append(el("span", { class: "time", text: x.pending ? "Sending…" : (at && h ? h.clockTime(at) : "") }));
      const UI = window.PigeonpostMessageActions;
      if (mine && UI && !x.deleted) {
        const state = groupTick(g.id, x);
        const ticks = UI.tickNode(state, null);
        if (ticks) {
          if (x.seq != null) {
            ticks.classList.add("ticks-button");
            ticks.tabIndex = 0;
            ticks.addEventListener("click", () => groupMessageInfo(g, x));
            ticks.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); groupMessageInfo(g, x); } });
          }
          meta.append(ticks);
        }
      }
      bubble.append(meta);
      bubble.addEventListener("contextmenu", (e) => groupMenu(e, g, x, mine));
      // Phase 1e: the action row, under the bubble and outside it.
      const stack = el("div", { class: "msg-stack" }, bubble);
      if (UI && !x.deleted && !x.undecryptable) {
        const actions = el("div", { class: "msg-actions" });
        const copyText = () => x.text || (x.files || []).map((f) => f.name).join("\n");
        if (mine) {
          if (groupEditable(g, x)) actions.append(UI.actionButton("edit", "Edit message", () => startGroupCompose("edit", g.id, x)));
          actions.append(UI.copyButton(copyText));
        } else {
          actions.append(UI.copyButton(copyText));
          if (x.seq != null && canPost(g)) actions.append(UI.actionButton("reply", "Reply", () => startGroupCompose("reply", g.id, x)));
        }
        stack.append(actions);
      }
      li.append(stack);
      ol.append(li);
    }
    const scroll = $("group-scroll");
    if (scroll) scroll.scrollTop = scroll.scrollHeight;
  }

  // ---- Phase 1e: reply, edit, ticks and message info (docs/message-actions.md §5) ------------------

  const EDIT_WINDOW = 24 * 3600;
  function groupEditable(g, x) {
    if (!x || x.sender !== me() || x.deleted || x.pending || x.seq == null || !canPost(g)) return false;
    if (!x.text) return false;
    const at = x.created_at || 0;
    return at > 0 && Date.now() / 1000 - at <= EDIT_WINDOW && !(x.expires_at && x.expires_at <= Date.now() / 1000);
  }

  function groupQuote(list, seq) {
    const parent = list.find((y) => y.kind === "message" && String(y.seq) === String(seq));
    const who = parent ? (parent.sender === me() ? "You" : nameOf(parent.sender)) : "";
    const UI = window.PigeonpostMessageActions;
    const line = !parent || parent.deleted ? "Original message unavailable"
      : (UI ? UI.quoteExcerpt(parent.text || "", (parent.files || []).map((f) => ({ filename: f.name }))) : parent.text || "");
    return el("button", { type: "button", class: "reply-quote", "aria-label": `Replying to ${who || "a message"}: ${line}. Show the original.`,
      onclick: () => jumpToSeq(seq) },
      el("span", { class: "reply-who", text: who }), el("span", { class: "reply-line", text: line }));
  }

  function jumpToSeq(seq) {
    const row = $("group-messages")?.querySelector(`li[data-seq="${CSS.escape(String(seq))}"]`);
    if (!row) { host().toast("The original message is no longer here."); return; }
    row.scrollIntoView({ block: "center" });
    row.classList.remove("flash");
    void row.offsetWidth;
    row.classList.add("flash");
    setTimeout(() => row.classList.remove("flash"), 1600);
  }

  // Delivered when every other member's client has listed it; seen (green) when every member whose
  // seen state is visible has read past it — and at least one is visible.
  function groupTick(gid, x) {
    if (x.pending) return "pending";
    if (x.seq == null) return null;
    const r = G.receipts.get(gid);
    const members = (r && r.members) || [];
    if (!r || !members.length) return "sent";
    const visible = members.filter((m) => m.seen_seq != null);
    if (visible.length && visible.every((m) => m.seen_seq >= x.seq)) return "seen";
    if (members.every((m) => (m.delivered_seq || 0) >= x.seq)) return "delivered";
    return "sent";
  }

  async function loadReceipts(gid) {
    try {
      const body = await api(groupPath(gid, "/receipts"));
      const before = JSON.stringify(G.receipts.get(gid) || null);
      G.receipts.set(gid, { members: Array.isArray(body.members) ? body.members : [], read_receipts: body.read_receipts !== false });
      if (gid === G.open && before !== JSON.stringify(G.receipts.get(gid))) renderView();
    } catch (_) {
      // An older postbox has no receipts for groups: no ticks beyond "sent".
    }
  }

  function setGroupComposeBar() {
    const bar = $("group-compose-context");
    if (!bar) return;
    const c = G.compose;
    bar.hidden = !c;
    if (!c) return;
    const UI = window.PigeonpostMessageActions;
    $("group-compose-context-icon").innerHTML = UI ? (c.kind === "edit" ? UI.ICONS.edit : UI.ICONS.reply) : "";
    $("group-compose-context-title").textContent = c.kind === "edit" ? "Editing message" : "Replying to " + (c.x.sender === me() ? "yourself" : nameOf(c.x.sender));
    $("group-compose-context-line").textContent = UI ? UI.quoteExcerpt(c.x.text || "", (c.x.files || []).map((f) => ({ filename: f.name }))) : c.x.text || "";
    $("group-compose-context-close").setAttribute("aria-label", c.kind === "edit" ? "Cancel editing" : "Cancel reply");
  }
  function startGroupCompose(kind, gid, x) {
    const box = $("group-compose");
    const draft = G.compose && G.compose.kind === "edit" ? G.compose.draft : box.value;
    G.compose = { kind, gid, x, draft: kind === "edit" ? draft : null };
    if (kind === "edit") box.value = x.text || "";
    setGroupComposeBar();
    box.focus();
  }
  function cancelCompose() {
    const c = G.compose;
    G.compose = null;
    setGroupComposeBar();
    if (c && c.kind === "edit") { const box = $("group-compose"); if (box) box.value = c.draft || ""; }
  }

  function groupMessageInfo(g, x) {
    const r = G.receipts.get(g.id);
    const members = (r && r.members) || [];
    const name = (m) => (m.handle ? host().displayName(m.handle) : nameOf(m.address));
    const when = x.created_at ? new Date(x.created_at * 1000).toLocaleString() : "—";
    const seen = members.filter((m) => m.seen_seq != null && m.seen_seq >= x.seq);
    const delivered = members.filter((m) => (m.delivered_seq || 0) >= x.seq && !seen.includes(m));
    const waiting = members.filter((m) => !seen.includes(m) && !delivered.includes(m));
    const list = (title, people) => el("div", { class: "info-group" },
      el("h3", { class: "info-head", text: `${title} (${people.length})` }),
      people.length ? el("ul", { class: "info-names" }, ...people.map((m) => el("li", { text: name(m) }))) : el("p", { class: "field-note", text: "Nobody" }));
    const body = el("div", { class: "msg-info" },
      el("p", { class: "info-row" }, el("span", { class: "info-key", text: "Sent" }), el("span", { class: "info-value", text: when })),
      list("Seen", seen), list("Delivered", delivered), list("Not delivered yet", waiting));
    if (r && r.read_receipts === false) body.append(el("p", { class: "field-note", text: "You turned off read receipts, so you don't see who has read messages." }));
    else if (members.some((m) => m.seen_seq == null)) body.append(el("p", { class: "field-note", text: "Some members don't share when they have read messages." }));
    document.getElementById("group-msg-info-sheet")?.remove();
    sheet("group-msg-info-sheet", "Message info", body, el("button", { type: "button", class: "btn-ghost", text: "Done", onclick: () => hideSheet("group-msg-info-sheet") }));
    showSheet("group-msg-info-sheet");
  }

  function groupMenu(event, g, x, mine) {
    if (window.getSelection && String(window.getSelection()).trim()) return;
    if (x.deleted || x.undecryptable) return;
    const menu = document.getElementById("msg-menu");
    if (!menu) return;
    event.preventDefault();
    menu.textContent = "";
    const item = (label, fn) => menu.append(el("button", { type: "button", role: "menuitem", text: label, onclick: () => { menu.hidden = true; fn(); } }));
    if (x.seq != null && canPost(g)) item("Reply", () => startGroupCompose("reply", g.id, x));
    item("Copy", () => navigator.clipboard?.writeText(x.text || "").catch(() => {}));
    if (groupEditable(g, x)) item("Edit", () => startGroupCompose("edit", g.id, x));
    if (mine && x.seq != null) item("Info", () => groupMessageInfo(g, x));
    if (x.seq != null && (mine || isAdmin(g))) item("Delete", () => deleteMessage(g.id, x.seq));
    menu.hidden = false;
    menu.style.left = Math.max(8, Math.min(event.clientX, window.innerWidth - menu.offsetWidth - 8)) + "px";
    menu.style.top = Math.max(8, Math.min(event.clientY, window.innerHeight - menu.offsetHeight - 8)) + "px";
    menu.querySelector("button")?.focus();
  }

  // ---- sending -------------------------------------------------------------------------------------

  let staged = [];
  function stageFiles(fileList) {
    for (const f of fileList || []) {
      if (f.size > MAX_FILE) { host().toast(`${f.name} is larger than 25 MB.`); continue; }
      staged.push(f);
    }
    renderStaged();
  }
  function renderStaged() {
    const ul = $("group-files");
    ul.textContent = "";
    ul.hidden = !staged.length;
    staged.forEach((f, i) => ul.append(el("li", {}, el("span", { text: f.name }), el("button", { type: "button", class: "icon-btn", "aria-label": "Remove " + f.name, onclick: () => { staged.splice(i, 1); renderStaged(); } }, svg(ICON.close)))));
  }

  async function sendFromComposer() {
    const gid = G.open;
    const box = $("group-compose");
    const text = box.value.trim();
    if (!gid || (!text && !staged.length)) return;
    // Phase 1e: an edit carries the new text and names the old seq; a reply names its parent.
    const mode = G.compose && G.compose.gid === gid ? G.compose : null;
    if (mode && mode.kind === "edit" && text === (mode.x.text || "").trim()) { cancelCompose(); return; }
    const extra = mode ? (mode.kind === "edit"
      ? { edit_of: mode.x.seq, ...(mode.x.reply_to != null ? { reply_to: mode.x.reply_to } : {}) }
      : { reply_to: mode.x.seq }) : {};
    if (mode) { const draft = mode.kind === "edit" ? mode.draft : ""; G.compose = null; setGroupComposeBar(); if (mode.kind === "edit") box.value = draft || ""; }
    const files = [];
    for (const f of staged) files.push({ name: f.name, mime: f.type || "application/octet-stream", bytes: new Uint8Array(await f.arrayBuffer()) });
    if (!mode || mode.kind !== "edit") box.value = "";
    staged = [];
    renderStaged();
    $("group-send").disabled = true;
    try {
      await client().send(gid, { text, files, ...extra });
      loadReceipts(gid);
    } catch (e) {
      if (mode) { G.compose = mode; setGroupComposeBar(); }
      box.value = text;
      host().toast("Not sent: " + explain(e));
    } finally {
      $("group-send").disabled = false;
      renderView();
      renderList();
    }
  }

  async function downloadFile(gid, f) {
    try {
      const bytes = await client().openFile(gid, f);
      const url = URL.createObjectURL(new Blob([bytes], { type: "application/octet-stream" }));
      const a = el("a", { href: url, download: f.name });
      document.body.append(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 30000);
    } catch (e) {
      host().toast("Could not open the file: " + explain(e));
    }
  }

  async function deleteMessage(gid, seq) {
    if (!(await confirmAction("Delete this message for everyone?", "It is removed from the postbox. Copies people already saw on their screens stay where they are.", "Delete"))) return;
    try {
      await api(groupPath(gid, "/messages/" + seq), { method: "DELETE" });
      await syncGroup(gid);
    } catch (e) { host().toast(explain(e)); }
  }

  // ---- calls ---------------------------------------------------------------------------------------

  async function pollCall() {
    const gid = G.open;
    if (!gid) return;
    try {
      const res = await fetch("/calls/v1/groups/" + encodeURIComponent(gid) + "/call?from=" + encodeURIComponent(me()), { headers: { authorization: "Bearer " + host().token(), accept: "application/json" } });
      G.callState = res.ok ? await res.json() : null;
    } catch (_) { G.callState = null; }
    if (gid === G.open) renderView();
  }
  function startCall(kind) {
    const g = groupById(G.open);
    if (!g) return;
    const calls = window.PigeonpostCalls;
    if (!calls || !calls.placeGroup) { host().toast("Group calls are not available here yet."); return; }
    calls.placeGroup(kind, { id: g.id, name: g.name });
  }
  function joinCall() {
    const g = groupById(G.open);
    const calls = window.PigeonpostCalls;
    if (!g || !calls || !calls.joinGroup) return;
    calls.joinGroup((G.callState && G.callState.kind) || "audio", { id: g.id, name: g.name });
  }

  // ---- Phase 6: live streams (live.js does the media; this is the group's side of it) ----------------

  async function pollStream(gid) {
    if (!gid || !G.streaming) return;
    try {
      const body = await api(groupPath(gid, "/streams"));
      G.streams.set(gid, (body.streams || [])[0] || null);
    } catch (_) { G.streams.set(gid, null); }
    if (gid === G.open) renderView();
  }

  function onStreamEvent(data) {
    const gid = data.group_id;
    if (!gid) return;
    window.PigeonpostLive?.onGroupStream(data);
    if (data.state === "ended") {
      G.streams.set(gid, null);
      soon("sync:" + gid, () => syncGroup(gid)); // the "streamed for" notice
    } else {
      soon("stream:" + gid, () => pollStream(gid), 100);
      const mine = window.PigeonpostLive?.current();
      if (!data.stage && data.revision === 1 && data.by !== me() && !(mine && mine.id === data.stream_id)) {
        const g = groupById(gid);
        host().toast(`${nameOf(data.by)} is live${g ? " in " + g.name : ""}: ${data.title}`);
      }
    }
    if (gid === G.open) renderView();
  }

  function canGoLive(g) {
    if (!g || !g.my || g.my.state !== "active" || g.my.role === "restricted" || g.activation_state !== "active") return false;
    return g.stream_policy === "everyone" || isAdmin(g);
  }

  function renderLive(g, active) {
    const live = G.streaming && active ? G.streams.get(g.id) : null;
    const mine = window.PigeonpostLive?.current();
    $("group-live-btn").hidden = !(G.streaming && active && canGoLive(g) && !live && window.PigeonpostLive.supported());
    const banner = $("group-live-banner");
    banner.hidden = !live;
    if (live) {
      const who = live.by === me() ? "You are" : nameOf(live.by) + " is";
      $("group-live-text").textContent = `${who} live: ${live.title}`;
      $("group-live-watch").textContent = mine && mine.id === live.id ? "Return" : live.by === me() ? "Open" : "Watch";
    }
  }

  function watchStream() {
    const g = groupById(G.open);
    const live = g && G.streams.get(g.id);
    if (!live) return;
    const L = window.PigeonpostLive;
    if (!L.supported()) { host().toast("Live streams need a newer browser or app — one that can encrypt and decrypt audio and video itself."); return; }
    const mine = L.current();
    if (mine && mine.id === live.id) { L.show(); return; }
    L.watch({ id: g.id, name: g.name }, live.id);
  }

  function buildGoLiveSheet() {
    sheet("group-live-sheet", "Go live", [
      el("label", { class: "field" }, el("span", { text: "Title" }), el("input", { type: "text", id: "gl-title", maxlength: "100", required: true, placeholder: "Sprint demo" })),
      el("fieldset", { class: "field" }, el("legend", { text: "Show" }),
        el("label", { class: "peer-toggle" }, el("span", { text: "Camera" }), el("input", { type: "checkbox", id: "gl-camera", checked: true })),
        navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia ? el("label", { class: "peer-toggle" }, el("span", { text: "Screen" }), el("input", { type: "checkbox", id: "gl-screen" })) : null),
      el("p", { class: "note", text: "Everyone in this group can watch. The stream is end-to-end encrypted and isn’t recorded; it can run for up to 4 hours." }),
      el("p", { class: "sheet-error", id: "gl-error", role: "alert", hidden: true }),
    ], [
      el("button", { class: "btn-ghost", type: "button", text: "Cancel", onclick: () => hideSheet("group-live-sheet") }),
      el("button", { class: "btn-primary", id: "gl-start", type: "button", text: "Go live", onclick: () => startLive() }),
    ]);
  }
  function openGoLive() {
    if (!$("group-live-sheet")) buildGoLiveSheet();
    $("gl-error").hidden = true;
    showSheet("group-live-sheet", "gl-title");
  }
  async function startLive() {
    const g = groupById(G.open);
    const title = $("gl-title").value.trim();
    const camera = $("gl-camera").checked, screen = Boolean($("gl-screen") && $("gl-screen").checked);
    const err = $("gl-error");
    if (!title || (!camera && !screen)) { err.textContent = !title ? "Give your stream a title." : "Choose camera, screen or both."; err.hidden = false; return; }
    hideSheet("group-live-sheet");
    const r = await window.PigeonpostLive.goLive({ id: g.id, name: g.name }, { title, camera, screen });
    if (r && r.ok) { $("gl-title").value = ""; pollStream(g.id); }
  }

  // The group's chat beside the live player: this very view, moved into the stream screen's panel
  // while watching, and put back afterwards.
  function dock(gid, container) {
    if (!G.enabled || !container) return false;
    if (G.open !== gid) openGroup(gid);
    const view = $("group-view");
    if (!view) return false;
    G.dock = { home: view.parentNode };
    view.classList.add("docked");
    container.append(view);
    return true;
  }
  function undock() {
    if (!G.dock) return;
    const view = $("group-view");
    if (view) { view.classList.remove("docked"); G.dock.home.append(view); }
    G.dock = null;
    renderView();
  }

  // ---- creating ------------------------------------------------------------------------------------

  const create = { chips: [] };
  function timerSelect(id, value) {
    const s = el("select", { id });
    for (const t of Core.TIMERS) {
      const o = el("option", { value: t == null ? "" : String(t), text: Core.timerLabel(t) });
      if ((value || null) === t) o.selected = true;
      s.append(o);
    }
    return s;
  }
  const LIMITS_NOTE = "Recipients can still take screenshots or copy text, notifications already shown may stay on screen, and copies made outside Pigeonpost are not affected.";

  function buildCreateSheet() {
    const body = [
      el("label", { class: "field", id: "gc-as-row" }, el("span", { text: "Sending as" }), el("select", { id: "gc-as" })),
      el("div", { class: "field" },
        el("span", { text: "Who" }),
        el("div", { class: "chip-row", id: "gc-chips", role: "list", "aria-label": "People to invite" }),
        el("div", { class: "chip-input" },
          el("input", { type: "text", id: "gc-who", list: "gc-contacts", autocomplete: "off", autocapitalize: "off", spellcheck: "false", placeholder: "/alp/main or /k/…", "aria-label": "Add a person by handle or address",
            onkeydown: (e) => { if (e.key === "Enter" || e.key === ",") { e.preventDefault(); addChip(); } } }),
          el("button", { type: "button", class: "btn-ghost", text: "Add", onclick: () => addChip() })),
        el("datalist", { id: "gc-contacts" })),
      el("label", { class: "field" }, el("span", { text: "Name" }), el("input", { type: "text", id: "gc-name", maxlength: "80", required: true, placeholder: "Garden club" })),
      el("label", { class: "field" }, el("span", { text: "Description (optional)" }), el("textarea", { id: "gc-desc", rows: "2", maxlength: "500" })),
      el("fieldset", { class: "field" }, el("legend", { text: "Who can join" }),
        el("div", { class: "segmented", role: "radiogroup" },
          el("label", {}, el("input", { type: "radio", name: "gc-join", value: "private", checked: true }), el("span", { text: "Private — invite only" })),
          el("label", {}, el("input", { type: "radio", name: "gc-join", value: "public" }), el("span", { text: "Public — anyone with the link" })))),
      el("fieldset", { class: "field" }, el("legend", { text: "Who can post" }),
        el("div", { class: "segmented", role: "radiogroup" },
          el("label", {}, el("input", { type: "radio", name: "gc-post", value: "everyone", checked: true }), el("span", { text: "Everyone" })),
          el("label", {}, el("input", { type: "radio", name: "gc-post", value: "admins" }), el("span", { text: "Only admins" })))),
      el("label", { class: "field", id: "gc-timer-row" }, el("span", { text: "Disappearing messages" }), timerSelect("gc-timer", null)),
      el("p", { class: "field-note", text: "Group messages are end-to-end encrypted. People you add see messages from when they join, not before." }),
      el("p", { class: "sheet-error", id: "gc-error", role: "alert", hidden: true }),
    ];
    const foot = [
      el("button", { class: "btn-ghost", type: "button", text: "Cancel", onclick: () => hideSheet("group-create-sheet") }),
      el("button", { class: "btn-primary", id: "gc-create", type: "button", text: "Create group", onclick: () => submitCreate() }),
    ];
    sheet("group-create-sheet", "New group", body, foot);
  }

  function renderChips() {
    const row = $("gc-chips");
    row.textContent = "";
    for (const who of create.chips) {
      row.append(el("span", { class: "chip", role: "listitem" }, el("span", { text: who }),
        el("button", { type: "button", class: "chip-x", "aria-label": "Remove " + who, onclick: () => { create.chips = create.chips.filter((x) => x !== who); renderChips(); } }, "×")));
    }
  }
  function addChip() {
    const input = $("gc-who");
    let v = input.value.trim().replace(/,$/, "");
    if (!v) return;
    if (!v.startsWith("/")) v = "/" + v;
    if (!/^\/[A-Za-z0-9._~\-/]{1,200}$/.test(v)) { showError("gc-error", "That does not look like a Pigeonpost address."); return; }
    if (!create.chips.includes(v) && create.chips.length < 200) create.chips.push(v);
    input.value = "";
    $("gc-error").hidden = true;
    renderChips();
  }
  function showError(id, text) { const e = $(id); e.textContent = text; e.hidden = false; }

  function openCreate() {
    if (!G.enabled) return;
    create.chips = [];
    renderChips();
    $("gc-name").value = "";
    $("gc-desc").value = "";
    $("gc-who").value = "";
    $("gc-error").hidden = true;
    const h = host();
    const ids = h.identities();
    const as = $("gc-as");
    as.textContent = "";
    for (const id of ids) as.append(el("option", { value: id.address, text: id.handle || id.address, selected: id.address === me() }));
    $("gc-as-row").hidden = ids.length < 2;
    const dl = $("gc-contacts");
    dl.textContent = "";
    for (const c of h.contacts()) if (c.peer && !c.peer.includes("*")) dl.append(el("option", { value: c.peer }));
    $("gc-timer-row").hidden = !G.disappearing;
    $("gc-timer").value = "";
    showSheet("group-create-sheet", "gc-who");
  }

  async function submitCreate() {
    addChip();
    const name = $("gc-name").value.trim();
    if (!name) { showError("gc-error", "Give the group a name."); $("gc-name").focus(); return; }
    const as = $("gc-as").value || me();
    const join = document.querySelector('input[name="gc-join"]:checked').value;
    const settings = {
      name, join_policy: join, post_policy: document.querySelector('input[name="gc-post"]:checked').value,
      visibility: "unlisted", invite: create.chips.slice(),
    };
    const desc = $("gc-desc").value.trim();
    if (desc) settings.description = desc;
    if (G.disappearing && $("gc-timer").value) settings.ephemeral_secs = Number($("gc-timer").value);
    const btn = $("gc-create");
    btn.disabled = true;
    try {
      const c = await clientFor(as);
      const res = await c.createGroup(settings);
      hideSheet("group-create-sheet");
      const refused = (res.invitations || []).filter((i) => i.status !== "invited");
      if (refused.length) host().toast(`${refused.length} ${refused.length === 1 ? "person" : "people"} could not be invited.`);
      if (as === me()) { await refresh(); openGroup(res.group.id); }
      else host().toast(`Created as ${as}. Switch to that mailbox to open it.`);
    } catch (e) {
      showError("gc-error", explain(e));
    } finally {
      btn.disabled = false;
    }
  }

  // ---- group info ----------------------------------------------------------------------------------

  function buildInfoSheet() {
    sheet("group-info-sheet", "Group info", [el("div", { id: "gi-body", class: "group-info" })], [el("button", { class: "btn-primary", type: "button", text: "Done", onclick: () => hideSheet("group-info-sheet") })], true);
  }

  async function openInfo() {
    const g = groupById(G.open);
    if (!g) return;
    renderInfo();
    showSheet("group-info-sheet");
    await loadMembers(g.id);
    renderInfo();
  }

  const ROLE_LABEL = { owner: "Owner", admin: "Admin", member: "Member", restricted: "Restricted" };

  function section(title, ...children) {
    return el("section", { class: "peer-section" }, el("h3", { text: title }), ...children);
  }

  function renderInfo() {
    const body = $("gi-body");
    const g = groupById(G.open);
    if (!body || !g) return;
    const admin = isAdmin(g);
    const owner = g.my && g.my.role === "owner";
    const members = G.members.get(g.id) || [];
    body.textContent = "";
    const status = el("p", { class: "note", id: "gi-status", role: "status" });

    // Header and name
    const head = el("div", { class: "gi-head" }, groupAvatar(g, "avatar peer-info-avatar"),
      el("div", {}, el("div", { class: "gi-name", text: g.name }), g.description ? el("p", { class: "note", text: g.description }) : null));
    body.append(head);

    if (admin) {
      const name = el("input", { type: "text", id: "gi-name", value: g.name, maxlength: "80" });
      const desc = el("textarea", { id: "gi-desc", rows: "2", maxlength: "500" });
      desc.value = g.description || "";
      body.append(section("Name and description",
        el("label", { class: "field" }, el("span", { text: "Name" }), name),
        el("label", { class: "field" }, el("span", { text: "Description" }), desc),
        el("button", { type: "button", class: "btn-ghost", text: "Save name", onclick: () => patch(g, { name: name.value.trim(), description: desc.value.trim() || null }) })));
    }

    // Members
    const ul = el("ul", { class: "gi-members", role: "list" });
    const visible = members.filter((m) => m.state === "active" || m.state === "keying" || (admin && m.state === "invited"));
    for (const m of visible) ul.append(memberRow(g, m));
    const membersSection = section(`Members (${members.filter((m) => m.state === "active").length})`, ul);
    if (admin || (g.members_can_invite && g.my && g.my.state === "active")) {
      const input = el("input", { type: "text", id: "gi-add", placeholder: "/alp/main", "aria-label": "Add people by handle or address", autocomplete: "off" });
      membersSection.append(el("div", { class: "chip-input" }, input, el("button", { type: "button", class: "btn-ghost", text: "Add people", onclick: () => invite(g, input.value) })));
    }
    body.append(membersSection);

    // Invite link
    if (admin && g.join_policy === "public") {
      const out = el("div", { class: "gi-link", id: "gi-link-out" });
      body.append(section("Invite link",
        el("p", { class: "note", text: g.has_link ? "Anyone with the current link can join. Resetting makes the old link stop working." : "Create a link anyone can use to join this public group." }),
        out,
        el("div", { class: "gi-actions" },
          el("button", { type: "button", class: "btn-ghost", text: g.has_link ? "Reset link" : "Create link", onclick: () => makeLink(g, true, out) }),
          g.has_link ? el("button", { type: "button", class: "btn-ghost", text: "Turn off link", onclick: () => makeLink(g, false, out) }) : null)));
    }

    // Settings
    if (admin) {
      const join = el("select", { id: "gi-join" }, el("option", { value: "private", text: "Private — invite only" }), el("option", { value: "public", text: "Public — anyone with the link" }));
      join.value = g.join_policy;
      const post = el("select", { id: "gi-post" }, el("option", { value: "everyone", text: "Everyone" }), el("option", { value: "admins", text: "Only admins" }));
      post.value = g.post_policy;
      const inv = el("input", { type: "checkbox", id: "gi-invite", role: "switch" });
      inv.checked = Boolean(g.members_can_invite);
      const tmr = el("input", { type: "checkbox", id: "gi-timer-members", role: "switch" });
      tmr.checked = Boolean(g.members_can_change_timer);
      const live = el("select", { id: "gi-live" }, el("option", { value: "admins", text: "Only admins" }), el("option", { value: "everyone", text: "Everyone" }));
      live.value = g.stream_policy || "admins";
      body.append(section("Settings",
        el("label", { class: "field" }, el("span", { text: "Who can join" }), join),
        el("label", { class: "field" }, el("span", { text: "Who can post" }), post),
        G.streaming ? el("label", { class: "field" }, el("span", { text: "Who can go live" }), live) : null,
        el("label", { class: "peer-toggle" }, el("span", { text: "Members can add people" }), inv),
        G.disappearing ? el("label", { class: "peer-toggle" }, el("span", { text: "Members can change the timer" }), tmr) : null,
        el("button", { type: "button", class: "btn-ghost", text: "Save settings", onclick: () => patch(g, { join_policy: join.value, post_policy: post.value, members_can_invite: inv.checked, ...(G.disappearing ? { members_can_change_timer: tmr.checked } : {}), ...(G.streaming ? { stream_policy: live.value } : {}) }) })));
    }

    // Disappearing messages
    if (G.disappearing) {
      const may = admin || (g.members_can_change_timer && g.my && g.my.state === "active");
      const sel = timerSelect("gi-timer", g.ephemeral_secs || null);
      sel.disabled = !may;
      sel.onchange = () => patch(g, { ephemeral_secs: sel.value ? Number(sel.value) : null });
      body.append(section("Disappearing messages", el("label", { class: "field" }, el("span", { text: "New messages disappear after" }), sel),
        el("p", { class: "note", text: "The clock starts when a message is sent. " + LIMITS_NOTE })));
    }

    // Notifications
    const notify = el("select", { id: "gi-notify" }, el("option", { value: "all", text: "All messages" }), el("option", { value: "mentions", text: "Only mentions" }), el("option", { value: "none", text: "Nothing" }));
    notify.value = (g.my && g.my.notify) || "all";
    const mute = el("input", { type: "checkbox", id: "gi-mute", role: "switch" });
    mute.checked = Boolean(g.my && g.my.muted_until && g.my.muted_until > Date.now() / 1000);
    const saveNotify = () => api(groupPath(g.id, "/notifications"), { method: "PUT", body: { notify: notify.value, muted_until: mute.checked ? 4102444800 : null } })
      .then(() => { g.my.notify = notify.value; g.my.muted_until = mute.checked ? 4102444800 : null; renderList(); }).catch((e) => host().toast(explain(e)));
    notify.onchange = saveNotify;
    mute.onchange = saveNotify;
    body.append(section("Notifications", el("label", { class: "field" }, el("span", { text: "Notify me about" }), notify), el("label", { class: "peer-toggle" }, el("span", { text: "Mute" }), mute),
      el("p", { class: "note", text: "Notifications say only “New message”: the postbox cannot read group messages." })));

    // Safety number
    const c = client();
    const sn = c && c.safetyNumber(g.id);
    body.append(section("Encryption",
      el("p", { class: "note", text: "Compare this safety number with another member in person or on a call. If it matches, you both see the same devices in this group." }),
      el("p", { class: "safety-number", id: "gi-safety", text: sn ? sn.replace(/(\d{5})(?=\d)/g, "$1 ") : "Not available on this device yet." }),
      el("p", { class: "note", text: "This browser: " + (c ? c.fingerprint() || "" : "") }),
      el("p", { class: "note", text: "Hosted mailboxes' keys are held by the postbox, so it could add a device to one. Every new device is shown here and in the conversation; check fingerprints with people you trust." })));

    // Ownership, leave, delete
    const danger = [];
    if (owner) {
      const admins = members.filter((m) => m.role === "admin" && m.state === "active");
      if (admins.length) {
        const sel = el("select", { id: "gi-transfer" }, admins.map((m) => el("option", { value: m.address, text: nameOf(m.address) })));
        danger.push(el("div", { class: "chip-input" }, sel, el("button", { type: "button", class: "btn-ghost", text: "Transfer ownership", onclick: () => transfer(g, sel.value) })));
      } else danger.push(el("p", { class: "note", text: "To leave, first make another member an admin and transfer ownership to them." }));
    }
    danger.push(el("button", { type: "button", class: "btn-danger peer-action", id: "gi-leave", text: "Leave group", onclick: () => leave(g) }));
    if (owner) danger.push(el("button", { type: "button", class: "btn-danger peer-action", id: "gi-delete", text: "Delete group", onclick: () => removeGroup(g) }));
    body.append(section("Membership", ...danger));
    body.append(status);
  }

  function memberRow(g, m) {
    const admin = isAdmin(g);
    const owner = g.my && g.my.role === "owner";
    const self = m.address === me();
    const li = el("li", { class: "gi-member" });
    li.append(el("span", { class: "gi-member-name", text: nameOf(m.address) + (self ? " (you)" : "") }));
    li.append(el("span", { class: "pill role-" + m.role, text: m.state === "active" ? ROLE_LABEL[m.role] : m.state === "invited" ? "Invited" : "Joining" }));
    const actions = el("span", { class: "gi-member-actions" });
    const act = (label, change) => actions.append(el("button", { type: "button", class: "linkish", text: label, onclick: () => changeMember(g, m, change) }));
    if (m.state === "invited" && admin) act("Revoke", { state: "removed" });
    if (m.state === "active" && !self && m.role !== "owner") {
      if (admin && m.role !== "admin") act("Make admin", { role: "admin" });
      if (owner && m.role === "admin") act("Remove admin", { role: "member" });
      if (admin && m.role === "member") act("Restrict", { role: "restricted" });
      if (admin && m.role === "restricted") act("Unrestrict", { role: "member" });
      if (admin && (m.role !== "admin" || owner)) { act("Remove", { state: "removed" }); act("Ban", { state: "banned" }); }
    }
    if (self && m.role === "admin") act("Step down as admin", { role: "member" });
    li.append(actions);
    if (m.state === "active") {
      const devs = el("details", { class: "gi-devices" }, el("summary", { text: "Devices" }));
      devs.addEventListener("toggle", async () => {
        if (!devs.open || devs.dataset.loaded) return;
        devs.dataset.loaded = "1";
        const list = await loadDevices(m.address);
        for (const d of list) devs.append(el("p", { class: "fingerprint", text: `${d.device_id}: ${d.fingerprint_digits || d.fingerprint}` }));
        if (!list.length) devs.append(el("p", { class: "note", text: "No devices visible." }));
      });
      li.append(devs);
    }
    return li;
  }

  // Settings carry the group's revision, which also moves with every key change. Read it fresh
  // first; if somebody else's change still lands in between, reload once and reapply exactly the
  // fields this person chose.
  async function patch(g, fields) {
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        const fresh = await api(groupPath(g.id)).catch(() => null);
        const rev = fresh && ((fresh.group && fresh.group.revision) || fresh.revision);
        if (rev) g.revision = rev;
        const res = await api(groupPath(g.id), { method: "PATCH", body: { expected_revision: g.revision, ...fields } });
        Object.assign(g, res.group || res);
        host().toast("Saved.");
        break;
      } catch (e) {
        if (e.code === "revision_conflict" && attempt === 0) continue;
        host().toast(explain(e));
        break;
      }
    }
    await refresh();
    renderInfo();
  }

  async function changeMember(g, m, change) {
    const destructive = change.state === "removed" || change.state === "banned";
    if (destructive && !(await confirmAction(change.state === "banned" ? "Remove and ban?" : "Remove from the group?", `${nameOf(m.address)} loses access at once. Messages they already saw stay on their devices.`, change.state === "banned" ? "Ban" : "Remove"))) return;
    try {
      await api(groupPath(g.id, "/members/" + encodeURIComponent(m.address)), { method: "PATCH", body: { expected_revision: m.revision, ...change } });
      if (destructive) client().processPending(g.id, { noJitter: true }).catch(() => {});
    } catch (e) { host().toast(explain(e)); }
    await loadMembers(g.id);
    renderInfo();
    refresh();
  }

  async function invite(g, raw) {
    const list = String(raw || "").split(/[\s,]+/).filter(Boolean).map((v) => (v.startsWith("/") ? v : "/" + v));
    if (!list.length) return;
    try {
      const res = await api(groupPath(g.id, "/members"), { method: "POST", body: { request_key: "rk_" + Core.randomId(20), invite: list } });
      const ok = (res.results || []).filter((r) => r.status === "invited").length;
      host().toast(ok === list.length ? "Invited." : `${ok} of ${list.length} invited.`);
    } catch (e) { host().toast(explain(e)); }
    await loadMembers(g.id);
    renderInfo();
  }

  async function makeLink(g, enabled, out) {
    try {
      const res = await api(groupPath(g.id, "/link"), { method: "POST", body: { enabled } });
      out.textContent = "";
      if (res.url) {
        const input = el("input", { type: "text", readonly: true, value: res.url, "aria-label": "Invite link" });
        out.append(input, el("button", { type: "button", class: "btn-ghost", text: "Copy", onclick: () => { navigator.clipboard?.writeText(res.url).then(() => host().toast("Link copied."), () => input.select()); } }),
          el("p", { class: "note", text: "Shown once. Anyone with this link can join while it is on." }));
      }
      g.has_link = Boolean(res.link_token);
    } catch (e) { host().toast(explain(e)); }
  }

  async function transfer(g, to) {
    if (!(await confirmAction("Transfer ownership?", `${nameOf(to)} becomes the owner. You stay an admin.`, "Transfer"))) return;
    try { await api(groupPath(g.id, "/transfer"), { method: "POST", body: { to, expected_revision: g.revision } }); host().toast("Ownership transferred."); }
    catch (e) { host().toast(explain(e)); }
    await refresh();
    renderInfo();
  }

  async function leave(g) {
    if (!(await confirmAction("Leave this group?", "You stop receiving its messages. To come back you need a new invitation.", "Leave"))) return;
    try {
      await api(groupPath(g.id, "/leave"), { method: "POST" });
      await client().forget(g.id);
      hideSheet("group-info-sheet");
      close();
    } catch (e) { host().toast(explain(e)); }
    refresh();
  }

  async function removeGroup(g) {
    if (!(await confirmAction("Delete this group for everyone?", "Members see that it was deleted and its messages are removed from the postbox.", "Delete group"))) return;
    try {
      await api(groupPath(g.id), { method: "DELETE" });
      await client().forget(g.id);
      hideSheet("group-info-sheet");
      close();
    } catch (e) { host().toast(explain(e)); }
    refresh();
  }

  // ---- confirm -------------------------------------------------------------------------------------

  let confirmResolve = null;
  function buildConfirmSheet() {
    sheet("group-confirm-sheet", "Are you sure?", [el("p", { id: "gq-detail" })], [
      el("button", { class: "btn-ghost", type: "button", text: "Cancel", onclick: () => settleConfirm(false) }),
      el("button", { class: "btn-danger", id: "gq-ok", type: "button", text: "OK", onclick: () => settleConfirm(true) }),
    ]);
  }
  function settleConfirm(v) { hideSheet("group-confirm-sheet"); const r = confirmResolve; confirmResolve = null; if (r) r(v); }
  function confirmAction(title, detail, ok) {
    if (confirmResolve) settleConfirm(false);
    $("group-confirm-sheet-title").textContent = title;
    $("gq-detail").textContent = detail;
    $("gq-ok").textContent = ok;
    showSheet("group-confirm-sheet", "gq-ok");
    return new Promise((resolve) => { confirmResolve = resolve; });
  }

  // ---- a link to one group ---------------------------------------------------------------------------
  // `#group=<id>` (the Linux app opens live streams here, since WebKitGTK cannot play them): the group
  // opens once a mailbox that is a member of it is on screen. Nothing else is read from the link.
  let wantedGroup = (() => {
    const m = /^#group=(grp_[a-z2-7]{26})$/.exec(window.location.hash || "");
    if (!m) return null;
    history.replaceState(history.state, "", window.location.pathname + window.location.search);
    return { id: m[1], at: Date.now(), told: false };
  })();
  function pickUpGroupLink() {
    const wanted = wantedGroup;
    if (!wanted || Date.now() - wanted.at > 10 * 60 * 1000) { wantedGroup = null; return; }
    if (groupById(wanted.id)) { wantedGroup = null; openGroup(wanted.id); return; }
    if (!wanted.told) { wanted.told = true; host().toast("Switch to the mailbox that is in this group to open it."); }
  }

  // ---- joining by link -----------------------------------------------------------------------------

  const JOIN_KEY = "ppi_group_join";
  function buildJoinSheet() {
    sheet("group-join-sheet", "Join group", [el("div", { id: "gj-body" }), el("p", { class: "sheet-error", id: "gj-error", role: "alert", hidden: true })],
      [el("button", { class: "btn-ghost", type: "button", text: "Not now", onclick: () => hideSheet("group-join-sheet") }), el("button", { class: "btn-primary", id: "gj-join", type: "button", text: "Join" })]);
  }

  async function pickUpJoinLink() {
    let pending = null;
    try { pending = JSON.parse(localStorage.getItem(JOIN_KEY) || "null"); } catch (_) { pending = null; }
    if (!pending || !pending.group_id) return;
    localStorage.removeItem(JOIN_KEY);
    if (Date.now() - (pending.at || 0) > 30 * 60 * 1000) return;
    const body = $("gj-body");
    body.textContent = "";
    $("gj-error").hidden = true;
    const join = $("gj-join");
    join.hidden = true;
    showSheet("group-join-sheet");
    try {
      const preview = await api("/v1/groups/resolve", { method: "POST", body: { group_id: pending.group_id, link_token: pending.link_token || undefined } });
      body.append(el("p", { class: "gi-name", text: preview.name }), preview.description ? el("p", { class: "note", text: preview.description }) : null,
        el("p", { class: "note", text: `${preview.member_count || 1} members · joining as ${host().mailbox().handle || me()}` }),
        el("p", { class: "note", text: "Group messages are end-to-end encrypted. You will see messages from when you join, not before." }));
      if (preview.my_state === "active") { body.append(el("p", { text: "You are already a member." })); return; }
      join.hidden = !preview.can_join;
      if (!preview.can_join) body.append(el("p", { class: "note", text: preview.needs === "invitation" ? "You have an invitation to this group: accept it from Invitations in your conversation list." : "This group is invite only." }));
      join.onclick = async () => {
        join.disabled = true;
        try {
          await api(groupPath(pending.group_id, "/join"), { method: "POST", body: pending.link_token ? { link_token: pending.link_token } : {} });
          hideSheet("group-join-sheet");
          host().toast("Joined. A member's device will add you to the encryption in a moment.");
          await refresh();
          openGroup(pending.group_id);
        } catch (e) {
          showError("gj-error", explain(e));
        } finally { join.disabled = false; }
      };
    } catch (e) {
      body.append(el("p", { text: e.code === "not_found" || e.code === "invalid_link" ? "This invite link is not valid any more." : explain(e) }));
    }
  }

  // ---- disappearing direct messages ------------------------------------------------------------------

  function renderDirect() {
    const h = host();
    if (!h) return;
    const thread = G.disappearing ? h.openDirectThread() : null;
    // Header badge and note.
    let badge = $("dm-timer-badge");
    const headEl = $("thread-head");
    if (!badge && headEl) {
      badge = el("span", { id: "dm-timer-badge", class: "dm-timer-badge", hidden: true }, svg(ICON.timer), el("span", { id: "dm-timer-label" }));
      const anchor = $("find-btn");
      if (anchor) anchor.before(badge); else headEl.append(badge);
    }
    if (badge) {
      badge.hidden = !(thread && thread.ephemeral_secs) || Boolean(G.open);
      if (thread && thread.ephemeral_secs) { $("dm-timer-label").textContent = Core.timerLabel(thread.ephemeral_secs); badge.title = `Messages disappear after ${Core.timerLabel(thread.ephemeral_secs)}`; }
    }
    let note = $("dm-timer-note");
    const scroll = $("thread-scroll");
    if (!note && scroll) { note = el("p", { id: "dm-timer-note", class: "group-e2ee-note", hidden: true }); scroll.prepend(note); }
    if (note) {
      note.hidden = !(thread && thread.ephemeral_secs);
      if (thread && thread.ephemeral_secs) note.textContent = `Messages disappear after ${Core.timerLabel(thread.ephemeral_secs)}.`;
    }
    // Info sheet control.
    const info = $("peer-info");
    if (!info) return;
    let sec = $("dm-timer-section");
    if (!thread) { if (sec) sec.hidden = true; return; }
    if (!sec) {
      const sel = timerSelect("dm-timer", null);
      sel.setAttribute("aria-describedby", "dm-timer-help");
      sel.onchange = () => setDirectTimer(sel);
      sec = el("section", { class: "peer-section", id: "dm-timer-section" }, el("h3", { text: "Disappearing messages" }),
        el("label", { class: "field" }, el("span", { text: "New messages disappear after" }), sel),
        el("p", { class: "note", id: "dm-timer-help", text: "Applies to both of you; they see who changed it. The clock starts when a message is sent. " + LIMITS_NOTE }));
      const facts = info.querySelector('[aria-labelledby="peer-facts-title"]');
      if (facts) facts.before(sec); else info.append(sec);
    }
    sec.hidden = false;
    const sel = $("dm-timer");
    if (document.activeElement !== sel) sel.value = thread.ephemeral_secs ? String(thread.ephemeral_secs) : "";
    sel.dataset.thread = thread.id;
  }

  async function setDirectTimer(sel) {
    const h = host();
    const thread = h.openDirectThread();
    if (!thread) return;
    sel.disabled = true;
    try {
      const mb = h.mailbox();
      await h.api("/v1/threads/" + encodeURIComponent(thread.id) + "?identity=" + encodeURIComponent(mb.address), { method: "PATCH", body: { ephemeral_secs: sel.value ? Number(sel.value) : null } });
      await h.reloadThreads();
      h.toast(sel.value ? `New messages disappear after ${Core.timerLabel(Number(sel.value))}.` : "Disappearing messages are off.");
    } catch (e) {
      h.toast("Could not change the timer: " + ((e && e.message) || "error"));
    } finally {
      sel.disabled = false;
    }
  }

  // ---- render entry points -----------------------------------------------------------------------

  function renderAll() {
    renderList();
    renderView();
    renderDirect();
    const view = $("group-view");
    if (view && G.open) $("pane-thread").dataset.open = "true";
  }

  window.PigeonpostGroups = {
    onMailbox, onEvent, reset, close,
    render: () => { if (G.enabled || $("groups-section")) renderAll(); else renderDirect(); },
    isOpen: () => Boolean(G.open),
    dock, undock, // Phase 6
    // For tests and automation: state without message contents.
    inspect: () => ({ enabled: G.enabled, mailbox: G.mailbox, open: G.open, groups: G.groups.map((g) => ({ id: g.id, name: g.name, state: g.my && g.my.state })), deviceId: client() && client().deviceId() }),
    _client: () => client(),
    _open: openGroup,
  };
})();
