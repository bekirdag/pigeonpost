// Phase 7: the Meeting notes reader in the inbox (SDS §14.1, docs/meeting-summaries.md).
//
// Lists the meetings whose notes this account agreed to, and opens a brief on this device: the
// postbox returns only ciphertext plus the content key wrapped for each of this account's notes
// keys, and this browser's own key (calls-summary.js, IndexedDB) unwraps it. While the feature is
// on, this browser's notes key is registered so notes made later can be opened here too.
(function () {
  "use strict";
  const host = () => window.PigeonpostNotesHost || null;
  const keys = () => window.PigeonpostNotesKeys || null;
  const $ = (id) => document.getElementById(id);
  const enabled = () => Boolean(host() && host().features().has("meeting_summaries"));
  let registeredFor = null;
  let renderGen = 0; // a newer list or brief view supersedes one still loading

  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? "" : v);
    }
    for (const c of children.flat()) if (c != null) node.append(c);
    return node;
  }
  function icon(paths) {
    const s = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    s.setAttribute("viewBox", "0 0 20 20");
    s.setAttribute("aria-hidden", "true");
    s.innerHTML = paths; // static markup from this file only
    return s;
  }
  const NOTE_ICON = '<path d="M5.5 2.5h6.5l3.5 3.5v11.5h-10z" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/><path d="M8 10h5M8 13.5h5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>';

  async function api(method, path, body, renewed) {
    const h = host();
    const token = h.token();
    const res = await fetch(h.postbox() + path, {
      method,
      headers: Object.assign({ authorization: "Bearer " + token }, body ? { "content-type": "application/json" } : {}),
      body: body ? JSON.stringify(body) : undefined,
    });
    if (res.status === 401 && !renewed && (await h.renew())) return api(method, path, body, true);
    const value = await res.json().catch(() => ({}));
    if (!res.ok) throw Object.assign(new Error(value.error || "http_" + res.status), { status: res.status, code: value.error });
    return value;
  }

  // ---- nav and registration -----------------------------------------------------------------------

  function syncNav() {
    const nav = $("side-nav");
    if (!nav) return;
    let btn = $("notes-btn");
    if (enabled() && !btn) {
      btn = el("button", { id: "notes-btn", class: "side-nav-btn", type: "button", onclick: () => open() }, icon(NOTE_ICON), el("span", { text: "Meeting notes" }), el("span", { class: "dot", id: "notes-badge", hidden: true }));
      nav.append(btn);
    }
    if (btn) btn.hidden = !enabled();
  }

  async function register() {
    const h = host();
    const me = h && h.mailbox();
    if (!enabled() || !keys() || !me || registeredFor === me.address) return;
    registeredFor = me.address;
    try {
      const k = await keys().get();
      await api("PUT", `/v1/meetings/keys/${encodeURIComponent(k.key_id)}`, { public_key: k.public_key, label: keys().label() });
    } catch (_) {
      registeredFor = null; // try again on the next change
    }
  }

  function onMailbox() {
    syncNav();
    register();
  }

  function onEvent(event, frame) {
    if (event !== "meeting.brief") return;
    const data = (frame && frame.data) || frame || {};
    if ($("notes-badge")) $("notes-badge").hidden = false;
    if (data.state === "ready") host()?.toast("Meeting notes are ready.");
    if ($("notes-dialog") && !$("notes-dialog").hidden && !$("notes-dialog").dataset.meeting) showList();
  }

  // ---- the dialog ---------------------------------------------------------------------------------

  function dialog() {
    let d = $("notes-dialog");
    if (d) return d;
    d = el("div", { class: "notes-dialog", id: "notes-dialog", role: "dialog", "aria-modal": "true", "aria-labelledby": "notes-title", hidden: true },
      el("div", { class: "notes-panel" },
        el("header", { class: "notes-head" },
          el("h2", { id: "notes-title", text: "Meeting notes" }),
          el("button", { type: "button", class: "linkish", id: "notes-close", text: "Close", onclick: () => close() })),
        el("div", { class: "notes-body", id: "notes-body" })));
    d.addEventListener("click", (e) => { if (e.target === d) close(); });
    document.addEventListener("keydown", (e) => { if (e.key === "Escape" && !d.hidden) close(); });
    document.body.append(d);
    return d;
  }

  function open() {
    const d = dialog();
    d.hidden = false;
    if ($("notes-badge")) $("notes-badge").hidden = true;
    register();
    showList();
    $("notes-close").focus();
  }

  function close() {
    const d = $("notes-dialog");
    if (!d) return;
    d.hidden = true;
    delete d.dataset.meeting;
    $("notes-body").textContent = "";
  }

  const when = (s) => (s ? new Date(s * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }) : "");
  const STATE = { active: "Notes in progress", processing: "Being prepared…", ready: "Ready", failed: "Couldn't be made" };
  const FAILURE = { no_speech: "No speech was captured.", no_recipients: "No device could receive the notes.", stt_unavailable: "Transcription was unavailable.", timed_out: "The notes service didn't finish in time." };

  async function showList() {
    const gen = ++renderGen;
    const d = dialog();
    delete d.dataset.meeting;
    $("notes-title").textContent = "Meeting notes";
    const body = $("notes-body");
    body.textContent = "";
    body.append(el("p", { class: "notes-note", text: "Notes from calls where you agreed to them. They're encrypted for your devices that were in the call or set up for notes; Pigeonpost's servers keep only the encrypted copy, and delete it after the retention date." }));
    let list;
    try {
      list = (await api("GET", "/v1/meetings")).meetings || [];
    } catch (_) {
      if (gen === renderGen) body.append(el("p", { text: "Couldn't load your meeting notes." }));
      return;
    }
    if (gen !== renderGen) return;
    if (!list.length) { body.append(el("p", { text: "No meeting notes yet." })); return; }
    body.append(el("ul", { class: "notes-list" }, list.map((m) => el("li", {},
      el("button", { type: "button", onclick: () => showOne(m.id) },
        el("strong", { text: when(m.created_at) }),
        el("span", { class: "notes-meta", text: `${STATE[m.state] || m.state} · host ${m.host}` + (m.state === "ready" ? ` · kept until ${new Date(m.expires_at * 1000).toLocaleDateString()}` : "") }))))));
  }

  async function showOne(id) {
    const gen = ++renderGen;
    const d = dialog();
    d.dataset.meeting = id;
    const body = $("notes-body");
    body.textContent = "";
    const back = el("button", { type: "button", class: "linkish", text: "← All meeting notes", onclick: () => showList() });
    body.append(back);
    let m;
    try {
      m = await api("GET", `/v1/meetings/${encodeURIComponent(id)}`);
    } catch (_) {
      if (gen === renderGen) body.append(el("p", { text: "These notes are no longer available." }));
      return;
    }
    if (gen !== renderGen) return;
    if (m.state !== "ready") {
      body.append(el("p", { text: m.state === "failed" ? (FAILURE[m.failure] || "The notes couldn't be made.") : "The notes are still being prepared. Check back in a few minutes." }));
      body.append(actions(m));
      return;
    }
    let brief;
    try {
      brief = await keys().open(m);
      if (gen !== renderGen) return;
    } catch (e) {
      if (gen !== renderGen) return;
      body.append(el("p", { text: e.code === "no_key_for_device"
        ? "These notes were encrypted for your other devices. Open them on a device that was in the call, or one that opened Meeting notes before the call ended."
        : "This device couldn't open these notes." }));
      body.append(actions(m));
      return;
    }
    $("notes-title").textContent = brief.title || "Meeting notes";
    body.append(renderBrief(brief), actions(m, brief));
  }

  function renderBrief(b) {
    const section = (title, items, fn) => (items && items.length ? [el("h3", { text: title }), el("ul", {}, items.map(fn))] : []);
    return el("article", { class: "notes-brief" },
      el("p", { class: "notes-meta", text: [b.started_at ? new Date(b.started_at).toLocaleString() : null, b.duration_s ? Math.max(1, Math.round(b.duration_s / 60)) + " min" : null, (b.participants || []).join(", ")].filter(Boolean).join(" · ") }),
      b.draft ? el("p", { class: "notes-draft", text: "A summary couldn't be generated for this meeting. The transcript is below." }) : null,
      b.summary ? [el("h3", { text: "Summary" }), el("p", { text: b.summary })] : null,
      section("Decisions", b.decisions, (x) => el("li", { text: x })),
      section("Action items", b.action_items, (a) => el("li", { text: [a.owner ? a.owner + ":" : null, a.task, a.due ? `(due ${a.due})` : null].filter(Boolean).join(" ") })),
      section("Open questions", b.open_questions, (x) => el("li", { text: x })),
      b.transcript && b.transcript.length ? [el("h3", { text: "Transcript" }), el("ul", { class: "notes-transcript" }, b.transcript.map((t) => el("li", {}, el("time", { text: t.at }), el("b", { text: t.speaker + ":" }), el("span", { text: t.text }))))] : null,
      el("p", { class: "notes-meta", text: "Transcribed and summarized automatically on Pigeonpost's server" + (b.generated_by && b.generated_by.stt ? ` (${b.generated_by.stt}${b.generated_by.llm ? ", " + b.generated_by.llm : ""})` : "") + ". Speaker names come from each person's own microphone; a microphone can pick up other voices." }));
  }

  function asText(b) {
    const out = [b.title || "Meeting notes", ""];
    if (b.summary) out.push(b.summary, "");
    const list = (title, items, fn) => { if (items && items.length) { out.push(title + ":"); for (const x of items) out.push("- " + fn(x)); out.push(""); } };
    list("Decisions", b.decisions, (x) => x);
    list("Action items", b.action_items, (a) => [a.owner ? a.owner + ":" : null, a.task, a.due ? `(due ${a.due})` : null].filter(Boolean).join(" "));
    list("Open questions", b.open_questions, (x) => x);
    list("Transcript", b.transcript, (t) => `[${t.at}] ${t.speaker}: ${t.text}`);
    return out.join("\n");
  }

  function actions(m, brief) {
    return el("div", { class: "notes-actions" },
      brief ? el("button", { type: "button", class: "btn", text: "Copy as text", onclick: async () => { try { await navigator.clipboard.writeText(asText(brief)); host().toast("Copied."); } catch (_) { host().toast("Couldn't copy."); } } }) : null,
      el("button", { type: "button", class: "btn", text: "Delete my copy", onclick: async () => {
        if (!window.confirm("Delete your copy of these notes? Other attendees keep theirs.")) return;
        try { await api("DELETE", `/v1/meetings/${encodeURIComponent(m.id)}`); host().toast("Deleted."); showList(); } catch (_) { host().toast("Couldn't delete."); }
      } }));
  }

  window.PigeonpostNotes = { onMailbox, onEvent, open, close, _renderBrief: renderBrief, _asText: asText };
  // The inbox may have loaded its capabilities before this file was parsed: catch up once.
  if (host()) setTimeout(onMailbox, 0);
})();
