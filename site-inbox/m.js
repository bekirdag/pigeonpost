// Pigeonpost — the meeting and invitation page (Phase 3B), served at /m/<locator>.
//
// Three ways in (docs/calendar.md §6, §7):
//   /m/<locator>#t=<token>[&r=accepted]  a contact guest's personal invitation;
//   /m/u#u=<token>                       an unsubscribe link;
//   /m/<CODE>                            a bare meeting link. Signed-in members may join; anyone else
//                                        sees only whether they may ask to join. Never event details.
//
// The token is read from the fragment and the fragment is removed from the address bar before
// anything else happens. It lives in this closure only — never in storage, a URL, or a log — and is
// sent to the postbox only in POST bodies. Opening the page changes nothing: a preselected answer
// (`r=`) is only sent after the guest confirms it.
//
// Untrusted text (titles, names, locations) is always inserted with textContent.
(function () {
  "use strict";

  // ---- the fragment, first ----------------------------------------------------------------------
  const fragment = new URLSearchParams(String(location.hash || "").replace(/^#/, ""));
  let inviteToken = fragment.get("t") || null;
  let unsubscribeToken = fragment.get("u") || null;
  const presetRaw = fragment.get("r");
  if (location.hash) {
    try { history.replaceState(null, "", location.pathname + location.search); } catch (_) { /* nothing else to do */ }
  }
  const preset = ["accepted", "tentative", "declined"].includes(presetRaw) ? presetRaw : null;

  const cfg = window.PIGEONPOST_INBOX || {};
  const POSTBOX = cfg.postbox || "https://postbox.pigeonpost.dev";
  const CALLS = "/calls/v1";
  const CODE_RE = /^[ABCDEFGHJKMNPQRSTUVWXYZ23456789]{10}$/;
  const locatorRaw = decodeURIComponent((location.pathname.match(/^\/m\/([^/]+)\/?$/) || [])[1] || "");
  const asCode = locatorRaw.replace(/-/g, "").toUpperCase();
  const code = CODE_RE.test(asCode) ? asCode : null;
  const ANSWER = { accepted: "Yes", tentative: "Maybe", declined: "No" };

  const $ = (id) => document.getElementById(id);
  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      else if (k === "dataset") Object.assign(node.dataset, v);
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else if (k === "hidden") node.hidden = Boolean(v);
      else if (k === "checked" || k === "disabled") node[k] = Boolean(v);
      else node.setAttribute(k, v === true ? "" : String(v));
    }
    for (const c of children.flat()) if (c != null && c !== false) node.append(c instanceof Node ? c : document.createTextNode(String(c)));
    return node;
  }
  const content = (...nodes) => { const c = $("m-content"); c.textContent = ""; c.append(...nodes.flat().filter(Boolean)); };
  function setError(text) { const e = $("m-error"); e.textContent = text || ""; e.hidden = !text; }
  const codeLabel = (c) => (c && c.length === 10 ? c.slice(0, 5) + "-" + c.slice(5) : c || "");

  // ---- talking to the postbox (no bearer: guest routes) --------------------------------------------
  async function guestPost(path, body, { text = false } = {}) {
    const res = await fetch(POSTBOX + "/v1/calendar/guest/" + path, {
      method: "POST",
      headers: { "content-type": "application/json", accept: text ? "text/calendar" : "application/json" },
      body: JSON.stringify(body),
      credentials: "omit",
      referrerPolicy: "no-referrer",
    });
    if (text && res.ok) return res.text();
    const value = await res.json().catch(() => ({}));
    if (!res.ok) throw Object.assign(new Error(value.error || "http_" + res.status), { status: res.status, code: value.error, body: value });
    return value;
  }

  // ---- time --------------------------------------------------------------------------------------
  const browserZone = () => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC"; } catch (_) { return "UTC"; } };
  function when(ev, z) {
    if (ev.all_day) {
      const [y, m, d] = String(ev.start_date).split("-").map(Number);
      const [y2, m2, d2] = String(ev.end_date).split("-").map(Number);
      const f = new Intl.DateTimeFormat(undefined, { weekday: "long", day: "numeric", month: "long", year: "numeric", timeZone: "UTC" });
      const first = new Date(Date.UTC(y, m - 1, d, 12)), last = new Date(Date.UTC(y2, m2 - 1, d2 - 1, 12));
      return (first.getTime() === last.getTime() ? f.format(first) : f.format(first) + " – " + f.format(last)) + " · all day";
    }
    const day = new Intl.DateTimeFormat(undefined, { weekday: "long", day: "numeric", month: "long", year: "numeric", timeZone: z });
    const t = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit", timeZone: z });
    return `${day.format(new Date(ev.starts_at * 1000))} · ${t.format(new Date(ev.starts_at * 1000))} – ${t.format(new Date(ev.ends_at * 1000))}`;
  }
  const zoneName = (z) => String(z || "").split("/").pop().replace(/_/g, " ");

  // ---- device proof (docs §6) -------------------------------------------------------------------------
  const b64url = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  let device = null; // { keyPair, publicKey (base64url) } — never stored
  async function deviceKey() {
    if (device) return device;
    const subtle = window.crypto && window.crypto.subtle;
    if (!subtle) throw new Error("no_webcrypto");
    const keyPair = await subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, false, ["sign", "verify"]);
    const raw = await subtle.exportKey("raw", keyPair.publicKey);
    device = { keyPair, publicKey: b64url(raw) };
    return device;
  }
  async function proof(purpose, binding) {
    const d = await deviceKey();
    const ts = Math.floor(Date.now() / 1000);
    const message = `pigeonpost-guest-proof-v1\n${purpose}\n${binding}\n${ts}`;
    const sig = await window.crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, d.keyPair.privateKey, new TextEncoder().encode(message));
    return { device_proof: b64url(sig), ts };
  }

  // ---- the invitation ------------------------------------------------------------------------------
  const G = { preview: null, session: null, sessionId: null, selected: null, sending: false, challenge: null };

  async function loadInvitation() {
    try {
      G.preview = await guestPost("preview", { token: inviteToken });
    } catch (e) {
      $("m-title").textContent = "Invitation";
      content(el("p", { class: "join-note", text: e.code === "rate_limited" ? "Too many attempts. Wait a few minutes and try again." : "This invitation link isn’t valid any more. Ask the organizer for a new one." }));
      return;
    }
    if (preset) G.selected = preset;
    renderInvitation();
  }

  function currentOccurrence() {
    const p = G.preview;
    const list = Array.isArray(p.occurrences) ? p.occurrences : [];
    const t = Date.now() / 1000;
    return list.find((o) => o.ends_at + 1800 >= t && o.status !== "cancelled") || list[0] || null;
  }

  function renderInvitation() {
    const p = G.preview;
    const ev = p.event || {};
    const z = browserZone();
    $("m-title").textContent = ev.title || "Invitation";
    $("m-sub").textContent = ev.organizer_name ? "From " + ev.organizer_name : "";
    const rows = [];
    if (ev.status === "cancelled") rows.push(el("p", { class: "cd-cancelled", text: "This event was cancelled." }));
    rows.push(el("p", { class: "cd-when", id: "m-when", text: when(ev, z) }));
    if (!ev.all_day && ev.tzid && ev.tzid !== z) {
      const t = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit", timeZone: ev.tzid });
      rows.push(el("p", { class: "join-note", id: "m-organizer-zone", text: `${t.format(new Date(ev.starts_at * 1000))} – ${t.format(new Date(ev.ends_at * 1000))} ${zoneName(ev.tzid)} time (the organizer’s)` }));
    }
    if (ev.recurring) rows.push(el("p", { class: "join-note", text: "This is a repeating event." }));
    if (ev.location) rows.push(el("p", { class: "join-note", text: "Where: " + ev.location }));
    if (ev.meeting && ev.meeting.code) rows.push(el("p", { class: "join-note" }, "Pigeonpost meeting ", el("code", { text: codeLabel(ev.meeting.code) })));
    if (Array.isArray(p.guests) && p.guests.length) {
      rows.push(el("ul", { class: "cd-guests" }, p.guests.map((g) => el("li", { class: "cd-guest" }, el("span", { class: "cd-guest-name", text: g.name || "Guest" }), el("span", { class: "cd-rsvp", dataset: { rsvp: g.rsvp || "needs_action" }, text: ANSWER[g.rsvp] || "Awaiting" })))));
    }

    // The answer.
    if (ev.status !== "cancelled") {
      const current = p.attendee && p.attendee.rsvp;
      const pressed = G.selected || current;
      const occ = Array.isArray(p.occurrences) ? p.occurrences : [];
      const scope = ev.recurring && occ.length ? el("select", { id: "m-scope", "aria-label": "Answer for" },
        el("option", { value: "all", text: "All events in the series" }),
        occ.map((o) => el("option", { value: o.occurrence_id, text: "Only " + when({ starts_at: o.starts_at, ends_at: o.ends_at }, z) }))) : null;
      const note = el("input", { type: "text", class: "join-input", id: "m-note", maxlength: "300", placeholder: "Add a note (optional)", "aria-label": "Note to the organizer" });
      const btn = (status) => el("button", { type: "button", class: "btn-ghost cd-rsvp-btn" + (G.selected === status ? " is-preselected" : ""), id: "m-rsvp-" + status, "aria-pressed": String(pressed === status), text: ANSWER[status],
        onclick: () => {
          // A link that preselected an answer asks for a confirmation; a tap is its own confirmation.
          if (G.selected) { G.selected = status; renderInvitation(); return; }
          sendAnswer(status);
        } });
      rows.push(el("section", { class: "cd-section", "aria-labelledby": "m-answer-title" },
        el("h2", { class: "m-h2", id: "m-answer-title", text: "Going?" }),
        el("div", { class: "cd-rsvp-row", role: "group", "aria-labelledby": "m-answer-title" }, btn("accepted"), btn("tentative"), btn("declined")),
        G.selected ? el("p", { class: "join-note", id: "m-confirm-note", text: `Your answer “${ANSWER[G.selected]}” is not sent yet.` }) : null,
        G.selected ? el("button", { type: "button", class: "btn-primary", id: "m-confirm", text: "Confirm: " + ANSWER[G.selected], onclick: () => sendAnswer(G.selected) }) : null,
        scope, note,
        ANSWER[current] ? el("p", { class: "join-note", id: "m-current", text: "Your answer: " + ANSWER[current] }) : null));
    }

    const actions = [el("button", { type: "button", class: "btn-ghost", id: "m-ics", text: "Add to calendar", onclick: () => downloadIcs() })];
    rows.push(el("div", { class: "join-actions m-actions" }, actions));
    if (ev.meeting && ev.meeting.code && ev.status !== "cancelled") rows.push(joinSection(ev, p));
    rows.push(el("p", { class: "join-note", text: "Calendar events are stored encrypted at rest; Pigeonpost can read them to send reminders and invitations." }));
    content(rows);
  }

  async function sendAnswer(status) {
    if (G.sending) return;
    G.sending = true;
    setError("");
    const scopeSel = $("m-scope");
    const body = Object.assign(G.session ? { guest_session: G.session } : { token: inviteToken }, { status, scope: "all" });
    if (scopeSel && scopeSel.value !== "all") { body.scope = "this"; body.occurrence_id = scopeSel.value; }
    const note = $("m-note") && $("m-note").value.trim();
    if (note) body.note = note.slice(0, 300);
    try {
      const res = await guestPost("rsvp", body);
      G.selected = null;
      G.preview.attendee = Object.assign({}, G.preview.attendee, { rsvp: res.rsvp || status, proof: res.proof });
      renderInvitation();
    } catch (e) {
      setError(e.code === "event_cancelled" ? "This event was cancelled." : "Your answer couldn’t be sent. Try again.");
    } finally { G.sending = false; }
  }

  async function downloadIcs() {
    try {
      const text = await guestPost("ics", G.session ? { guest_session: G.session } : { token: inviteToken }, { text: true });
      const url = URL.createObjectURL(new Blob([text], { type: "text/calendar" }));
      const a = el("a", { href: url, download: "invitation.ics", hidden: true });
      document.body.append(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (_) { setError("The calendar file couldn’t be downloaded. Try again."); }
  }

  function joinSection(ev, p) {
    const t = Date.now() / 1000;
    const m = ev.meeting;
    const open = (m.opens_at == null || t >= m.opens_at) && (m.closes_at == null || t <= m.closes_at);
    const box = el("section", { class: "cd-section", id: "m-join", "aria-labelledby": "m-join-title" }, el("h2", { class: "m-h2", id: "m-join-title", text: "Join the meeting" }));
    if (!open) {
      box.append(el("p", { class: "join-note", text: "The meeting opens shortly before it starts. Come back to this page then." }));
      return box;
    }
    if (G.session) {
      box.append(el("p", { class: "join-note", text: "You confirmed it’s you." }), el("button", { type: "button", class: "btn-primary", id: "m-join-btn", text: "Join", onclick: () => joinVerified() }));
    } else if (p.verification_available) {
      if (G.challenge) {
        box.append(el("p", { class: "join-note", text: "We sent a 6-digit code to the address this invitation went to." }),
          el("label", { class: "call-field", for: "m-code" }, el("span", { class: "call-panel-label", text: "Code" }),
            el("input", { class: "join-input", id: "m-code", type: "text", inputmode: "numeric", autocomplete: "one-time-code", maxlength: "6", pattern: "[0-9]{6}" })),
          el("button", { type: "button", class: "btn-primary", id: "m-verify-complete", text: "Confirm", onclick: () => completeVerify() }));
      } else {
        box.append(el("p", { class: "join-note", text: "To join, confirm it’s you with a code sent to the address this invitation went to." }),
          el("button", { type: "button", class: "btn-primary", id: "m-verify-start", text: "Confirm it’s you", onclick: () => startVerify() }));
      }
    } else {
      box.append(el("p", { class: "join-note", text: "Someone in the meeting will let you in." }),
        el("button", { type: "button", class: "btn-primary", id: "m-ask", text: "Ask to join", onclick: () => askWithLink() }));
    }
    return box;
  }

  async function startVerify() {
    setError("");
    try {
      const res = await guestPost("verify/start", { token: inviteToken });
      G.challenge = res.challenge_id;
      renderInvitation();
      $("m-code")?.focus();
    } catch (e) {
      setError(e.code === "external_unavailable" ? "Codes can’t be sent right now." : e.code === "rate_limited" ? "Too many codes requested. Try again later." : "The code couldn’t be sent. Try again.");
    }
  }
  async function completeVerify() {
    const codeInput = $("m-code").value.replace(/\D/g, "");
    if (codeInput.length !== 6) { setError("Enter the 6-digit code."); return; }
    setError("");
    try {
      const d = await deviceKey();
      const pr = await proof("verify", G.challenge);
      const res = await guestPost("verify/complete", { challenge_id: G.challenge, code: codeInput, device_public_key: d.publicKey, device_proof: pr.device_proof, ts: pr.ts });
      G.session = res.guest_session;
      G.sessionId = res.session_id;
      G.challenge = null;
      renderInvitation();
    } catch (e) {
      setError(e.code === "rate_limited" ? "Too many attempts. Ask for a new code." : "That code didn’t work. Check it and try again.");
    }
  }
  async function joinVerified() {
    const occ = currentOccurrence();
    if (!occ) return;
    setError("");
    try {
      const pr = await proof("join", `${G.sessionId}\n${occ.occurrence_id}`);
      const ticket = await guestPost("join", { guest_session: G.session, occurrence_id: occ.occurrence_id, device_proof: pr.device_proof, ts: pr.ts });
      await enterCall(ticket);
    } catch (e) { setError(joinError(e)); }
  }
  async function askWithLink() {
    const occ = currentOccurrence();
    if (!occ) return;
    setError("");
    try {
      const ticket = await guestPost("join", { token: inviteToken, occurrence_id: occ.occurrence_id });
      await enterCall(ticket);
    } catch (e) { setError(joinError(e)); }
  }
  function joinError(e) {
    return { window_closed: "The meeting isn’t open right now.", event_cancelled: "This event was cancelled.", verification_required: "Confirm it’s you first.", rate_limited: "Too many attempts. Wait a minute and try again." }[e.code] || "Couldn’t join. Try again.";
  }
  async function enterCall(ticket) {
    const calls = window.PigeonpostCalls;
    if (!calls || !calls.joinScheduled) { setError("This browser can’t make calls. Try a recent Chrome, Edge, Firefox or Safari."); return; }
    const ev = (G.preview && G.preview.event) || {};
    const res = await calls.joinScheduled(ticket.code || (ev.meeting && ev.meeting.code), {
      ticket: ticket.ticket, occurrenceId: ticket.occurrence_id, guest: true,
      name: (G.preview.attendee && G.preview.attendee.name) || "Guest", title: ev.title || "Meeting",
    });
    if (res && res.state === "error" && res.message) setError(res.message);
  }

  // ---- unsubscribe -----------------------------------------------------------------------------------
  function renderUnsubscribe() {
    $("m-title").textContent = "Stop invitations";
    content(
      el("p", { class: "join-note", text: "Stop invitation messages from Pigeonpost to your address. Events you already answered are not changed." }),
      el("div", { class: "join-actions m-actions" },
        el("button", { type: "button", class: "btn-ghost", id: "m-unsub-organizer", text: "From this organizer", onclick: () => unsubscribe("organizer") }),
        el("button", { type: "button", class: "btn-primary", id: "m-unsub-all", text: "From anyone on Pigeonpost", onclick: () => unsubscribe("all") })));
  }
  async function unsubscribe(scope) {
    setError("");
    try {
      await guestPost("unsubscribe", { token: unsubscribeToken, scope });
      content(el("p", { class: "join-note", role: "status", id: "m-unsub-done", text: scope === "all" ? "Done. You won’t get invitations from anyone on Pigeonpost." : "Done. You won’t get invitations from this organizer." }));
    } catch (_) { setError("That didn’t work. The link may have expired."); }
  }

  // ---- a bare meeting link --------------------------------------------------------------------------
  function memberToken() { try { return localStorage.getItem("ppi_token"); } catch (_) { return null; } }

  async function meetingInfo(bearer) {
    const res = await fetch(CALLS + "/meetings/scheduled/" + encodeURIComponent(code), { headers: Object.assign({ accept: "application/json" }, bearer ? { authorization: "Bearer " + bearer } : {}) });
    const body = await res.json().catch(() => ({}));
    return { status: res.status, body };
  }

  async function loadMeeting() {
    $("m-title").textContent = "Meeting " + codeLabel(code);
    const token = memberToken();
    if (token) {
      try {
        const info = await meetingInfo(token);
        if (info.status !== 401 && info.status !== 403) return renderMember(info.body, token);
      } catch (_) { /* treat as anonymous */ }
    }
    try {
      const info = await meetingInfo(null);
      renderAnonymous(info.body);
    } catch (_) {
      content(el("p", { class: "join-note", text: "Couldn’t reach the meeting service. Check your connection and reload." }));
    }
  }

  async function renderMember(info, token) {
    if (info.state !== "open") {
      const list = Array.isArray(info.occurrences) ? info.occurrences : [];
      content(el("p", { class: "join-note", id: "m-not-open", text: info.state === "not_open" ? "This meeting isn’t open yet. It opens shortly before it starts." : "This meeting isn’t open." }),
        list.length ? el("ul", { class: "cd-guests" }, list.slice(0, 5).map((o) => el("li", { class: "cd-guest", text: when({ starts_at: o.starts_at, ends_at: o.ends_at }, browserZone()) }))) : null);
      return;
    }
    let mailboxes = [];
    try {
      const res = await fetch(POSTBOX + "/v1/identities", { headers: { authorization: "Bearer " + token, accept: "application/json" } });
      if (res.ok) mailboxes = ((await res.json()).identities || []).filter((m) => m && m.address);
    } catch (_) { /* the calls service picks */ }
    const pick = mailboxes.length > 1 ? el("select", { class: "call-select", id: "m-as", "aria-label": "Join as" }, mailboxes.map((m) => el("option", { value: m.address, text: m.handle || m.label || m.address }))) : null;
    content(el("p", { class: "join-note", text: "You’re signed in to Pigeonpost." }), pick,
      el("div", { class: "join-actions" }, el("button", { type: "button", class: "btn-primary", id: "m-member-join", text: "Join",
        onclick: () => memberJoin(pick ? pick.value : (mailboxes[0] && mailboxes[0].address)) })));
  }
  async function memberJoin(as, occurrenceId) {
    const calls = window.PigeonpostCalls;
    if (!calls || !calls.joinScheduled) return;
    setError("");
    const res = await calls.joinScheduled(code, { as, occurrenceId });
    if (res && res.state === "choose_occurrence") {
      content(el("p", { class: "join-note", text: "Two meetings in this series are open. Which one?" }),
        el("ul", { class: "cal-choose-list" }, (res.occurrences || []).map((o) => el("li", {}, el("button", { type: "button", class: "btn-ghost", text: when({ starts_at: o.starts_at, ends_at: o.ends_at }, browserZone()), onclick: () => memberJoin(as, o.occurrence_id) })))));
    } else if (res && res.state === "error" && res.message) setError(res.message);
  }

  function renderAnonymous(info) {
    if (info.state === "open" && info.can_knock) {
      const name = el("input", { class: "join-input", id: "m-name", type: "text", maxlength: "60", autocomplete: "name", placeholder: "How the others will see you" });
      content(el("p", { class: "join-note", text: "Someone in the meeting has to let you in." }),
        el("label", { class: "call-field", for: "m-name" }, el("span", { class: "call-panel-label", text: "Your name" }), name),
        el("div", { class: "join-actions" }, el("button", { type: "button", class: "btn-primary", id: "m-knock", text: "Ask to join", onclick: async () => {
          const n = name.value.trim();
          if (!n) { name.focus(); return; }
          const calls = window.PigeonpostCalls;
          if (!calls || !calls.joinScheduled) return;
          const res = await calls.joinScheduled(code, { name: n, guest: true });
          if (res && res.state === "error" && res.message) setError(res.message);
        } })));
      return;
    }
    content(el("p", { class: "join-note", id: "m-not-open", text: "This meeting isn’t open." }));
  }

  // calls.js tells us when a call this page started has ended.
  window.PigeonpostMeetingPage = {
    left(message) { if (message) setError(message); },
  };

  function boot() {
    if (locatorRaw === "u" && unsubscribeToken) return renderUnsubscribe();
    if (inviteToken) return loadInvitation();
    if (code) return loadMeeting();
    $("m-title").textContent = "Meeting";
    content(el("p", { class: "join-note", text: "Open this page from the invitation or meeting link you received." }));
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", boot);
  else boot();
})();
