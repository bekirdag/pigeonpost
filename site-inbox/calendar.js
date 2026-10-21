// Pigeonpost inbox — calendar and scheduled meetings (Phase 3B).
//
// The contract is docs/calendar.md. Calendar data is NOT end-to-end encrypted: the postbox seals it
// at rest and can read it, because it renders reminders and invitations from it. Nothing in this file
// shows a lock or an "end-to-end" label on calendar data.
//
// Hidden entirely unless `GET /v1/capabilities` lists `calendar`. Wired into app.js by a handful of
// `// Phase 3B` lines through window.PigeonpostCalendarHost, the way groups.js is.
//
// Times: the server sends occurrences as Unix seconds (timed) or civil dates with an exclusive end
// (all-day). Everything on screen is placed by its wall time in the viewer's zone — the calendar
// setting's `timezone` when set, otherwise the browser's — computed with Intl.DateTimeFormat, so a
// 09:00 New York meeting stays at 09:00 for a New York viewer on both sides of a DST change.
//
// Untrusted text (titles, names, locations, notes) is always inserted with textContent.
(function () {
  "use strict";

  const host = () => window.PigeonpostCalendarHost;
  const $ = (id) => document.getElementById(id);
  const HOUR_PX = 48;
  const COLORS = ["blue", "green", "orange", "red", "purple", "teal", "gray"];
  const BYDAY = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];
  const DAY_NAMES = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
  const RSVP_WORD = { accepted: "Yes", tentative: "Maybe", declined: "No", needs_action: "Awaiting" };
  const AT_REST_NOTE = "Calendar events are stored encrypted at rest; Pigeonpost can read them to send reminders and invitations.";

  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      else if (k === "dataset") Object.assign(node.dataset, v);
      // Through the CSSOM: the inbox's CSP (style-src 'self') blocks a style attribute set as markup
      // or with setAttribute, which would leave the time grid unlaid.
      else if (k === "style") node.style.cssText = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else if (k === "hidden") node.hidden = Boolean(v);
      else if (k === "checked" || k === "disabled" || k === "selected") node[k] = Boolean(v);
      else node.setAttribute(k, v === true ? "" : String(v));
    }
    for (const c of children.flat()) if (c != null && c !== false) node.append(c instanceof Node ? c : document.createTextNode(String(c)));
    return node;
  }
  function svg(paths) {
    const ns = "http://www.w3.org/2000/svg";
    const s = document.createElementNS(ns, "svg");
    s.setAttribute("viewBox", "0 0 20 20");
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
  }
  const ICON = {
    calendar: ["M4 5h12v11H4zM4 8.5h12M7.5 3v3.5M12.5 3v3.5"],
    back: ["M12.5 4L6 10l6.5 6"],
    prev: ["M12 5l-5 5 5 5"],
    next: ["M8 5l5 5-5 5"],
    close: ["M5 5l10 10M15 5L5 15"],
    plus: ["M10 4.5v11M4.5 10h11"],
    gear: ["M10 12.9a2.9 2.9 0 100-5.8 2.9 2.9 0 000 5.8zM10 1.9l1.05 1.86a7.6 7.6 0 011.83.76L14.99 4l1.01 1.01-.53 2.11c.34.56.6 1.17.76 1.83L18.1 10l-1.87 1.05a7.6 7.6 0 01-.76 1.83l.53 2.11L14.99 16l-2.11-.53a7.6 7.6 0 01-1.83.76L10 18.1l-1.05-1.87a7.6 7.6 0 01-1.83-.76L5.01 16 4 14.99l.53-2.11a7.6 7.6 0 01-.76-1.83L1.9 10l1.87-1.05c.16-.66.42-1.27.76-1.83L4 5.01 5.01 4l2.11.53c.56-.34 1.17-.6 1.83-.76z"],
    envelope: ["M3 5.5h14v9H3zM3 6l7 5 7-5"],
    video: ["M4.5 5h7a2 2 0 012 2v6a2 2 0 01-2 2h-7a2 2 0 01-2-2V7a2 2 0 012-2zM13.5 9l4-2.3v6.6l-4-2.3"],
    link: ["M8.5 11.5l3-3M7 9.5l-1.5 1.5a2.5 2.5 0 003.5 3.5L10.5 13M13 10.5l1.5-1.5A2.5 2.5 0 0011 5.5L9.5 7"],
    pin: ["M10 17s5-4.6 5-8.5a5 5 0 00-10 0C5 12.4 10 17 10 17zM10 10.2a1.7 1.7 0 100-3.4 1.7 1.7 0 000 3.4z"],
  };

  // ---- state ------------------------------------------------------------------------------------

  const C = {
    enabled: false,
    contactInvites: false, // `calendar_email`: invitations go out to contact addresses
    mailbox: null,
    generation: 0,
    wasOpen: false,
    view: null,           // month | week | day | agenda
    cursor: null,         // YYYY-MM-DD the view is about
    focusDate: null,      // month view keyboard focus
    occurrences: [],
    range: null,          // { from, to } fetched
    fetchGen: 0,
    loading: false,
    error: null,
    calendars: [],
    hidden: new Set(),    // calendar ids switched off on this device
    settings: { previews: false, timezone: null, week_start: 1, working_hours: null },
    invitations: [],
    filtered: [],
    detail: null,         // { event, occurrence, saved }
    form: null,           // the create/edit sheet's working state
    nowOverride: null,
    zoneOverride: null,
    nowTimer: null,
  };

  // ---- api ----------------------------------------------------------------------------------------

  class CalendarError extends Error {
    constructor(status, body) {
      super((body && (body.message || body.error)) || "http_" + status);
      this.status = status;
      this.code = body && body.error;
      this.body = body || {};
    }
  }

  async function api(path, o = {}, renewed) {
    const h = host();
    const headers = { authorization: "Bearer " + h.token(), accept: o.raw ? "text/calendar" : "application/json" };
    if (C.mailbox) headers["x-pigeonpost-identity"] = C.mailbox;
    let body;
    if (o.text != null) { headers["content-type"] = "text/calendar"; body = o.text; }
    else if (o.body) { headers["content-type"] = "application/json"; body = JSON.stringify(o.body); }
    const res = await fetch(h.postbox() + path, { method: o.method || "GET", headers, body });
    if (res.status === 401 && !renewed && (await h.renew())) return api(path, o, true);
    if (o.raw && res.ok) return res.text();
    const value = await res.json().catch(() => ({}));
    if (!res.ok) throw new CalendarError(res.status, value);
    return value;
  }
  const eventPath = (id, rest = "") => "/v1/calendar/events/" + encodeURIComponent(id) + rest;
  const key = () => "web-" + Date.now().toString(36) + "-" + Math.random().toString(36).slice(2, 10);

  function explain(e) {
    const code = e && e.code;
    const body = (e && e.body) || {};
    if (code === "time_invalid") {
      if (body.kind === "nonexistent") return "That time doesn’t exist on that day: the clocks go forward then. Pick another time.";
      if (body.kind === "order") return "The end has to be after the start.";
      return "That time happens twice on that day because the clocks go back. Choose which one you mean.";
    }
    const known = {
      feature_disabled: "Calendar isn’t enabled for this account.",
      not_found: "This event isn’t available any more.",
      forbidden: "Only the organizer can do that.",
      human_required: "Inviting people by contact address needs you to be signed in yourself, with a confirmed sign-in address — not an agent key.",
      revision_conflict: "This event was changed somewhere else while you were editing. Reload it and try again.",
      idempotency_conflict: "That was already sent with different details. Try again.",
      recurrence_invalid: "That repeat rule isn’t supported.",
      zone_unknown: "That time zone isn’t known.",
      window_too_large: "That range is too long to show at once.",
      expansion_limit: "Too many events in this range. Show a shorter range.",
      attendee_limit: "Too many guests: at most 500 people with handles and 50 contact addresses per event.",
      invite_limit: "You’ve reached today’s limit of 200 new invitations to contact addresses. Try again tomorrow.",
      meeting_capacity: "Too many people for this meeting. A small call holds 6 people, a large call 30 — choose Large call, or mark some guests optional.",
      external_unavailable: "Invitations to contact addresses are not sent yet.",
      event_cancelled: "This event was cancelled.",
      calendar_unavailable: "That calendar isn’t available.",
      rate_limited: "Too many requests. Wait a moment and try again.",
    };
    return known[code] || body.message || "Something went wrong. Try again.";
  }

  // ---- time --------------------------------------------------------------------------------------

  const pad = (n) => String(n).padStart(2, "0");
  const browserZone = () => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC"; } catch (_) { return "UTC"; } };
  const zone = () => C.zoneOverride || C.settings.timezone || browserZone();
  const now = () => (C.nowOverride != null ? C.nowOverride : Date.now() / 1000);

  const partsCache = new Map();
  function wall(unix, z = zone()) {
    let f = partsCache.get(z);
    if (!f) {
      f = new Intl.DateTimeFormat("en-US", { timeZone: z, hourCycle: "h23", year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit" });
      partsCache.set(z, f);
    }
    const p = {};
    for (const part of f.formatToParts(new Date(unix * 1000))) p[part.type] = part.value;
    const y = Number(p.year), m = Number(p.month), d = Number(p.day);
    return { y, m, d, h: Number(p.hour) % 24, mi: Number(p.minute), s: Number(p.second), ymd: ymdOf(y, m, d) };
  }
  function offsetAt(unix, z) {
    const w = wall(unix, z);
    return Date.UTC(w.y, w.m - 1, w.d, w.h, w.mi, w.s) / 1000 - unix;
  }
  // The instant a wall time names in a zone. Ambiguous (fall-back) times take `fold`; times in a
  // spring-forward gap move forward by the gap, as RFC 5545 does.
  function zonedToUnix(y, m, d, h, mi, z, fold = "earlier") {
    const guess = Date.UTC(y, m - 1, d, h, mi) / 1000;
    const offsets = [...new Set([offsetAt(guess - 86400, z), offsetAt(guess + 86400, z), offsetAt(guess, z)])];
    const hits = offsets.map((o) => guess - o).filter((t) => { const w = wall(t, z); return w.y === y && w.m === m && w.d === d && w.h === h && w.mi === mi; }).sort((a, b) => a - b);
    if (hits.length) return fold === "later" ? hits[hits.length - 1] : hits[0];
    return guess - offsetAt(guess - 86400, z);
  }
  function ymdOf(y, m, d) { return `${String(y).padStart(4, "0")}-${pad(m)}-${pad(d)}`; }
  function parseYmd(s) { const [y, m, d] = String(s).split("-").map(Number); return { y, m, d }; }
  function addDays(s, n) { const { y, m, d } = parseYmd(s); const t = new Date(Date.UTC(y, m - 1, d + n)); return ymdOf(t.getUTCFullYear(), t.getUTCMonth() + 1, t.getUTCDate()); }
  function addMonths(s, n) { const { y, m } = parseYmd(s); const t = new Date(Date.UTC(y, m - 1 + n, 1)); return ymdOf(t.getUTCFullYear(), t.getUTCMonth() + 1, 1); }
  function weekday(s) { const { y, m, d } = parseYmd(s); return new Date(Date.UTC(y, m - 1, d)).getUTCDay(); }
  const dayStart = (s, z = zone()) => { const { y, m, d } = parseYmd(s); return zonedToUnix(y, m, d, 0, 0, z); };
  const todayYmd = () => wall(now()).ymd;
  const wallMinutes = (unix) => { const w = wall(unix); return w.h * 60 + w.mi; };

  const fmtCache = new Map();
  function fmt(options) {
    const k = JSON.stringify(options);
    let f = fmtCache.get(k);
    if (!f) { f = new Intl.DateTimeFormat(undefined, options); fmtCache.set(k, f); }
    return f;
  }
  const dateLabel = (ymd, options) => { const { y, m, d } = parseYmd(ymd); return fmt({ ...options, timeZone: "UTC" }).format(new Date(Date.UTC(y, m - 1, d, 12))); };
  const timeLabel = (unix, z = zone()) => fmt({ hour: "2-digit", minute: "2-digit", timeZone: z }).format(new Date(unix * 1000));
  const longDay = (ymd) => dateLabel(ymd, { weekday: "long", day: "numeric", month: "long" });
  function zoneName(z) {
    const city = String(z || "").split("/").pop().replace(/_/g, " ");
    return city || z;
  }
  function whenLabel(o, z = zone()) {
    if (o.all_day) {
      const last = addDays(o.end_date, -1);
      return last === o.start_date ? longDay(o.start_date) + " · all day" : longDay(o.start_date) + " – " + longDay(last) + " · all day";
    }
    const startDay = wall(o.starts_at, z).ymd, endDay = wall(o.ends_at, z).ymd;
    const day = dateLabel(startDay, { weekday: "long", day: "numeric", month: "long", year: "numeric" });
    if (startDay === endDay) return `${day} · ${timeLabel(o.starts_at, z)} – ${timeLabel(o.ends_at, z)}`;
    return `${day} ${timeLabel(o.starts_at, z)} – ${dateLabel(endDay, { day: "numeric", month: "long" })} ${timeLabel(o.ends_at, z)}`;
  }
  const codeLabel = (code) => (code && code.length === 10 ? code.slice(0, 5) + "-" + code.slice(5) : code || "");

  // ---- loading ------------------------------------------------------------------------------------

  async function onMailbox() {
    const h = host();
    const mb = h && h.mailbox();
    const features = h ? h.features() : new Set();
    const gen = ++C.generation;
    C.contactInvites = features.has("calendar_email");
    if (!mb || !features.has("calendar")) { teardown(); return; }
    if (C.mailbox !== mb.address) {
      C.occurrences = []; C.invitations = []; C.filtered = []; C.calendars = []; C.detail = null; C.range = null;
      hideSheet("cal-detail-sheet");
      loadHidden(mb.address);
    }
    C.mailbox = mb.address;
    C.enabled = true;
    ensureDom();
    render();
    await Promise.all([loadSettings(), loadCalendars(), loadInvitations()]);
    if (gen !== C.generation) return;
    if (isOpen()) refetch();
    render();
  }

  function teardown() {
    C.enabled = false;
    C.wasOpen = false;
    clearInterval(C.nowTimer);
    for (const id of ["calendar-btn", "calendar-side", "calendar-view"]) { const n = $(id); if (n) n.hidden = true; }
    for (const s of [...sheetsOpen]) hideSheet(s);
  }
  function reset() {
    teardown();
    C.mailbox = null;
    C.occurrences = [];
    C.invitations = [];
    C.calendars = [];
    C.view = null;
  }

  async function loadSettings() {
    try {
      const body = await api("/v1/calendar/settings");
      C.settings = Object.assign({ previews: false, timezone: null, week_start: 1, working_hours: null }, body.settings || body);
    } catch (_) { /* defaults */ }
  }
  async function loadCalendars() {
    try {
      const body = await api("/v1/calendars");
      C.calendars = Array.isArray(body.calendars) ? body.calendars.filter((c) => c && c.id) : [];
    } catch (_) { C.calendars = []; }
    renderSide();
  }
  async function loadInvitations() {
    try {
      const body = await api("/v1/calendar/invitations?state=pending");
      C.invitations = Array.isArray(body.invitations) ? body.invitations : [];
    } catch (_) { C.invitations = []; }
    syncNav();
    renderBanner();
    if ($("cal-invites-sheet") && !$("cal-invites-sheet").hidden) renderInvites();
  }
  async function loadFiltered() {
    try {
      const body = await api("/v1/calendar/invitations?state=filtered");
      C.filtered = Array.isArray(body.invitations) ? body.invitations : [];
    } catch (_) { C.filtered = []; }
  }

  function hiddenKey(mailbox) { return "ppi_cal_hidden:" + mailbox; }
  function loadHidden(mailbox) {
    C.hidden = new Set();
    try { for (const id of JSON.parse(localStorage.getItem(hiddenKey(mailbox)) || "[]")) C.hidden.add(String(id)); } catch (_) { /* fine */ }
  }
  function saveHidden() {
    try { localStorage.setItem(hiddenKey(C.mailbox), JSON.stringify([...C.hidden])); } catch (_) { /* fine */ }
  }

  function visibleDays() {
    const c = C.cursor;
    if (C.view === "month") {
      const start = weekStartOf(c.slice(0, 8) + "01");
      return Array.from({ length: 42 }, (_, i) => addDays(start, i));
    }
    if (C.view === "week") { const start = weekStartOf(c); return Array.from({ length: 7 }, (_, i) => addDays(start, i)); }
    if (C.view === "day") return [c];
    return Array.from({ length: 14 }, (_, i) => addDays(c, i));
  }
  function weekStartOf(ymd) {
    const ws = ((Number(C.settings.week_start) % 7) + 7) % 7;
    return addDays(ymd, -((weekday(ymd) - ws + 7) % 7));
  }

  async function refetch() {
    if (!C.enabled || !C.view) return;
    const days = visibleDays();
    const from = dayStart(days[0]), to = dayStart(addDays(days[days.length - 1], 1));
    const gen = ++C.fetchGen;
    C.loading = true;
    renderStatus();
    try {
      const body = await api(`/v1/calendar/events?from=${from}&to=${to}&include=invitations`);
      if (gen !== C.fetchGen) return;
      C.occurrences = Array.isArray(body.occurrences) ? body.occurrences.filter((o) => o && o.event_id) : [];
      C.range = { from, to };
      C.error = null;
    } catch (e) {
      if (gen !== C.fetchGen) return;
      C.error = e.code === "expansion_limit" ? explain(e) : "Couldn’t load your calendar.";
    }
    C.loading = false;
    renderBody();
    renderStatus();
  }

  // ---- DOM scaffolding ---------------------------------------------------------------------------

  function ensureDom() {
    const nav = $("side-nav");
    if (nav && !$("calendar-btn")) {
      nav.append(el("button", { id: "calendar-btn", class: "side-nav-btn", type: "button", "aria-pressed": "false", onclick: () => (isOpen() ? close() : open()) },
        svg(ICON.calendar), el("span", { text: "Calendar" }), el("span", { class: "dot calendar-badge", id: "calendar-badge", hidden: true })));
    }
    if (!$("calendar-side")) buildSide();
    if (!$("calendar-view")) buildView();
    if (!$("cal-detail-sheet")) buildSheets();
  }

  function buildSide() {
    const anchor = $("calls-view") || $("threads");
    if (!anchor) return;
    const side = el("section", { class: "side-view cal-side", id: "calendar-side", "aria-labelledby": "cal-side-title", hidden: true },
      el("div", { class: "archive-banner side-view-head" },
        el("span", { id: "cal-side-title", text: "Calendar" }),
        el("button", { class: "linkish", type: "button", text: "Back to inbox", onclick: () => close() })),
      el("button", { class: "btn-primary cal-new-side", id: "cal-side-new", type: "button", onclick: () => openCreate() }, svg(ICON.plus), el("span", { text: "New event" })),
      el("div", { class: "cal-mini", id: "cal-mini" }),
      el("button", { class: "cal-side-invites", id: "cal-side-invites", type: "button", hidden: true, onclick: () => openInvites() }),
      el("h3", { class: "cal-side-label", id: "cal-list-title", text: "My calendars" }),
      el("ul", { class: "cal-list", id: "cal-list", "aria-labelledby": "cal-list-title" }),
      el("button", { class: "linkish cal-side-settings", type: "button", text: "Calendar settings", onclick: () => openSettings() }));
    anchor.after(side);
  }

  function buildView() {
    const pane = $("pane-thread");
    if (!pane) return;
    const viewBtn = (v, label) => el("label", {}, el("input", { type: "radio", name: "cal-view", value: v, onchange: () => setView(v) }), el("span", { text: label }));
    const view = el("section", { id: "calendar-view", class: "calendar-view", "aria-labelledby": "cal-range", hidden: true },
      el("header", { class: "cal-head" },
        el("button", { class: "icon-btn back cal-back", type: "button", "aria-label": "Back to the inbox", onclick: () => close() }, svg(ICON.back)),
        el("button", { class: "btn-ghost cal-today", id: "cal-today", type: "button", text: "Today", onclick: () => goto(todayYmd()) }),
        el("div", { class: "cal-step" },
          el("button", { class: "icon-btn", id: "cal-prev", type: "button", "aria-label": "Previous", onclick: () => step(-1) }, svg(ICON.prev)),
          el("button", { class: "icon-btn", id: "cal-next", type: "button", "aria-label": "Next", onclick: () => step(1) }, svg(ICON.next))),
        el("h2", { class: "cal-range", id: "cal-range", "aria-live": "polite" }),
        el("div", { class: "segmented cal-views", id: "cal-views", role: "radiogroup", "aria-label": "Calendar view" },
          viewBtn("month", "Month"), viewBtn("week", "Week"), viewBtn("day", "Day"), viewBtn("agenda", "Agenda")),
        el("button", { class: "icon-btn cal-head-btn", id: "cal-new", type: "button", title: "New event", "aria-label": "New event", onclick: () => openCreate() }, svg(ICON.plus)),
        el("button", { class: "icon-btn cal-head-btn", id: "cal-settings-btn", type: "button", title: "Calendar settings", "aria-label": "Calendar settings", onclick: () => openSettings() }, svg(ICON.gear))),
      el("button", { class: "cal-banner", id: "cal-invites-banner", type: "button", hidden: true, onclick: () => openInvites() }),
      el("p", { class: "inbox-status cal-status", id: "cal-status", role: "status", hidden: true }),
      el("div", { class: "cal-body", id: "cal-body" }));
    pane.append(view);
  }

  // Our own small sheet manager, like groups.js: app.js's stack is private to it.
  const sheetsOpen = [];
  let sheetReturn = new Map();
  function showSheet(id, focus) {
    const s = $(id);
    if (!s) return;
    if (!sheetReturn.has(id)) sheetReturn.set(id, document.activeElement);
    s.hidden = false;
    const at = sheetsOpen.indexOf(id);
    if (at !== -1) sheetsOpen.splice(at, 1);
    sheetsOpen.push(id);
    for (const x of sheetsOpen) $(x).inert = x !== id;
    const app = $("app");
    if (app) app.inert = true;
    s.style.zIndex = String(70 + sheetsOpen.length);
    const target = focus ? $(focus) : s.querySelector("input:not([type=hidden]), select, textarea, button:not(.icon-btn)");
    if (target) target.focus({ preventScroll: true });
  }
  function hideSheet(id) {
    const s = $(id);
    if (!s || s.hidden) return;
    s.hidden = true;
    const at = sheetsOpen.indexOf(id);
    if (at !== -1) sheetsOpen.splice(at, 1);
    for (const x of sheetsOpen) $(x).inert = x !== sheetsOpen[sheetsOpen.length - 1];
    if (!sheetsOpen.length) {
      const app = $("app");
      if (app && !document.querySelector(".sheet-wrap:not([hidden]):not(.cal-sheet)")) app.inert = false;
    }
    const back = sheetReturn.get(id);
    sheetReturn.delete(id);
    if (back && back.isConnected && typeof back.focus === "function") back.focus({ preventScroll: true });
  }
  function sheet(id, title, wide) {
    const titleId = id + "-title";
    const wrap = el("div", { class: "sheet-wrap cal-sheet", id, hidden: true },
      el("div", { class: "sheet" + (wide ? " sheet-wide" : ""), role: "dialog", "aria-modal": "true", "aria-labelledby": titleId },
        el("header", { class: "sheet-head" },
          el("h2", { id: titleId, text: title }),
          el("button", { class: "icon-btn", type: "button", "aria-label": "Close", onclick: () => hideSheet(id) }, svg(ICON.close))),
        el("div", { class: "sheet-body", id: id + "-body" }),
        el("footer", { class: "sheet-foot", id: id + "-foot" })));
    wrap.addEventListener("click", (e) => { if (e.target === wrap) hideSheet(id); });
    wrap.addEventListener("keydown", (e) => { if (e.key === "Escape") { e.stopPropagation(); e.preventDefault(); hideSheet(id); } });
    document.body.append(wrap);
    return wrap;
  }
  function fill(id, ...nodes) { const n = $(id); n.textContent = ""; n.append(...nodes.flat().filter(Boolean)); return n; }
  function showError(id, text) { const e = $(id); if (!e) return; e.textContent = text; e.hidden = !text; }

  function buildSheets() {
    sheet("cal-detail-sheet", "Event", true);
    sheet("cal-edit-sheet", "New event", true);
    sheet("cal-scope-sheet", "Change a recurring event");
    sheet("cal-cancel-sheet", "Cancel event");
    sheet("cal-invites-sheet", "Invitations", true);
    sheet("cal-settings-sheet", "Calendar settings", true);
    sheet("cal-choose-sheet", "Which meeting?");
    // A reminder can carry a Join button, which app.js's text toast cannot.
    document.body.append(el("div", { class: "cal-toast", id: "cal-toast", role: "status", "aria-live": "polite", hidden: true },
      el("span", { id: "cal-toast-text" }),
      el("button", { class: "btn-primary btn-small", id: "cal-toast-join", type: "button", text: "Join", hidden: true }),
      el("button", { class: "icon-btn", type: "button", "aria-label": "Dismiss", onclick: () => { $("cal-toast").hidden = true; } }, svg(ICON.close))));
  }

  // ---- open / close / navigation -------------------------------------------------------------------

  const isOpen = () => Boolean(C.enabled && host() && host().leftView() === "calendar");

  function open() {
    const h = host();
    if (!C.enabled || !h) return;
    h.closeConversation();
    window.PigeonpostGroups?.close();
    if (h.onPhone() && !isOpen()) { try { history.pushState({ calendar: true }, ""); } catch (_) { /* fine */ } }
    h.setLeftView("calendar");
  }
  function close() {
    const h = host();
    if (!h || !isOpen()) return;
    h.setLeftView("inbox");
  }
  window.addEventListener("popstate", () => { if (isOpen() && host() && host().onPhone()) close(); });

  function setView(v) {
    C.view = v;
    if (v === "month") C.focusDate = C.cursor;
    renderHead();
    renderBody();
    refetch();
  }
  function goto(ymd) {
    C.cursor = ymd;
    C.focusDate = ymd;
    renderHead();
    renderBody();
    refetch();
  }
  function step(dir) {
    const c = C.cursor;
    if (C.view === "month") goto(addMonths(c, dir));
    else if (C.view === "week") goto(addDays(c, 7 * dir));
    else if (C.view === "day") goto(addDays(c, dir));
    else goto(addDays(c, 14 * dir));
  }

  // ---- rendering --------------------------------------------------------------------------------

  function render() {
    if (!C.enabled) return;
    const open = isOpen();
    if ($("calendar-side")) $("calendar-side").hidden = !open;
    if ($("calendar-view")) $("calendar-view").hidden = !open;
    syncNav();
    if (open) {
      $("pane-thread").dataset.open = "true";
      if (!C.wasOpen) {
        C.wasOpen = true;
        if (!C.view) C.view = host().onPhone() ? "agenda" : "week";
        if (!C.cursor) C.cursor = todayYmd();
        C.focusDate = C.cursor;
        renderHead();
        renderSide();
        renderBanner();
        renderBody();
        refetch();
        clearInterval(C.nowTimer);
        C.nowTimer = setInterval(renderNow, 60000);
      }
    } else if (C.wasOpen) {
      C.wasOpen = false;
      clearInterval(C.nowTimer);
    }
  }

  function syncNav() {
    const btn = $("calendar-btn");
    if (!btn) return;
    btn.hidden = !C.enabled;
    btn.setAttribute("aria-pressed", String(isOpen()));
    const n = C.invitations.length;
    const badge = $("calendar-badge");
    badge.hidden = !n;
    badge.textContent = n > 99 ? "99+" : String(n);
    btn.setAttribute("aria-label", n ? `Calendar, ${n} ${n === 1 ? "invitation" : "invitations"}` : "Calendar");
  }

  function renderHead() {
    if (!$("cal-range")) return;
    const c = C.cursor;
    let label = "";
    if (C.view === "month") label = dateLabel(c, { month: "long", year: "numeric" });
    else if (C.view === "week") {
      const days = visibleDays();
      label = dateLabel(days[0], { day: "numeric", month: "short" }) + " – " + dateLabel(days[6], { day: "numeric", month: "short", year: "numeric" });
    } else if (C.view === "day") label = dateLabel(c, { weekday: "long", day: "numeric", month: "long", year: "numeric" });
    else label = "From " + dateLabel(c, { day: "numeric", month: "long" });
    $("cal-range").textContent = label;
    for (const input of document.querySelectorAll('input[name="cal-view"]')) input.checked = input.value === C.view;
    const unit = { month: "month", week: "week", day: "day", agenda: "two weeks" }[C.view];
    $("cal-prev").setAttribute("aria-label", "Previous " + unit);
    $("cal-next").setAttribute("aria-label", "Next " + unit);
    renderMini();
  }

  function renderStatus() {
    const s = $("cal-status");
    if (!s) return;
    s.hidden = !(C.error || (C.loading && !C.occurrences.length));
    s.textContent = C.error || "Loading…";
  }

  function renderBanner() {
    const n = C.invitations.length;
    const banner = $("cal-invites-banner");
    if (banner) {
      banner.hidden = !n;
      banner.textContent = "";
      banner.append(el("span", { text: `${n} ${n === 1 ? "invitation" : "invitations"}` }), el("strong", { text: " — Review" }));
    }
    const side = $("cal-side-invites");
    if (side) { side.hidden = !n; side.textContent = `Invitations (${n})`; }
  }

  function renderSide() {
    const list = $("cal-list");
    if (!list) return;
    list.textContent = "";
    if (!C.calendars.length) list.append(el("li", { class: "field-note", text: "Your calendar is created with your first event." }));
    for (const cal of C.calendars) {
      const id = "cal-toggle-" + cal.id;
      list.append(el("li", { class: "cal-list-row" },
        el("input", { type: "checkbox", id, class: "cal-color-check", dataset: { color: COLORS.includes(cal.color) ? cal.color : "blue" }, checked: !C.hidden.has(cal.id),
          onchange: (e) => { if (e.target.checked) C.hidden.delete(cal.id); else C.hidden.add(cal.id); saveHidden(); renderBody(); } }),
        el("label", { for: id, text: cal.name || "Calendar" })));
    }
    renderMini();
  }

  function renderMini() {
    const box = $("cal-mini");
    if (!box || !C.cursor) return;
    box.textContent = "";
    const month = C.miniMonth || C.cursor.slice(0, 8) + "01";
    const start = weekStartOf(month);
    const today = todayYmd();
    const head = el("div", { class: "cal-mini-head" },
      el("button", { class: "icon-btn", type: "button", "aria-label": "Previous month", onclick: () => { C.miniMonth = addMonths(month, -1); renderMini(); } }, svg(ICON.prev)),
      el("span", { class: "cal-mini-title", text: dateLabel(month, { month: "long", year: "numeric" }) }),
      el("button", { class: "icon-btn", type: "button", "aria-label": "Next month", onclick: () => { C.miniMonth = addMonths(month, 1); renderMini(); } }, svg(ICON.next)));
    const grid = el("div", { class: "cal-mini-grid", role: "group", "aria-label": "Pick a date" });
    for (let i = 0; i < 7; i++) grid.append(el("span", { class: "cal-mini-dow", "aria-hidden": "true", text: dateLabel(addDays(start, i), { weekday: "narrow" }) }));
    for (let i = 0; i < 42; i++) {
      const d = addDays(start, i);
      grid.append(el("button", {
        type: "button", class: "cal-mini-day", text: String(parseYmd(d).d), "aria-label": longDay(d),
        dataset: { out: String(d.slice(0, 7) !== month.slice(0, 7)), today: String(d === today), selected: String(d === C.cursor) },
        "aria-current": d === today ? "date" : null,
        onclick: () => { C.miniMonth = null; goto(d); },
      }));
    }
    box.append(head, grid);
  }

  const shown = () => C.occurrences.filter((o) => !(o.calendar_id && C.hidden.has(o.calendar_id)) && o.my_rsvp !== "filtered");

  function renderBody() {
    const body = $("cal-body");
    if (!body || !C.view || !C.cursor) return;
    body.textContent = "";
    body.dataset.view = C.view;
    if (C.view === "month") body.append(monthView());
    else if (C.view === "agenda") body.append(agendaView());
    else body.append(gridView(visibleDays()));
    if (C.view === "week" || C.view === "day") {
      const scroller = $("cal-grid-scroll");
      if (scroller) scroller.scrollTop = 7.5 * HOUR_PX;
    }
  }

  // Inline Join: the meeting's window is open (from `join_before_minutes` before the start until 30
  // minutes after the end), and the viewer has not declined.
  function joinable(o) {
    if (!o || !o.meeting || !o.meeting.code || o.status === "cancelled" || o.my_rsvp === "declined") return false;
    const t = now();
    const opens = o.meeting.opens_at != null ? o.meeting.opens_at : o.starts_at - 600;
    const closes = o.meeting.closes_at != null ? o.meeting.closes_at : o.ends_at + 1800;
    return t >= opens && t <= closes;
  }
  function joinButton(o, cls = "") {
    return el("button", { class: "btn-primary btn-small cal-join " + cls, type: "button", text: "Join", "aria-label": "Join " + (o.title || "meeting"),
      onclick: (e) => { e.stopPropagation(); join(o.meeting.code, o.occurrence_id, o.title); } });
  }
  function colorOf(o) {
    const cal = C.calendars.find((c) => c.id === o.calendar_id);
    const color = o.color || (cal && cal.color) || "blue";
    return COLORS.includes(color) ? color : "blue";
  }
  function eventClasses(o) {
    return ["cal-event", o.invited || o.my_rsvp === "needs_action" ? "is-invited" : "", o.my_rsvp === "declined" ? "is-declined" : "",
      o.status === "cancelled" ? "is-cancelled" : "", o.transparency === "transparent" ? "is-free" : ""].filter(Boolean).join(" ");
  }
  function rsvpNote(o) {
    if (o.status === "cancelled") return "Cancelled";
    if (o.my_rsvp === "needs_action" || o.invited) return "Invited";
    if (o.my_rsvp === "declined") return "Declined";
    if (o.my_rsvp === "tentative") return "Maybe";
    return "";
  }

  // ---- week and day: the time grid -------------------------------------------------------------------

  function allDayOn(ymd) { return shown().filter((o) => o.all_day && o.start_date <= ymd && ymd < o.end_date); }
  function timedOn(ymd) {
    const s = dayStart(ymd), e = dayStart(addDays(ymd, 1));
    return shown().filter((o) => !o.all_day && o.starts_at < e && (o.ends_at > s || (o.ends_at <= o.starts_at && o.starts_at >= s)));
  }
  function segmentsFor(ymd) {
    const s = dayStart(ymd), e = dayStart(addDays(ymd, 1));
    return timedOn(ymd).map((o) => {
      const a = Math.max(o.starts_at, s), b = Math.min(Math.max(o.ends_at, o.starts_at + 900), e);
      const top = a <= s ? 0 : wallMinutes(a);
      const bottom = b >= e ? 1440 : wallMinutes(b);
      return { o, top, bottom: Math.max(bottom, top + 15), continues: o.starts_at < s };
    });
  }
  // Overlapping events sit side by side: each cluster of overlaps is split into as many columns as
  // its widest moment needs.
  function layoutColumns(segs) {
    segs.sort((a, b) => a.top - b.top || b.bottom - a.bottom);
    const out = [];
    let cluster = [], end = -1;
    const flush = () => {
      const cols = [];
      for (const s of cluster) {
        let i = cols.findIndex((colEnd) => colEnd <= s.top);
        if (i === -1) { i = cols.length; cols.push(0); }
        cols[i] = s.bottom;
        s.col = i;
      }
      for (const s of cluster) s.cols = cols.length;
      out.push(...cluster);
      cluster = [];
    };
    for (const s of segs) {
      if (cluster.length && s.top >= end) { flush(); end = -1; }
      cluster.push(s);
      end = Math.max(end, s.bottom);
    }
    if (cluster.length) flush();
    return out;
  }

  function chip(o, extra = "") {
    return el("button", { type: "button", class: "cal-chip " + eventClasses(o) + " " + extra, dataset: { color: colorOf(o), occurrence: o.occurrence_id || "", event: o.event_id },
      "aria-label": (o.title || "(No title)") + ", " + whenLabel(o) + (rsvpNote(o) ? ", " + rsvpNote(o) : ""),
      onclick: (e) => { e.stopPropagation(); openDetail(o.event_id, o.occurrence_id); } },
    o.all_day ? null : el("span", { class: "cal-chip-time", text: timeLabel(o.starts_at) }),
    el("span", { class: "cal-chip-title", text: o.title || "(No title)" }));
  }

  function gridView(days) {
    const today = todayYmd();
    const cols = `56px repeat(${days.length}, minmax(0, 1fr))`;
    const wrap = el("div", { class: "cal-week", dataset: { days: String(days.length) } });
    const heads = el("div", { class: "cal-week-heads", style: `grid-template-columns:${cols}` }, el("span", { class: "cal-gutter", "aria-hidden": "true" }));
    for (const d of days) {
      heads.append(el("button", { type: "button", class: "cal-week-head", dataset: { today: String(d === today) }, "aria-label": longDay(d) + ". Show this day.",
        onclick: () => { C.cursor = d; setView("day"); } },
      el("span", { class: "cal-week-dow", text: dateLabel(d, { weekday: "short" }) }),
      el("span", { class: "cal-week-num", text: String(parseYmd(d).d) })));
    }
    const allday = el("div", { class: "cal-allday-row", style: `grid-template-columns:${cols}` }, el("span", { class: "cal-gutter cal-allday-label", text: "All day" }));
    for (const d of days) {
      allday.append(el("div", { class: "cal-allday-cell", role: "list", "aria-label": "All-day events, " + longDay(d), dataset: { date: d } },
        allDayOn(d).map((o) => el("div", { role: "listitem" }, chip(o, "cal-allday-chip")))));
    }
    const grid = el("div", { class: "cal-grid", id: "cal-grid", style: `grid-template-columns:${cols}` });
    const hours = el("div", { class: "cal-hours", "aria-hidden": "true" });
    for (let h = 0; h < 24; h++) hours.append(el("span", { class: "cal-hour", style: `top:${h * HOUR_PX}px`, text: h ? pad(h) + ":00" : "" }));
    grid.append(hours);
    for (const d of days) {
      const col = el("div", { class: "cal-day-col", role: "list", "aria-label": longDay(d), dataset: { date: d, today: String(d === today) },
        onclick: (e) => { if (e.target === col) createAt(d, e.offsetY); } });
      for (const seg of layoutColumns(segmentsFor(d))) {
        const o = seg.o;
        const width = 100 / seg.cols;
        const item = el("div", { role: "listitem", class: eventClasses(o), dataset: { color: colorOf(o), occurrence: o.occurrence_id || "", event: o.event_id, startMinute: String(seg.top), endMinute: String(seg.bottom), col: String(seg.col), cols: String(seg.cols) },
          style: `top:${(seg.top / 60) * HOUR_PX}px;height:${Math.max(((seg.bottom - seg.top) / 60) * HOUR_PX - 2, 16)}px;left:calc(${seg.col * width}% + 1px);width:calc(${width}% - 3px)` },
        el("button", { type: "button", class: "cal-event-open", "aria-label": (o.title || "(No title)") + ", " + whenLabel(o) + (rsvpNote(o) ? ", " + rsvpNote(o) : ""),
          onclick: () => openDetail(o.event_id, o.occurrence_id) },
        el("span", { class: "cal-event-title", text: o.title || "(No title)" }),
        el("span", { class: "cal-event-time", text: timeLabel(o.starts_at) + " – " + timeLabel(o.ends_at) + (rsvpNote(o) ? " · " + rsvpNote(o) : "") }),
        o.location ? el("span", { class: "cal-event-place", text: o.location }) : null),
        joinable(o) ? joinButton(o, "cal-event-join") : null);
        col.append(item);
      }
      if (d === today) col.append(el("div", { class: "cal-now", id: "cal-now", "aria-hidden": "true", style: `top:${(wallMinutes(now()) / 60) * HOUR_PX}px` }));
      grid.append(col);
    }
    const scroller = el("div", { class: "cal-grid-scroll", id: "cal-grid-scroll", tabindex: "0", "aria-label": "Time grid" }, grid);
    wrap.append(heads, allday, scroller);
    return wrap;
  }
  function renderNow() {
    const line = $("cal-now");
    if (line) line.style.top = (wallMinutes(now()) / 60) * HOUR_PX + "px";
    else if (isOpen() && (C.view === "week" || C.view === "day")) renderBody();
  }
  function createAt(ymd, offsetY) {
    const minutes = Math.max(0, Math.min(1410, Math.floor((offsetY / HOUR_PX) * 2) * 30));
    openCreate({ date: ymd, start: pad(Math.floor(minutes / 60)) + ":" + pad(minutes % 60), tz: zone() });
  }

  // ---- month ---------------------------------------------------------------------------------------

  function monthView() {
    const days = visibleDays();
    const today = todayYmd();
    const month = C.cursor.slice(0, 7);
    if (!C.focusDate || !days.includes(C.focusDate)) C.focusDate = days.includes(C.cursor) ? C.cursor : days[0];
    const grid = el("div", { class: "cal-month", role: "grid", "aria-label": dateLabel(C.cursor, { month: "long", year: "numeric" }), onkeydown: monthKeys });
    const head = el("div", { class: "cal-month-row cal-month-head", role: "row" });
    for (let i = 0; i < 7; i++) head.append(el("div", { role: "columnheader", class: "cal-month-dow", text: dateLabel(days[i], { weekday: "short" }) }));
    grid.append(head);
    for (let w = 0; w < 6; w++) {
      const row = el("div", { class: "cal-month-row", role: "row" });
      for (let i = 0; i < 7; i++) {
        const d = days[w * 7 + i];
        const list = [...allDayOn(d), ...timedOn(d).sort((a, b) => a.starts_at - b.starts_at)];
        const cell = el("div", { role: "gridcell", class: "cal-month-cell", tabindex: d === C.focusDate ? "0" : "-1",
          "aria-label": longDay(d) + (list.length ? `, ${list.length} ${list.length === 1 ? "event" : "events"}` : ", no events"),
          "aria-selected": String(d === C.cursor), dataset: { date: d, out: String(d.slice(0, 7) !== month), today: String(d === today) },
          onclick: (e) => { if (e.target.closest(".cal-chip")) return; focusCell(d); },
          ondblclick: (e) => { if (!e.target.closest(".cal-chip")) openCreate({ date: d }); } },
        el("button", { type: "button", class: "cal-month-num", tabindex: "-1", text: String(parseYmd(d).d), "aria-label": "Show " + longDay(d), onclick: (e) => { e.stopPropagation(); C.cursor = d; setView("day"); } }));
        const chips = el("div", { class: "cal-month-chips" });
        for (const o of list.slice(0, 3)) chips.append(chip(o));
        if (list.length > 3) chips.append(el("button", { type: "button", class: "linkish cal-more", text: `+${list.length - 3} more`, onclick: (e) => { e.stopPropagation(); C.cursor = d; setView("day"); } }));
        // Phones get dots instead of chips; the stylesheet decides which to show.
        const dots = el("div", { class: "cal-month-dots", "aria-hidden": "true" }, list.slice(0, 4).map((o) => el("span", { class: "cal-dot", dataset: { color: colorOf(o) } })));
        cell.append(chips, dots);
        row.append(cell);
      }
      grid.append(row);
    }
    return grid;
  }
  function focusCell(d) {
    C.focusDate = d;
    const cells = document.querySelectorAll(".cal-month-cell");
    let target = null;
    for (const c of cells) { const on = c.dataset.date === d; c.tabIndex = on ? 0 : -1; if (on) target = c; }
    if (target) target.focus({ preventScroll: true });
    else { C.cursor = d; renderHead(); renderBody(); refetch(); focusCell(d); }
  }
  function monthKeys(e) {
    const d = C.focusDate;
    if (!d || !e.target.classList.contains("cal-month-cell")) return;
    const moves = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -7, ArrowDown: 7 };
    if (e.key in moves) { e.preventDefault(); focusCell(addDays(d, moves[e.key])); return; }
    if (e.key === "Home") { e.preventDefault(); focusCell(weekStartOf(d)); return; }
    if (e.key === "End") { e.preventDefault(); focusCell(addDays(weekStartOf(d), 6)); return; }
    if (e.key === "PageUp" || e.key === "PageDown") {
      e.preventDefault();
      const next = addMonths(d, e.key === "PageUp" ? -1 : 1);
      const { d: day } = parseYmd(d);
      const target = next.slice(0, 8) + pad(Math.min(day, 28));
      C.cursor = target; renderHead(); renderBody(); refetch(); focusCell(target);
      return;
    }
    if (e.key === "Enter" || e.key === " ") { e.preventDefault(); C.cursor = d; setView("day"); }
  }

  // ---- agenda --------------------------------------------------------------------------------------

  function agendaView() {
    const days = visibleDays();
    const list = el("div", { class: "cal-agenda" });
    let any = false;
    for (const d of days) {
      const items = [...allDayOn(d), ...timedOn(d).filter((o) => wall(o.starts_at).ymd === d || d === days[0]).sort((a, b) => a.starts_at - b.starts_at)];
      if (!items.length) continue;
      any = true;
      const titleId = "cal-agenda-" + d;
      list.append(el("section", { class: "cal-agenda-day", "aria-labelledby": titleId },
        el("h3", { id: titleId, class: "cal-agenda-date", dataset: { today: String(d === todayYmd()) }, text: dateLabel(d, { weekday: "long", day: "numeric", month: "long" }) }),
        el("ul", { class: "cal-agenda-list", role: "list" }, items.map((o) => el("li", { class: "cal-agenda-row " + eventClasses(o), dataset: { color: colorOf(o), occurrence: o.occurrence_id || "" } },
          el("span", { class: "cal-dot", dataset: { color: colorOf(o) }, "aria-hidden": "true" }),
          el("button", { type: "button", class: "cal-agenda-open", onclick: () => openDetail(o.event_id, o.occurrence_id) },
            el("span", { class: "cal-agenda-time", text: o.all_day ? "All day" : timeLabel(o.starts_at) + " – " + timeLabel(o.ends_at) }),
            el("span", { class: "cal-agenda-title", text: o.title || "(No title)" }),
            o.location || rsvpNote(o) ? el("span", { class: "cal-agenda-sub", text: [rsvpNote(o), o.location].filter(Boolean).join(" · ") }) : null),
          joinable(o) ? joinButton(o) : null)))));
    }
    if (!any) list.append(el("p", { class: "empty cal-empty", text: C.loading ? "Loading…" : "Nothing scheduled in the next two weeks." }));
    return list;
  }

  // ---- joining -------------------------------------------------------------------------------------

  async function join(code, occurrenceId, title) {
    const calls = window.PigeonpostCalls;
    if (!calls || !calls.joinScheduled) { host().toast("Calls aren’t available in this browser."); return; }
    const mb = host().mailbox();
    hideSheet("cal-detail-sheet");
    const res = await calls.joinScheduled(code, { occurrenceId: occurrenceId || undefined, as: mb ? mb.address : undefined, title: title || undefined });
    if (res && res.state === "choose_occurrence") chooseOccurrence(code, res.occurrences || [], title);
  }
  function chooseOccurrence(code, list, title) {
    fill("cal-choose-sheet-body",
      el("p", { text: "Two meetings in this series are open right now. Which one are you joining?" }),
      el("ul", { class: "cal-choose-list" }, list.map((o) => el("li", {},
        el("button", { type: "button", class: "btn-ghost", text: whenLabel({ all_day: false, starts_at: o.starts_at, ends_at: o.ends_at }),
          onclick: () => { hideSheet("cal-choose-sheet"); join(code, o.occurrence_id, title); } })))));
    fill("cal-choose-sheet-foot", el("button", { class: "btn-ghost", type: "button", text: "Cancel", onclick: () => hideSheet("cal-choose-sheet") }));
    showSheet("cal-choose-sheet");
  }

  // ---- event detail --------------------------------------------------------------------------------

  async function openDetail(eventId, occurrenceId, extra = {}) {
    fill("cal-detail-sheet-body", el("p", { class: "field-note", text: "Loading…" }));
    fill("cal-detail-sheet-foot");
    showSheet("cal-detail-sheet");
    try {
      const body = await api(eventPath(eventId) + (occurrenceId ? "?occurrence_id=" + encodeURIComponent(occurrenceId) : ""));
      C.detail = { event: body.event, occurrence: body.occurrence || null, saved: extra.saved || null };
      renderDetail();
    } catch (e) {
      C.detail = null;
      fill("cal-detail-sheet-body", el("p", { class: "sheet-error", role: "alert", text: explain(e) }));
    }
  }
  async function reloadDetail() {
    if (!C.detail || $("cal-detail-sheet").hidden) return;
    const { event, occurrence, saved } = C.detail;
    try {
      const body = await api(eventPath(event.id) + (occurrence ? "?occurrence_id=" + encodeURIComponent(occurrence.occurrence_id) : ""));
      C.detail = { event: body.event, occurrence: body.occurrence || null, saved };
      renderDetail();
    } catch (e) {
      if (e.status === 404) { C.detail = null; hideSheet("cal-detail-sheet"); host().toast("That event isn’t available any more."); }
    }
  }

  // Descriptions are plain text; web addresses in them become links. Nothing else is interpreted.
  function linkified(text) {
    const out = el("p", { class: "cd-description" });
    const re = /\bhttps?:\/\/[^\s<>"']+/g;
    let last = 0, m;
    const s = String(text || "");
    while ((m = re.exec(s))) {
      out.append(document.createTextNode(s.slice(last, m.index)));
      const url = m[0].replace(/[.,;:!?)]+$/, "");
      out.append(el("a", { href: url, target: "_blank", rel: "noopener noreferrer", text: url }));
      last = m.index + url.length;
      re.lastIndex = last;
    }
    out.append(document.createTextNode(s.slice(last)));
    return out;
  }

  function countsLine(counts) {
    const c = counts || {};
    const parts = [];
    if (c.accepted) parts.push(`${c.accepted} yes`);
    if (c.tentative) parts.push(`${c.tentative} maybe`);
    if (c.declined) parts.push(`${c.declined} no`);
    if (c.needs_action) parts.push(`${c.needs_action} awaiting`);
    return parts.length ? parts.join(", ") : "No answers yet";
  }

  function describeRule(rrule) {
    if (!rrule) return "";
    const p = parseRule(rrule);
    const n = Number(p.INTERVAL || 1);
    const days = (p.BYDAY || "").split(",").filter(Boolean);
    let s = "Repeats";
    if (p.FREQ === "DAILY") s = n > 1 ? `Every ${n} days` : "Every day";
    else if (p.FREQ === "WEEKLY") {
      if (days.join(",") === "MO,TU,WE,TH,FR" && n === 1) s = "Every weekday";
      else s = (n > 1 ? `Every ${n} weeks` : "Weekly") + (days.length ? " on " + days.map((d) => DAY_NAMES[BYDAY.indexOf(d.slice(-2))]).filter(Boolean).join(", ") : "");
    } else if (p.FREQ === "MONTHLY") s = (n > 1 ? `Every ${n} months` : "Monthly") + (p.BYMONTHDAY ? " on day " + p.BYMONTHDAY : "");
    else if (p.FREQ === "YEARLY") s = n > 1 ? `Every ${n} years` : "Every year";
    if (p.COUNT) s += `, ${p.COUNT} times`;
    else if (p.UNTIL) { const u = p.UNTIL; s += ", until " + dateLabel(`${u.slice(0, 4)}-${u.slice(4, 6)}-${u.slice(6, 8)}`, { day: "numeric", month: "long", year: "numeric" }); }
    return s;
  }

  function renderDetail() {
    const d = C.detail;
    if (!d) return;
    const ev = d.event, occ = d.occurrence;
    const my = ev.my || {};
    const organizer = my.role === "organizer";
    const timing = occ && occ.starts_at != null ? { ...ev, starts_at: occ.starts_at, ends_at: occ.ends_at } : ev;
    const when = timing.all_day ? { all_day: true, start_date: ev.start_date, end_date: ev.end_date } : timing;
    const status = (occ && occ.status) || ev.status;
    const myRsvp = (occ && occ.my_rsvp) || my.rsvp;
    const recurring = Boolean(ev.rrule);
    $("cal-detail-sheet-title").textContent = ev.title || "(No title)";
    const rows = [];
    if (d.saved) rows.push(el("p", { class: "cd-saved", id: "cd-saved", role: "status", text: d.saved }));
    if (status === "cancelled") rows.push(el("p", { class: "cd-cancelled", text: "This event was cancelled." }));
    rows.push(el("p", { class: "cd-when", id: "cd-when", text: whenLabel(when) }));
    if (!ev.all_day && ev.tzid && ev.tzid !== zone() && timing.starts_at != null) {
      rows.push(el("p", { class: "field-note cd-zone", id: "cd-organizer-zone", text: `${timeLabel(timing.starts_at, ev.tzid)} – ${timeLabel(timing.ends_at, ev.tzid)} ${zoneName(ev.tzid)} time (${ev.tzid})` }));
    }
    if (recurring) rows.push(el("p", { class: "field-note", text: describeRule(ev.rrule) }));
    if (ev.location) rows.push(el("p", { class: "cd-location" }, svg(ICON.pin), el("span", { text: ev.location })));
    rows.push(el("p", { class: "field-note cd-organizer", text: "Organizer: " + (organizer ? "you" : (ev.organizer && (ev.organizer.handle || ev.organizer.address)) || "unknown") }));

    if (ev.meeting && ev.meeting.code) {
      const o = { meeting: { code: ev.meeting.code, opens_at: timing.starts_at - 60 * (ev.meeting.join_before_minutes ?? 10), closes_at: timing.ends_at + 1800 }, status, my_rsvp: myRsvp, starts_at: timing.starts_at, ends_at: timing.ends_at };
      const open = joinable(o);
      const url = ev.meeting.url || "https://inbox.pigeonpost.dev/m/" + ev.meeting.code;
      rows.push(el("section", { class: "cd-meeting", "aria-labelledby": "cd-meeting-title" },
        el("h3", { id: "cd-meeting-title" }, svg(ICON.video), el("span", { text: "Pigeonpost meeting" })),
        el("div", { class: "cd-link-row" },
          el("code", { class: "cd-link", id: "cd-link", text: url }),
          el("button", { class: "btn-ghost btn-small", id: "cd-copy-link", type: "button", text: "Copy", onclick: () => copy(url, "Meeting link copied.") }),
          el("button", { class: "btn-primary btn-small", id: "cd-join", type: "button", text: "Join", disabled: !open, onclick: () => join(ev.meeting.code, occ && occ.occurrence_id, ev.title) })),
        el("p", { class: "field-note", text: open ? "The meeting is open." : `Meeting ${codeLabel(ev.meeting.code)} opens ${ev.meeting.join_before_minutes ?? 10} minutes before it starts.` })));
    }

    // Guests. Contact guests are shown to the organizer with their address; to anyone else only by
    // the name the organizer typed, or "Guest" — whatever the response holds.
    const attendees = Array.isArray(ev.attendees) ? ev.attendees : [];
    if (attendees.length) {
      const list = el("ul", { class: "cd-guests", id: "cd-guests" });
      for (const a of attendees) {
        const contact = a.kind === "external";
        const name = contact ? (organizer && a.contact ? (a.name && a.name !== a.contact ? `${a.name} (${a.contact})` : a.contact) : (a.name || "Guest")) : (a.handle || a.name || a.address || "Someone");
        const li = el("li", { class: "cd-guest", dataset: { attendee: a.attendee_id || "", kind: contact ? "contact" : "handle" } },
          contact ? el("span", { class: "cd-guest-icon", title: "Contact address" }, svg(ICON.envelope)) : null,
          el("span", { class: "cd-guest-name", text: name }),
          a.role === "cohost" ? el("span", { class: "pill cd-role", text: "Co-host" }) : null,
          a.optional ? el("span", { class: "field-note", text: "optional" }) : null,
          el("span", { class: "cd-rsvp", dataset: { rsvp: a.rsvp || "needs_action" }, text: RSVP_WORD[a.rsvp] || "Awaiting" }));
        if (organizer && a.delivery) li.append(el("span", { class: "cd-delivery", dataset: { delivery: a.delivery }, text: { queued: "Queued", delivered: "Delivered", failed: "Not delivered", suppressed: "Not sent" }[a.delivery] || a.delivery }));
        if (organizer && contact && a.proof) li.append(el("span", { class: "field-note", text: a.proof === "verified" ? "confirmed" : "answered by link" }));
        if (organizer && contact && a.attendee_id) {
          li.append(el("button", { class: "linkish cd-guest-link", type: "button", text: "Copy invitation link", dataset: { attendee: a.attendee_id },
            onclick: (e) => copyGuestLink(ev, a, e.currentTarget) }));
        }
        list.append(li);
      }
      rows.push(el("section", { class: "cd-section", "aria-labelledby": "cd-guests-title" },
        el("h3", { id: "cd-guests-title", text: "Guests" }),
        el("p", { class: "field-note", id: "cd-counts", text: countsLine(ev.counts) }),
        list,
        organizer && attendees.some((a) => a.kind === "external") && !C.contactInvites
          ? el("p", { class: "field-note cd-not-sent", id: "cd-not-sent", text: "Invitations to contact addresses are not sent yet. Copy each guest’s invitation link and send it yourself." }) : null));
    }
    if (ev.description) rows.push(el("section", { class: "cd-section" }, el("h3", { text: "Description" }), linkified(ev.description)));

    // Your answer.
    if (!organizer && (my.attendee_id != null || myRsvp)) {
      const note = el("input", { type: "text", id: "cd-note", maxlength: "300", placeholder: "Add a note (optional)", "aria-label": "Note to the organizer" });
      const scopeSel = recurring ? el("select", { id: "cd-rsvp-scope", "aria-label": "Answer for" }, el("option", { value: "all", text: "All events in the series" }), el("option", { value: "this", text: "This event only" })) : null;
      const btn = (st, label) => el("button", { type: "button", class: "btn-ghost cd-rsvp-btn", id: "cd-rsvp-" + st, "aria-pressed": String(myRsvp === st), text: label, onclick: () => rsvp(ev, occ, st, note.value, scopeSel ? scopeSel.value : "all") });
      rows.push(el("section", { class: "cd-section", "aria-labelledby": "cd-answer-title" },
        el("h3", { id: "cd-answer-title", text: "Going?" }),
        el("div", { class: "cd-rsvp-row", role: "group", "aria-labelledby": "cd-answer-title" }, btn("accepted", "Yes"), btn("tentative", "Maybe"), btn("declined", "No")),
        scopeSel, note,
        el("p", { class: "sheet-error", id: "cd-rsvp-error", role: "alert", hidden: true })));
    }
    rows.push(el("p", { class: "field-note cd-at-rest", id: "cd-at-rest", text: AT_REST_NOTE }));
    fill("cal-detail-sheet-body", rows);

    const foot = [];
    if (organizer && ev.can_edit !== false && status !== "cancelled") {
      foot.push(el("button", { class: "btn-danger", id: "cd-cancel", type: "button", text: "Cancel event", onclick: () => openCancel(ev, occ) }));
      foot.push(el("span", { class: "foot-spacer" }));
      foot.push(el("button", { class: "btn-ghost", id: "cd-duplicate", type: "button", text: "Duplicate", onclick: () => openCreate(fromEvent(ev, true)) }));
      foot.push(el("button", { class: "btn-primary", id: "cd-edit", type: "button", text: "Edit", onclick: () => openCreate(fromEvent(ev, false, occ)) }));
    } else {
      foot.push(el("button", { class: "btn-ghost", id: "cd-ics", type: "button", text: "Add to my calendar app", onclick: () => downloadIcs(ev) }));
      if (!organizer && ev.organizer) foot.push(el("button", { class: "btn-ghost", id: "cd-message", type: "button", text: "Message organizer", onclick: () => { hideSheet("cal-detail-sheet"); host().openConversation(ev.organizer.handle || ev.organizer.address); } }));
      if (organizer) foot.push(el("button", { class: "btn-ghost", id: "cd-duplicate", type: "button", text: "Duplicate", onclick: () => openCreate(fromEvent(ev, true)) }));
    }
    if (organizer && status !== "cancelled") foot.unshift(el("button", { class: "btn-ghost", id: "cd-ics", type: "button", text: "Add to my calendar app", onclick: () => downloadIcs(ev) }));
    if (ev.linked_group && window.PigeonpostGroups && window.PigeonpostGroups._open) {
      foot.push(el("button", { class: "btn-ghost", id: "cd-chat", type: "button", text: "Open the meeting chat", onclick: () => { hideSheet("cal-detail-sheet"); close(); window.PigeonpostGroups._open(ev.linked_group.id || ev.linked_group); } }));
    }
    fill("cal-detail-sheet-foot", foot);
  }

  async function copy(text, done) {
    try { await navigator.clipboard.writeText(text); host().toast(done); }
    catch (_) { host().toast("Select the text and copy it."); }
  }

  async function copyGuestLink(ev, a, button) {
    button.disabled = true;
    try {
      const body = await api(eventPath(ev.id, "/attendees/" + encodeURIComponent(a.attendee_id) + "/link"), { method: "POST", body: {} });
      const row = button.closest("li");
      row.querySelector(".cd-guest-url")?.remove();
      row.append(el("input", { class: "cd-guest-url", type: "text", readonly: true, value: body.url, "aria-label": "Invitation link for " + (a.name || "this guest") }));
      await copy(body.url, "Invitation link copied. It works for this guest only.");
    } catch (e) {
      host().toast(explain(e));
    } finally { button.disabled = false; }
  }

  async function rsvp(ev, occ, status, note, scope) {
    const body = { status, scope: scope === "this" ? "this" : "all" };
    if (note && note.trim()) body.note = note.trim().slice(0, 300);
    if (body.scope === "this" && occ) body.occurrence_id = occ.occurrence_id;
    for (const b of document.querySelectorAll(".cd-rsvp-btn")) b.disabled = true;
    try {
      await api(eventPath(ev.id, "/rsvp"), { method: "POST", body });
      host().toast(status === "declined" ? "You declined. You can change your answer later." : "Answer sent.");
      loadInvitations();
      refetch();
      reloadDetail();
    } catch (e) {
      showError("cd-rsvp-error", explain(e));
      for (const b of document.querySelectorAll(".cd-rsvp-btn")) b.disabled = false;
    }
  }

  async function downloadIcs(ev) {
    try {
      const text = await api(eventPath(ev.id) + ".ics", { raw: true });
      saveFile(text, (ev.title || "event").replace(/[^\w\- ]+/g, "").trim().slice(0, 60) || "event");
    } catch (e) {
      host().toast("Couldn’t download the event: " + explain(e));
    }
  }
  function saveFile(text, name) {
    const blob = new Blob([text], { type: "text/calendar" });
    const url = URL.createObjectURL(blob);
    const a = el("a", { href: url, download: name + ".ics", hidden: true });
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  // ---- cancelling -----------------------------------------------------------------------------------

  function openCancel(ev, occ) {
    const recurring = Boolean(ev.rrule);
    const radios = recurring ? el("fieldset", { class: "field" }, el("legend", { text: "Cancel" }),
      scopeRadio("cc-scope", "this", "This event", true), scopeRadio("cc-scope", "following", "This and following events"), scopeRadio("cc-scope", "all", "All events")) : null;
    fill("cal-cancel-sheet-body",
      el("p", { text: (ev.attendees || []).length ? "Guests are told the event is cancelled." : "The event is removed from your calendar." }),
      radios,
      (ev.attendees || []).length ? el("label", { class: "field" }, el("span", { text: "Message to guests (optional)" }), el("textarea", { id: "cc-message", rows: "2", maxlength: "500" })) : null,
      el("p", { class: "sheet-error", id: "cc-error", role: "alert", hidden: true }));
    fill("cal-cancel-sheet-foot",
      el("button", { class: "btn-ghost", type: "button", text: "Keep event", onclick: () => hideSheet("cal-cancel-sheet") }),
      el("button", { class: "btn-danger", id: "cc-confirm", type: "button", text: "Cancel event", onclick: async (e) => {
        const btn = e.currentTarget;
        const scope = recurring ? document.querySelector('input[name="cc-scope"]:checked').value : "all";
        const body = { expected_revision: ev.revision, scope };
        if (scope !== "all" && occ) body.occurrence_id = occ.occurrence_id;
        const msg = $("cc-message") && $("cc-message").value.trim();
        if (msg) body.message = msg;
        btn.disabled = true;
        try {
          await api(eventPath(ev.id, "/cancel"), { method: "POST", body });
          hideSheet("cal-cancel-sheet");
          hideSheet("cal-detail-sheet");
          host().toast("Event cancelled.");
          refetch();
        } catch (err) {
          showError("cc-error", explain(err));
        } finally { btn.disabled = false; }
      } }));
    showSheet("cal-cancel-sheet");
  }
  function scopeRadio(name, value, label, checked) {
    return el("label", { class: "cal-radio" }, el("input", { type: "radio", name, value, checked: Boolean(checked) }), el("span", { text: label }));
  }

  // ---- invitations ------------------------------------------------------------------------------------

  async function openInvites() {
    renderInvites();
    showSheet("cal-invites-sheet");
    await loadFiltered();
    renderInvites();
  }
  function inviteCard(inv, filtered) {
    const when = whenLabel(inv.all_day ? inv : { all_day: false, starts_at: inv.starts_at, ends_at: inv.ends_at });
    const answer = async (status, btn) => {
      btn.disabled = true;
      try {
        await api(eventPath(inv.event_id, "/rsvp"), { method: "POST", body: { status, scope: "all" } });
        C.invitations = C.invitations.filter((x) => x.event_id !== inv.event_id);
        C.filtered = C.filtered.filter((x) => x.event_id !== inv.event_id);
        renderInvites(); renderBanner(); syncNav();
        loadInvitations();
        refetch();
      } catch (e) { host().toast(explain(e)); btn.disabled = false; }
    };
    const b = (status, label) => el("button", { type: "button", class: "btn-ghost btn-small", dataset: { rsvp: status }, text: label, onclick: (e) => answer(status, e.currentTarget) });
    return el("li", { class: "cal-invite", dataset: { event: inv.event_id } },
      el("button", { type: "button", class: "cal-invite-open", onclick: () => openDetail(inv.event_id, inv.occurrence_id) },
        el("span", { class: "cal-invite-title", text: inv.title || "(No title)" }),
        el("span", { class: "field-note", text: when + (inv.recurring ? " · repeats" : "") }),
        el("span", { class: "field-note", text: "From " + ((inv.organizer && (inv.organizer.handle || inv.organizer.address)) || "someone") + (inv.meeting ? " · Pigeonpost meeting" : "") })),
      el("div", { class: "cal-invite-actions", role: "group", "aria-label": "Answer" }, b("accepted", "Yes"), b("tentative", "Maybe"), b("declined", "No")),
      filtered ? el("p", { class: "field-note", text: "Answering moves it out of Filtered. It doesn’t make the organizer a contact." }) : null);
  }
  function renderInvites() {
    if (!$("cal-invites-sheet-body")) return;
    const pending = C.invitations;
    fill("cal-invites-sheet-body",
      pending.length ? el("ul", { class: "cal-invites", id: "cal-invites-list" }, pending.map((inv) => inviteCard(inv, false))) : el("p", { class: "field-note", text: "No invitations waiting." }),
      el("details", { class: "cal-filtered", id: "cal-filtered" },
        el("summary", { text: `Filtered (${C.filtered.length})` }),
        el("p", { class: "field-note", text: "Invitations your filtering settings held back. No reminders, and not on your calendar until you answer. Organizers are not told." }),
        C.filtered.length ? el("ul", { class: "cal-invites", id: "cal-filtered-list" }, C.filtered.map((inv) => inviteCard(inv, true))) : el("p", { class: "field-note", text: "Nothing filtered." })));
    fill("cal-invites-sheet-foot", el("button", { class: "btn-primary", type: "button", text: "Done", onclick: () => hideSheet("cal-invites-sheet") }));
  }

  // ---- create and edit -------------------------------------------------------------------------------

  function nextHalfHour(z) {
    const w = wall(now(), z);
    let minutes = Math.ceil((w.h * 60 + w.mi + 1) / 30) * 30;
    let date = w.ymd;
    if (minutes >= 1440) { minutes -= 1440; date = addDays(date, 1); }
    return { date, start: pad(Math.floor(minutes / 60)) + ":" + pad(minutes % 60) };
  }
  const toMinutes = (hhmm) => { const [h, m] = String(hhmm).split(":").map(Number); return h * 60 + m; };
  const fromMinutes = (n) => pad(Math.floor(((n % 1440) + 1440) % 1440 / 60)) + ":" + pad(((n % 60) + 60) % 60);

  function parseRule(rrule) {
    const out = {};
    for (const part of String(rrule || "").replace(/^RRULE:/i, "").split(";")) {
      const [k, v] = part.split("=");
      if (k && v != null) out[k.toUpperCase()] = v;
    }
    return out;
  }

  // The form's starting values, from an event (edit, duplicate) or from nothing (new).
  function fromEvent(ev, duplicate, occ) {
    const all = Boolean(ev.all_day);
    const useOcc = !duplicate && occ && occ.starts_at != null && ev.rrule;
    let date, start, end, endDate;
    if (all) { date = ev.start_date; endDate = addDays(ev.end_date, -1); start = "09:00"; end = "09:30"; }
    else if (useOcc) {
      const a = wall(occ.starts_at, ev.tzid), b = wall(occ.ends_at, ev.tzid);
      date = a.ymd; start = pad(a.h) + ":" + pad(a.mi); end = pad(b.h) + ":" + pad(b.mi); endDate = b.ymd;
    } else {
      [date, start] = String(ev.start).split("T");
      const e = String(ev.end).split("T");
      endDate = e[0]; end = e[1];
    }
    const my = ev.my || {};
    return {
      mode: duplicate ? "create" : "edit", event: duplicate ? null : ev, occurrence: duplicate ? null : occ,
      title: ev.title || "", date, start: (start || "09:00").slice(0, 5), end: (end || "09:30").slice(0, 5), endDate, tz: ev.tzid || zone(), allDay: all,
      location: ev.location || "", description: ev.description || "", rrule: ev.rrule || null,
      calendarId: my.calendar_id || ev.calendar_id || "", transparency: ev.transparency || "opaque", privacy: ev.privacy || "default",
      reminder: Array.isArray(ev.reminders) && ev.reminders.length ? String(ev.reminders[0].minutes_before) : "",
      meeting: ev.meeting && ev.meeting.code ? { ...ev.meeting, enabled: true } : { enabled: false },
      chips: (ev.attendees || []).map((a) => a.kind === "external"
        ? { kind: "contact", value: a.contact || "", name: a.name || "", id: duplicate ? null : a.attendee_id }
        : { kind: "handle", value: a.handle || a.address, cohost: a.role === "cohost", optional: Boolean(a.optional), admitter: (ev.meeting && ev.meeting.admitters || []).includes(a.address), id: duplicate ? null : a.attendee_id })
        .filter((c) => c.value),
    };
  }

  function openCreate(prefill = {}) {
    if (!C.enabled) return;
    const tz = prefill.tz || browserZone();
    const nh = nextHalfHour(tz);
    const start = prefill.start || nh.start;
    const f = Object.assign({
      mode: "create", event: null, occurrence: null, title: "", date: prefill.date || nh.date, start, end: fromMinutes(toMinutes(start) + 30), endDate: null, tz,
      allDay: false, location: "", description: "", rrule: null, calendarId: (C.calendars.find((c) => c.is_default) || {}).id || "",
      transparency: "opaque", privacy: "default", reminder: "10", meeting: { enabled: false }, chips: [],
    }, prefill);
    if (!f.endDate) f.endDate = toMinutes(f.end) <= toMinutes(f.start) ? addDays(f.date, 1) : f.date;
    f.meetingTouched = f.mode === "edit" || Boolean(prefill.meeting);
    f.fold = null;
    f.requestKey = key();
    C.form = f;
    buildForm();
    $("cal-edit-sheet-title").textContent = f.mode === "edit" ? "Edit event" : "New event";
    showSheet("cal-edit-sheet", "ce-title");
  }

  let zoneList = null;
  function zones() {
    if (!zoneList) {
      try { zoneList = typeof Intl.supportedValuesOf === "function" ? Intl.supportedValuesOf("timeZone") : []; } catch (_) { zoneList = []; }
      if (!zoneList.includes("UTC")) zoneList = zoneList.concat("UTC");
    }
    return zoneList;
  }
  function zoneSelect(id, value, firstLabel) {
    const s = el("select", { id });
    if (firstLabel) s.append(el("option", { value: "", text: firstLabel }));
    const list = zones().slice();
    if (value && !list.includes(value)) list.unshift(value);
    const bz = browserZone();
    if (!list.includes(bz)) list.unshift(bz);
    for (const z of list) s.append(el("option", { value: z, text: z.replace(/_/g, " ") }));
    s.value = value || "";
    return s;
  }
  function seg(name, options, value, label) {
    return el("div", { class: "segmented", role: "radiogroup", "aria-label": label },
      options.map(([v, text]) => el("label", {}, el("input", { type: "radio", name, value: v, checked: v === value }), el("span", { text }))));
  }
  const radioValue = (name) => { const r = document.querySelector(`input[name="${name}"]:checked`); return r ? r.value : null; };

  function buildForm() {
    const f = C.form;
    const wd = weekday(f.date);
    const repeat = repeatChoice(f.rrule, f.date);
    const rule = parseRule(f.rrule);
    const remind = el("select", { id: "ce-reminder" },
      [["", "No reminder"], ["0", "At the start"], ["5", "5 minutes before"], ["10", "10 minutes before"], ["15", "15 minutes before"], ["30", "30 minutes before"], ["60", "1 hour before"], ["1440", "1 day before"]]
        .map(([v, t]) => el("option", { value: v, text: t })));
    remind.value = f.reminder;
    const cal = el("select", { id: "ce-calendar" });
    if (!C.calendars.length) cal.append(el("option", { value: "", text: "Calendar" }));
    for (const c of C.calendars) cal.append(el("option", { value: c.id, text: c.name || "Calendar" }));
    cal.value = f.calendarId || (C.calendars[0] ? C.calendars[0].id : "");
    const m = f.meeting || {};
    const contacts = (host().contacts() || []).filter((c) => c && c.peer && c.peer.startsWith("/") && c.admission !== "block");
    const dayBoxes = BYDAY.map((code, i) => ({ code, i })).sort((a, b) => ((a.i + 6) % 7) - ((b.i + 6) % 7))
      .map(({ code, i }) => el("label", { class: "cal-day-box" }, el("input", { type: "checkbox", id: "ce-byday-" + code, value: code, checked: (rule.BYDAY ? rule.BYDAY.split(",").includes(code) : i === wd) }), el("span", { text: DAY_NAMES[i].slice(0, 3) })));
    const endMode = rule.COUNT ? "count" : rule.UNTIL ? "until" : "never";
    const untilVal = rule.UNTIL ? `${rule.UNTIL.slice(0, 4)}-${rule.UNTIL.slice(4, 6)}-${rule.UNTIL.slice(6, 8)}` : addDays(f.date, 90);

    fill("cal-edit-sheet-body",
      el("label", { class: "field" }, el("span", { text: "Title" }), el("input", { type: "text", id: "ce-title", maxlength: "200", value: f.title, placeholder: "Design review" })),
      el("div", { class: "ce-when" },
        el("label", { class: "field" }, el("span", { text: "Date" }), el("input", { type: "date", id: "ce-date", value: f.date, onchange: () => refreshRepeatLabels() })),
        el("label", { class: "field ce-timed" }, el("span", { text: "Start" }), el("input", { type: "time", id: "ce-start", value: f.start, step: "300", onchange: (e) => { const end = $("ce-end"); end.value = fromMinutes(toMinutes(e.target.value) + (C.form.duration || 30)); } })),
        el("label", { class: "field ce-timed" }, el("span", { text: "End" }), el("input", { type: "time", id: "ce-end", value: f.end, step: "300", onchange: () => { C.form.duration = (toMinutes($("ce-end").value) - toMinutes($("ce-start").value) + 1440) % 1440 || 1440; } })),
        el("label", { class: "field ce-allday-only", hidden: true }, el("span", { text: "Last day" }), el("input", { type: "date", id: "ce-end-date", value: f.allDay ? f.endDate : f.date }))),
      el("label", { class: "field ce-timed" }, el("span", { text: "Time zone" }), zoneSelect("ce-tz", f.tz)),
      el("label", { class: "peer-toggle ce-meeting-row", for: "ce-meeting" },
        el("span", {}, svg(ICON.video), el("span", { text: "Pigeonpost meeting" })),
        el("input", { type: "checkbox", id: "ce-meeting", role: "switch", checked: Boolean(m.enabled), onchange: () => { C.form.meetingTouched = true; syncForm(); } })),
      el("p", { class: "field-note", id: "ce-meeting-note" }),
      el("div", { class: "field" },
        el("span", { id: "ce-guests-label", text: "Guests" }),
        el("div", { class: "chip-row ce-chips", id: "ce-chips", role: "list", "aria-labelledby": "ce-guests-label" }),
        el("div", { class: "chip-input" },
          el("input", { type: "text", id: "ce-guest", list: "ce-contacts", autocomplete: "off", autocapitalize: "off", spellcheck: "false", placeholder: "/handle or a contact address", "aria-label": "Add a guest by handle or contact address",
            onkeydown: (e) => { if (e.key === "Enter" || e.key === ",") { e.preventDefault(); addGuest(); } } }),
          el("button", { type: "button", class: "btn-ghost", id: "ce-guest-add", text: "Add", onclick: () => addGuest() })),
        el("datalist", { id: "ce-contacts" }, contacts.map((c) => el("option", { value: c.peer, label: c.alias || c.peer }))),
        el("p", { class: "field-note", id: "ce-contact-note", hidden: true })),
      el("details", { class: "ce-more", id: "ce-more", open: f.mode === "edit" || Boolean(f.rrule) || f.allDay ? true : null },
        el("summary", { text: "More options" }),
        el("label", { class: "field" }, el("span", { text: "Repeat" }), el("select", { id: "ce-repeat", onchange: () => syncForm() },
          el("option", { value: "none", text: "Does not repeat" }), el("option", { value: "daily", text: "Daily" }), el("option", { value: "weekdays", text: "Every weekday (Monday to Friday)" }),
          el("option", { value: "weekly", id: "ce-repeat-weekly", text: "Weekly on " + DAY_NAMES[wd] }), el("option", { value: "monthly", id: "ce-repeat-monthly", text: "Monthly on day " + parseYmd(f.date).d }),
          el("option", { value: "custom", text: "Custom…" }))),
        el("fieldset", { class: "field ce-custom", id: "ce-custom", hidden: true }, el("legend", { text: "Custom repeat" }),
          el("div", { class: "ce-inline" },
            el("span", { text: "Every" }),
            el("input", { type: "number", id: "ce-interval", min: "1", max: "1000", value: rule.INTERVAL || "1", "aria-label": "Interval" }),
            el("select", { id: "ce-freq", "aria-label": "Unit", onchange: () => syncForm() },
              el("option", { value: "DAILY", text: "day(s)" }), el("option", { value: "WEEKLY", text: "week(s)" }), el("option", { value: "MONTHLY", text: "month(s)" }), el("option", { value: "YEARLY", text: "year(s)" }))),
          el("div", { class: "ce-days", id: "ce-days", role: "group", "aria-label": "On these days" }, dayBoxes),
          el("div", { class: "ce-ends", role: "radiogroup", "aria-label": "Ends" },
            el("label", { class: "cal-radio" }, el("input", { type: "radio", name: "ce-ends", value: "never", checked: endMode === "never" }), el("span", { text: "Never" })),
            el("label", { class: "cal-radio" }, el("input", { type: "radio", name: "ce-ends", value: "count", checked: endMode === "count" }), el("span", { text: "After" }),
              el("input", { type: "number", id: "ce-count", min: "1", max: "1000", value: rule.COUNT || "10", "aria-label": "Number of times" }), el("span", { text: "times" })),
            el("label", { class: "cal-radio" }, el("input", { type: "radio", name: "ce-ends", value: "until", checked: endMode === "until" }), el("span", { text: "On" }),
              el("input", { type: "date", id: "ce-until", value: untilVal, "aria-label": "Last date" })))),
        el("label", { class: "peer-toggle", for: "ce-allday" }, el("span", { text: "All day" }), el("input", { type: "checkbox", id: "ce-allday", checked: f.allDay, onchange: () => syncForm() })),
        el("label", { class: "field" }, el("span", { text: "Location" }), el("input", { type: "text", id: "ce-location", maxlength: "500", value: f.location })),
        el("label", { class: "field" }, el("span", { text: "Description" }), el("textarea", { id: "ce-description", rows: "3", maxlength: "8000" }, f.description)),
        el("label", { class: "field" }, el("span", { text: "Reminder" }), remind),
        el("label", { class: "field" }, el("span", { text: "Calendar" }), cal),
        el("div", { class: "field" }, el("span", { text: "Show as" }), seg("ce-transp", [["opaque", "Busy"], ["transparent", "Free"]], f.transparency, "Show as")),
        el("label", { class: "peer-toggle", for: "ce-private" }, el("span", { text: "Private details" }), el("input", { type: "checkbox", id: "ce-private", checked: f.privacy === "private" })),
        el("fieldset", { class: "field ce-meeting-options", id: "ce-meeting-options" }, el("legend", { text: "Meeting options" }),
          el("label", { class: "field" }, el("span", { text: "Who waits in the lobby" }), selectOf("ce-waiting", [["guests", "Guests (contact addresses and unconfirmed people)"], ["everyone_except_hosts", "Everyone except hosts"], ["none", "Nobody invited"]], m.waiting || "guests", () => {})),
          el("label", { class: "field" }, el("span", { text: "Who can let people in" }), selectOf("ce-admit", [["organizer", "Only me"], ["hosts", "Me and co-hosts"], ["delegated", "Me, co-hosts and chosen guests"]], m.admit || "hosts", () => renderChips())),
          el("label", { class: "peer-toggle", for: "ce-knock" }, el("span", { text: "Let people with the link ask to join" }), el("input", { type: "checkbox", id: "ce-knock", checked: Boolean(m.allow_knock) })),
          el("div", { class: "field" }, el("span", { text: "Call type" }), seg("ce-transport", [["mesh", "Small call (up to 6)"], ["sfu", "Large call (up to 30)"]], m.transport || "mesh", "Call type")),
          el("p", { class: "field-note", id: "ce-capacity", role: "status" }))),
      el("div", { class: "ce-fold", id: "ce-fold", role: "alert", hidden: true },
        el("p", { text: "That time happens twice on that day, because the clocks go back an hour. Which one do you mean?" }),
        el("div", { class: "gi-actions" },
          el("button", { type: "button", class: "btn-ghost", id: "ce-fold-earlier", text: "The first one (earlier)", onclick: () => { C.form.fold = "earlier"; submitForm(); } }),
          el("button", { type: "button", class: "btn-ghost", id: "ce-fold-later", text: "The second one (later)", onclick: () => { C.form.fold = "later"; submitForm(); } }))),
      el("p", { class: "sheet-error", id: "ce-error", role: "alert", hidden: true }));
    $("ce-repeat").value = repeat;
    if (rule.FREQ) $("ce-freq").value = rule.FREQ;
    for (const input of document.querySelectorAll('input[name="ce-transport"]')) input.addEventListener("change", () => syncForm());
    fill("cal-edit-sheet-foot",
      el("button", { class: "btn-ghost", type: "button", text: "Cancel", onclick: () => hideSheet("cal-edit-sheet") }),
      el("button", { class: "btn-primary", id: "ce-save", type: "button", text: "Save", onclick: () => { C.form.fold = null; submitForm(); } }));
    renderChips();
    syncForm();
  }
  function selectOf(id, options, value, onchange) {
    const s = el("select", { id, onchange }, options.map(([v, t]) => el("option", { value: v, text: t })));
    s.value = value;
    return s;
  }
  function repeatChoice(rrule, date) {
    if (!rrule) return "none";
    const wd = BYDAY[weekday(date)], md = parseYmd(date).d;
    if (rrule === "FREQ=DAILY") return "daily";
    if (rrule === "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR") return "weekdays";
    if (rrule === "FREQ=WEEKLY;BYDAY=" + wd) return "weekly";
    if (rrule === "FREQ=MONTHLY;BYMONTHDAY=" + md) return "monthly";
    return "custom";
  }
  function refreshRepeatLabels() {
    const date = $("ce-date").value;
    if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) return;
    $("ce-repeat-weekly").textContent = "Weekly on " + DAY_NAMES[weekday(date)];
    $("ce-repeat-monthly").textContent = "Monthly on day " + parseYmd(date).d;
  }

  function syncForm() {
    const f = C.form;
    const allDay = $("ce-allday").checked;
    for (const n of document.querySelectorAll("#cal-edit-sheet .ce-timed")) n.hidden = allDay;
    for (const n of document.querySelectorAll("#cal-edit-sheet .ce-allday-only")) n.hidden = !allDay;
    const meeting = $("ce-meeting");
    if (allDay) { meeting.checked = false; meeting.disabled = true; }
    else {
      meeting.disabled = false;
      if (!f.meetingTouched) meeting.checked = f.chips.length > 0;
    }
    $("ce-meeting-note").textContent = allDay ? "All-day events can’t be meetings. Make a separate timed event for the call."
      : meeting.checked ? (f.event && f.event.meeting && f.event.meeting.url ? "Meeting link: " + f.event.meeting.url : "A meeting link is made when you save.") : "";
    $("ce-meeting-options").hidden = !meeting.checked;
    const repeat = $("ce-repeat").value;
    $("ce-custom").hidden = repeat !== "custom";
    $("ce-days").hidden = $("ce-freq").value !== "WEEKLY";
    // The capacity hint: the organizer plus every guest who is not optional must fit.
    const people = 1 + f.chips.filter((c) => !c.optional).length;
    const transport = radioValue("ce-transport") || "mesh";
    const limit = transport === "sfu" ? 30 : 6;
    $("ce-capacity").textContent = people > limit
      ? `${people} people with you — too many for a ${transport === "sfu" ? "large" : "small"} call (up to ${limit}).${transport === "mesh" && people <= 30 ? " Choose Large call." : ""}`
      : `${people} ${people === 1 ? "person" : "people"} with you. A small call holds up to 6 people; a large call up to 30, and needs browsers that encrypt their own audio and video.`;
    renderContactNote();
  }

  function renderChips() {
    const row = $("ce-chips");
    if (!row) return;
    const f = C.form;
    const delegated = $("ce-admit") && $("ce-admit").value === "delegated";
    row.textContent = "";
    for (const c of f.chips) {
      const label = c.kind === "contact" ? (c.name ? `${c.name} <${c.value}>` : c.value) : c.value;
      const chipEl = el("span", { class: "chip ce-chip", role: "listitem", dataset: { kind: c.kind } },
        c.kind === "contact" ? el("span", { class: "ce-chip-icon", title: C.contactInvites ? "Will get an invitation at this address" : "Invitation link to share yourself" }, svg(ICON.envelope)) : null,
        el("span", { text: label }));
      if (c.kind === "handle") {
        chipEl.append(el("button", { type: "button", class: "ce-chip-toggle", "aria-pressed": String(Boolean(c.cohost)), text: "Co-host", "aria-label": (c.cohost ? "Remove co-host: " : "Make co-host: ") + c.value,
          onclick: () => { c.cohost = !c.cohost; renderChips(); } }));
        if (delegated) chipEl.append(el("button", { type: "button", class: "ce-chip-toggle", "aria-pressed": String(Boolean(c.admitter)), text: "Can let in", "aria-label": "Can let people in: " + c.value,
          onclick: () => { c.admitter = !c.admitter; renderChips(); } }));
      }
      chipEl.append(el("button", { type: "button", class: "chip-x", "aria-label": "Remove " + label, text: "×", onclick: () => { f.chips = f.chips.filter((x) => x !== c); renderChips(); syncForm(); } }));
      row.append(chipEl);
    }
  }
  function renderContactNote() {
    const note = $("ce-contact-note");
    if (!note) return;
    const any = C.form.chips.some((c) => c.kind === "contact");
    note.hidden = !any;
    note.textContent = C.contactInvites ? "Will get an invitation at this address." : "Invitation link to share yourself: invitations to contact addresses are not sent yet. After saving, copy each guest’s link from the event.";
  }

  function addGuest() {
    const input = $("ce-guest");
    let v = input.value.trim().replace(/,$/, "").trim();
    if (!v) return;
    const f = C.form;
    let chipValue;
    const named = /^(.*)<([^<>\s]+@[^<>\s]+)>$/.exec(v);
    if (named || (v.includes("@") && !v.startsWith("/"))) {
      const contact = named ? named[2] : v;
      if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(contact)) { showError("ce-error", "That contact address doesn’t look right."); return; }
      chipValue = { kind: "contact", value: contact.toLowerCase(), name: named ? named[1].trim().replace(/^"|"$/g, "") : "" };
    } else {
      if (!v.startsWith("/")) v = "/" + v;
      if (!/^\/[A-Za-z0-9._~\-/]{1,200}$/.test(v)) { showError("ce-error", "That doesn’t look like a Pigeonpost handle or a contact address."); return; }
      chipValue = { kind: "handle", value: v, cohost: false };
    }
    if (!f.chips.some((c) => c.kind === chipValue.kind && c.value === chipValue.value)) f.chips.push(chipValue);
    input.value = "";
    showError("ce-error", "");
    renderChips();
    syncForm();
  }

  function untilStamp(date, allDay, tz) {
    const { y, m, d } = parseYmd(date);
    if (allDay) return `${y}${pad(m)}${pad(d)}`;
    const t = new Date(zonedToUnix(y, m, d, 23, 59, tz, "later") * 1000 + 59000);
    return `${t.getUTCFullYear()}${pad(t.getUTCMonth() + 1)}${pad(t.getUTCDate())}T${pad(t.getUTCHours())}${pad(t.getUTCMinutes())}${pad(t.getUTCSeconds())}Z`;
  }

  function buildRule(date, allDay, tz) {
    const repeat = $("ce-repeat").value;
    const wd = weekday(date), md = parseYmd(date).d;
    if (repeat === "none") return null;
    if (repeat === "daily") return "FREQ=DAILY";
    if (repeat === "weekdays") return "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR";
    if (repeat === "weekly") return "FREQ=WEEKLY;BYDAY=" + BYDAY[wd];
    if (repeat === "monthly") return "FREQ=MONTHLY;BYMONTHDAY=" + md;
    const freq = $("ce-freq").value;
    const parts = ["FREQ=" + freq];
    const interval = Math.max(1, Math.min(1000, Math.floor(Number($("ce-interval").value) || 1)));
    if (interval > 1) parts.push("INTERVAL=" + interval);
    if (freq === "WEEKLY") {
      const days = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"].filter((d) => $("ce-byday-" + d).checked);
      parts.push("BYDAY=" + (days.length ? days : [BYDAY[wd]]).join(","));
    }
    if (freq === "MONTHLY") parts.push("BYMONTHDAY=" + md);
    const ends = radioValue("ce-ends");
    if (ends === "count") parts.push("COUNT=" + Math.max(1, Math.min(1000, Math.floor(Number($("ce-count").value) || 1))));
    if (ends === "until" && /^\d{4}-\d{2}-\d{2}$/.test($("ce-until").value)) parts.push("UNTIL=" + untilStamp($("ce-until").value, allDay, tz));
    return parts.join(";");
  }

  // The request body, from the form. Throws a message when the form is not complete.
  function formBody() {
    const f = C.form;
    const title = $("ce-title").value.trim();
    const date = $("ce-date").value;
    const allDay = $("ce-allday").checked;
    if (!title) throw new Error("Give the event a title.");
    if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) throw new Error("Choose a date.");
    const body = { title, all_day: allDay, transparency: radioValue("ce-transp") || "opaque", privacy: $("ce-private").checked ? "private" : "default", locale: "en" };
    const tz = $("ce-tz").value || browserZone();
    if (allDay) {
      const last = $("ce-end-date").value && $("ce-end-date").value >= date ? $("ce-end-date").value : date;
      body.start_date = date;
      body.end_date = addDays(last, 1);
    } else {
      const start = $("ce-start").value, end = $("ce-end").value;
      if (!/^\d{2}:\d{2}/.test(start) || !/^\d{2}:\d{2}/.test(end)) throw new Error("Choose a start and an end time.");
      body.start = `${date}T${start.slice(0, 5)}`;
      body.end = `${toMinutes(end) <= toMinutes(start) ? addDays(date, 1) : date}T${end.slice(0, 5)}`;
      body.tzid = tz;
    }
    body.description = $("ce-description").value;
    body.location = $("ce-location").value.trim();
    body.rrule = buildRule(date, allDay, tz);
    if (!body.rrule) delete body.rrule;
    if ($("ce-calendar").value) body.calendar_id = $("ce-calendar").value;
    body.attendees = f.chips.map((c) => c.kind === "handle"
      ? Object.assign({ handle: c.value }, c.cohost ? { role: "cohost" } : {}, c.optional ? { optional: true } : {})
      : Object.assign({ contact: c.value }, c.name ? { name: c.name } : {}));
    if ($("ce-meeting").checked && !allDay) {
      const admit = $("ce-admit").value;
      body.meeting = { enabled: true, waiting: $("ce-waiting").value, admit, allow_knock: $("ce-knock").checked, transport: radioValue("ce-transport") || "mesh" };
      if (admit === "delegated") body.meeting.admitters = f.chips.filter((c) => c.kind === "handle" && c.admitter).map((c) => c.value);
    } else body.meeting = { enabled: false };
    const r = $("ce-reminder").value;
    body.reminders = r === "" ? [] : [{ minutes_before: Number(r), channel: "push" }];
    if (f.fold) body.fold = f.fold;
    return body;
  }

  async function submitForm() {
    const f = C.form;
    let body;
    try { body = formBody(); } catch (e) { showError("ce-error", e.message); return; }
    $("ce-fold").hidden = true;
    if (f.mode === "edit" && f.event && f.event.rrule && !f.scope) { askScope(); return; }
    const save = $("ce-save");
    save.disabled = true;
    showError("ce-error", "");
    try {
      if (f.mode === "create") {
        const res = await api("/v1/calendar/events", { method: "POST", body: Object.assign({ request_key: key() }, body) });
        hideSheet("cal-edit-sheet");
        afterSave(res.event, body.attendees.length ? "Event saved — invitations queued" : "Event saved");
      } else {
        const res = await saveEdit(body);
        hideSheet("cal-edit-sheet");
        afterSave(res.event, body.attendees.length ? "Event saved — updates queued for guests" : "Event saved", res.occurrenceId);
      }
    } catch (e) {
      f.scope = null;
      if (e.code === "time_invalid" && (e.body.kind === "ambiguous" || !e.body.kind)) {
        $("ce-fold").hidden = false;
        $("ce-fold-earlier").focus({ preventScroll: true });
      } else showError("ce-error", explain(e));
    } finally { save.disabled = false; }
  }

  function askScope() {
    fill("cal-scope-sheet-body",
      el("fieldset", { class: "field" }, el("legend", { text: "Change" }),
        scopeRadio("cs-scope", "this", "This event", true), scopeRadio("cs-scope", "following", "This and following events"), scopeRadio("cs-scope", "all", "All events")));
    fill("cal-scope-sheet-foot",
      el("button", { class: "btn-ghost", type: "button", text: "Cancel", onclick: () => hideSheet("cal-scope-sheet") }),
      el("button", { class: "btn-primary", id: "cs-scope-ok", type: "button", text: "Save", onclick: () => { C.form.scope = radioValue("cs-scope") || "this"; hideSheet("cal-scope-sheet"); submitForm(); } }));
    showSheet("cal-scope-sheet");
  }

  // Edits: PATCH the event with its fields, then add or remove guests with the attendees route (the
  // contract keeps the guest list there), then the organizer's own reminders.
  async function saveEdit(body) {
    const f = C.form;
    const ev = f.event;
    const scope = ev.rrule ? (f.scope || "this") : "all";
    const patch = Object.assign({ expected_revision: ev.revision, scope }, body);
    delete patch.attendees;
    delete patch.reminders;
    if (scope !== "all") patch.occurrence_id = f.occurrence ? f.occurrence.occurrence_id : undefined;
    if (scope === "this") delete patch.rrule;
    else if (!patch.rrule && ev.rrule) patch.rrule = null;
    const res = await api(eventPath(ev.id), { method: "PATCH", body: patch });
    let event = res.event || ev;
    const before = new Map((ev.attendees || []).map((a) => [a.attendee_id, a]));
    const kept = new Set(f.chips.filter((c) => c.id).map((c) => c.id));
    const remove = scope === "following" ? [] : [...before.keys()].filter((id) => id && !kept.has(id));
    const add = body.attendees.filter((_, i) => !f.chips[i].id);
    const cohostChanged = f.chips.some((c) => c.id && c.kind === "handle" && (before.get(c.id)?.role === "cohost") !== Boolean(c.cohost));
    if (cohostChanged) {
      for (const c of f.chips) if (c.id && c.kind === "handle" && (before.get(c.id)?.role === "cohost") !== Boolean(c.cohost)) { remove.push(c.id); add.push(Object.assign({ handle: c.value }, c.cohost ? { role: "cohost" } : {})); }
    }
    if (add.length || remove.length) {
      const att = { expected_revision: event.revision, add, remove, scope: scope === "this" ? "this" : "all" };
      if (scope === "this" && f.occurrence) att.occurrence_id = f.occurrence.occurrence_id;
      const r2 = await api(eventPath(event.id, "/attendees"), { method: "POST", body: att });
      event = r2.event || event;
    }
    const oldRem = JSON.stringify(ev.reminders || []), newRem = JSON.stringify(body.reminders);
    if (oldRem !== newRem) await api(eventPath(event.id, "/reminders"), { method: "PUT", body: { reminders: body.reminders } }).catch(() => {});
    return { event, occurrenceId: scope === "this" && f.occurrence ? f.occurrence.occurrence_id : null };
  }

  function afterSave(event, message, occurrenceId) {
    refetch();
    loadCalendars();
    if (event && event.id) openDetail(event.id, occurrenceId || null, { saved: message });
    else host().toast(message);
  }

  // ---- settings: calendars, previews, time zone, feeds, import ------------------------------------------

  async function openSettings() {
    C.feedUrls = new Map();
    if ($("cs-import-preview")) $("cs-import-preview").textContent = "";
    renderSettings();
    showSheet("cal-settings-sheet");
    await loadFeeds();
    renderSettings();
  }
  async function loadFeeds() {
    C.feeds = new Map();
    await Promise.all(C.calendars.map(async (cal) => {
      try { C.feeds.set(cal.id, await api("/v1/calendar/feeds/" + encodeURIComponent(cal.id))); } catch (_) { C.feeds.set(cal.id, { enabled: false }); }
    }));
  }
  function colorSelect(id, value, label, onchange) {
    return selectOf(id, COLORS.map((c) => [c, c[0].toUpperCase() + c.slice(1)]), COLORS.includes(value) ? value : "blue", onchange);
  }
  async function putSettings(patch) {
    const next = Object.assign({}, C.settings, patch);
    try {
      const body = await api("/v1/calendar/settings", { method: "PUT", body: { previews: Boolean(next.previews), timezone: next.timezone || null, week_start: next.week_start ?? 1, working_hours: next.working_hours || undefined } });
      C.settings = Object.assign(next, body.settings || {});
      renderHead();
      renderBody();
      refetch();
    } catch (e) { showError("cs-error", explain(e)); }
  }

  function renderSettings() {
    if (!$("cal-settings-sheet-body")) return;
    const calRows = C.calendars.map((cal) => el("li", { class: "cs-cal", dataset: { calendar: cal.id } },
      el("span", { class: "cal-dot", dataset: { color: cal.color || "blue" }, "aria-hidden": "true" }),
      el("input", { type: "text", class: "cs-cal-name", value: cal.name || "", maxlength: "60", "aria-label": "Name of " + (cal.name || "calendar"),
        onchange: (e) => patchCalendar(cal, { name: e.target.value.trim() || cal.name }) }),
      colorSelect("cs-color-" + cal.id, cal.color, "Color", (e) => patchCalendar(cal, { color: e.target.value })),
      el("button", { type: "button", class: "btn-ghost btn-small", text: "Delete", "aria-label": "Delete " + (cal.name || "calendar"), onclick: () => deleteCalendar(cal) })));
    const feeds = C.calendars.map((cal) => feedRow(cal));
    // A preview in progress survives a redraw (the feed states arrive after the sheet opens).
    const keptPreview = $("cs-import-preview") && $("cs-import-preview").childNodes.length ? $("cs-import-preview") : null;
    const importCal = el("select", { id: "cs-import-cal", "aria-label": "Import into" });
    if (!C.calendars.length) importCal.append(el("option", { value: "", text: "Calendar" }));
    for (const cal of C.calendars) importCal.append(el("option", { value: cal.id, text: cal.name || "Calendar" }));
    fill("cal-settings-sheet-body",
      el("section", { class: "setting" }, el("h3", { text: "Calendars" }),
        calRows.length ? el("ul", { class: "cs-cals" }, calRows) : el("p", { class: "field-note", text: "Your first calendar is created with your first event, or add one here." }),
        el("div", { class: "chip-input" },
          el("input", { type: "text", id: "cs-new-name", maxlength: "60", placeholder: "New calendar name", "aria-label": "New calendar name" }),
          colorSelect("cs-new-color", "green", "Color"),
          el("button", { type: "button", class: "btn-ghost", id: "cs-add", text: "Add", onclick: () => addCalendar() }))),
      el("section", { class: "setting" }, el("h3", { text: "Reminders" }),
        el("label", { class: "peer-toggle", for: "cs-previews" }, el("span", { text: "Show event titles in reminder notifications" }),
          el("input", { type: "checkbox", id: "cs-previews", checked: Boolean(C.settings.previews), onchange: (e) => putSettings({ previews: e.target.checked }) })),
        el("p", { class: "field-note", text: "Off: notifications say only “Event starting soon”." })),
      el("section", { class: "setting" }, el("h3", { text: "Time zone" }),
        el("label", { class: "field" }, el("span", { text: "Show times in" }), (() => {
          const s = zoneSelect("cs-tz", C.settings.timezone || "", "This device (" + browserZone().replace(/_/g, " ") + ")");
          s.addEventListener("change", () => putSettings({ timezone: s.value || null }));
          return s;
        })())),
      el("section", { class: "setting" }, el("h3", { text: "Subscription links" }),
        el("p", { class: "field-note", text: "Let another calendar app follow one of your calendars. Anyone with the link sees what it shows; copies already made in other apps can’t be recalled." }),
        feeds.length ? el("ul", { class: "cs-feeds" }, feeds) : el("p", { class: "field-note", text: "Create a calendar first." })),
      el("section", { class: "setting" }, el("h3", { text: "Import" }),
        el("p", { class: "field-note", text: "Add events from an .ics file. Nothing in the file is fetched from the internet, and no invitations are sent." }),
        el("div", { class: "chip-input" },
          el("input", { type: "file", id: "cs-import-file", accept: ".ics,text/calendar", "aria-label": "Choose an .ics file", onchange: (e) => previewImport(e.target.files && e.target.files[0]) }),
          importCal),
        keptPreview || el("div", { id: "cs-import-preview" })),
      el("p", { class: "field-note", text: AT_REST_NOTE }),
      el("p", { class: "sheet-error", id: "cs-error", role: "alert", hidden: true }));
    fill("cal-settings-sheet-foot", el("button", { class: "btn-primary", type: "button", text: "Done", onclick: () => hideSheet("cal-settings-sheet") }));
  }

  function feedRow(cal) {
    const state = (C.feeds && C.feeds.get(cal.id)) || { enabled: false };
    const shownUrl = C.feedUrls && C.feedUrls.get(cal.id);
    const row = el("li", { class: "cs-feed", dataset: { calendar: cal.id } }, el("strong", { text: cal.name || "Calendar" }));
    if (shownUrl) {
      row.append(
        el("div", { class: "gi-link" },
          el("input", { type: "text", readonly: true, id: "cs-feed-url-" + cal.id, class: "cs-feed-url", value: shownUrl, "aria-label": "Subscription link for " + (cal.name || "calendar") }),
          el("button", { type: "button", class: "btn-ghost btn-small", text: "Copy", onclick: () => copy(shownUrl, "Subscription link copied.") })),
        el("p", { class: "field-note", text: "This link is shown only now. Paste it into the other app. Copies in other calendar apps are not recalled when you reset or turn off the link — they just stop updating." }));
    }
    if (state.enabled) {
      row.append(el("p", { class: "field-note", text: "On" + (state.projection === "busy" ? " — shows only busy times." : " — shows your own titles and locations; private events and invitations as busy.") }),
        el("div", { class: "gi-actions" },
          el("button", { type: "button", class: "btn-ghost btn-small", id: "cs-feed-reset-" + cal.id, text: "Reset link", onclick: () => setFeed(cal, true, state.projection) }),
          el("button", { type: "button", class: "btn-danger btn-small", id: "cs-feed-off-" + cal.id, text: "Turn off", onclick: () => setFeed(cal, false) })));
    } else {
      const proj = selectOf("cs-feed-proj-" + cal.id, [["busy", "Busy times only"], ["personal_redacted", "My titles and locations"]], "busy");
      proj.setAttribute("aria-label", "What the link shows");
      row.append(el("div", { class: "gi-actions" }, proj,
        el("button", { type: "button", class: "btn-ghost btn-small", id: "cs-feed-on-" + cal.id, text: "Turn on", onclick: () => setFeed(cal, true, proj.value) })));
    }
    return row;
  }
  async function setFeed(cal, enabled, projection) {
    try {
      const body = await api("/v1/calendar/feeds/" + encodeURIComponent(cal.id), { method: "POST", body: enabled ? { enabled: true, projection: projection || "busy" } : { enabled: false } });
      if (enabled && body.url) C.feedUrls.set(cal.id, body.url); else C.feedUrls.delete(cal.id);
      C.feeds.set(cal.id, enabled ? { enabled: true, projection: projection || "busy", expires_at: body.expires_at } : { enabled: false });
      renderSettings();
    } catch (e) { showError("cs-error", explain(e)); }
  }
  async function addCalendar() {
    const name = $("cs-new-name").value.trim();
    if (!name) { $("cs-new-name").focus(); return; }
    try {
      await api("/v1/calendars", { method: "POST", body: { name, color: $("cs-new-color").value, request_key: key() } });
      await loadCalendars();
      await loadFeeds();
      renderSettings();
    } catch (e) { showError("cs-error", e.status === 422 ? "You can have up to 20 calendars." : explain(e)); }
  }
  async function patchCalendar(cal, patch) {
    try {
      await api("/v1/calendars/" + encodeURIComponent(cal.id), { method: "PATCH", body: Object.assign({ expected_revision: cal.revision }, patch) });
      await loadCalendars();
      renderSettings();
      renderBody();
    } catch (e) { showError("cs-error", explain(e)); }
  }
  async function deleteCalendar(cal) {
    if (!window.confirm(`Delete “${cal.name || "Calendar"}”? Events you organize in it are cancelled and guests are told. Invitations in it move to your default calendar.`)) return;
    try {
      await api("/v1/calendars/" + encodeURIComponent(cal.id), { method: "DELETE", body: { expected_revision: cal.revision } });
      await loadCalendars();
      renderSettings();
      refetch();
    } catch (e) { showError("cs-error", explain(e)); }
  }

  async function previewImport(file) {
    if (!file) return;
    if (file.size > 512 * 1024) { fill("cs-import-preview", el("p", { class: "sheet-error", text: "That file is larger than 512 KB." })); return; }
    fill("cs-import-preview", el("p", { class: "field-note", text: "Reading…" }));
    try {
      const text = typeof file.text === "function" ? await file.text() : await new Promise((resolve, reject) => {
        const r = new FileReader();
        r.onload = () => resolve(String(r.result));
        r.onerror = () => reject(r.error);
        r.readAsText(file);
      });
      const calId = $("cs-import-cal").value;
      const res = await api("/v1/calendar/import/preview" + (calId ? "?calendar_id=" + encodeURIComponent(calId) : ""), { method: "POST", text });
      const events = Array.isArray(res.events) ? res.events : [];
      const box = $("cs-import-preview"); // looked up now: the sheet may have been redrawn meanwhile
      box.textContent = "";
      if (Array.isArray(res.warnings) && res.warnings.length) box.append(el("ul", { class: "cs-warnings" }, res.warnings.map((w) => el("li", { class: "field-note", text: String(w) }))));
      box.append(el("ul", { class: "cs-import-list", id: "cs-import-list" }, events.map((ev) => el("li", {},
        el("label", { class: "cal-radio" },
          el("input", { type: "checkbox", class: "cs-import-pick", value: String(ev.index), checked: true }),
          el("span", { text: importLabel(ev) })),
        (ev.warnings || []).map((w) => el("p", { class: "field-note", text: String(w) }))))),
      el("button", { type: "button", class: "btn-primary", id: "cs-import-confirm", text: `Import ${events.length} ${events.length === 1 ? "event" : "events"}`, disabled: !events.length, onclick: (e) => confirmImport(res.draft_id, e.currentTarget) }));
    } catch (e) {
      fill("cs-import-preview", el("p", { class: "sheet-error", role: "alert", text: explain(e) }));
    }
  }
  function importLabel(ev) {
    const when = ev.all_day
      ? ev.start_date + (ev.end_date && addDays(ev.end_date, -1) !== ev.start_date ? " – " + addDays(ev.end_date, -1) : "")
      : String(ev.start || "").replace("T", " ") + (ev.tzid ? " " + ev.tzid : "");
    return (ev.title || "(No title)") + " — " + when + (ev.rrule ? " · repeats" : "");
  }
  async function confirmImport(draftId, button) {
    const select = [...document.querySelectorAll(".cs-import-pick")].filter((x) => x.checked).map((x) => Number(x.value));
    if (!select.length) return;
    button.disabled = true;
    try {
      const calId = $("cs-import-cal").value;
      const res = await api("/v1/calendar/import/" + encodeURIComponent(draftId) + "/confirm", { method: "POST", body: Object.assign({ select }, calId ? { calendar_id: calId } : {}) });
      const n = Array.isArray(res.created) ? res.created.length : select.length;
      fill("cs-import-preview", el("p", { class: "field-note", role: "status", text: `Imported ${n} ${n === 1 ? "event" : "events"}.` }));
      $("cs-import-file").value = "";
      loadCalendars();
      refetch();
    } catch (e) {
      button.disabled = false;
      showError("cs-error", explain(e));
    }
  }

  // ---- live updates --------------------------------------------------------------------------------

  const timers = new Map();
  function soon(k, fn, ms = 150) { clearTimeout(timers.get(k)); timers.set(k, setTimeout(fn, ms)); }

  function onEvent(type, frame) {
    if (!C.enabled) return;
    const data = frame && frame.data && typeof frame.data === "object" ? frame.data : (frame || {});
    if (frame && frame.mailbox && frame.mailbox !== C.mailbox) return;
    switch (type) {
      case "event.invite":
        soon("invites", loadInvitations);
        soon("range", refetch);
        break;
      case "event.updated":
      case "event.cancelled":
      case "event.rsvp":
        soon("range", refetch);
        if (type !== "event.rsvp") soon("invites", loadInvitations);
        if (C.detail && C.detail.event && C.detail.event.id === data.event_id) soon("detail", reloadDetail);
        break;
      case "event.reminder":
        reminder(data);
        break;
      default:
    }
  }

  async function reminder(data) {
    let text = "An event is starting soon.";
    let meeting = null;
    try {
      const body = await api(eventPath(data.event_id) + (data.occurrence_id ? "?occurrence_id=" + encodeURIComponent(data.occurrence_id) : ""));
      const ev = body.event;
      const at = (body.occurrence && body.occurrence.starts_at) || data.starts_at || ev.starts_at;
      text = `${ev.title || "An event"} starts at ${timeLabel(at)}.`;
      if (ev.meeting && ev.meeting.code) meeting = { code: ev.meeting.code, occurrence: data.occurrence_id, title: ev.title };
    } catch (_) { /* the generic text */ }
    $("cal-toast-text").textContent = text;
    const joinBtn = $("cal-toast-join");
    joinBtn.hidden = !meeting;
    joinBtn.onclick = meeting ? () => { $("cal-toast").hidden = true; join(meeting.code, meeting.occurrence, meeting.title); } : null;
    $("cal-toast").hidden = false;
    clearTimeout(reminder.timer);
    reminder.timer = setTimeout(() => { $("cal-toast").hidden = true; }, 20000);
  }

  window.PigeonpostCalendar = {
    onMailbox, onEvent, reset, render, open, close, isOpen, syncNav, goto, setView,
    // For tests and automation: no event contents.
    inspect: () => ({ enabled: C.enabled, mailbox: C.mailbox, view: C.view, cursor: C.cursor, zone: zone(), occurrences: C.occurrences.length, invitations: C.invitations.length, range: C.range }),
    _test: {
      setNow: (unix) => { C.nowOverride = unix; },
      setZone: (z) => { C.zoneOverride = z; partsCache.clear(); },
      zonedToUnix, wall,
    },
  };
})();
