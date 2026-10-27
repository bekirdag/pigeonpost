// Phase 7: meeting notes in a call (SDS §14, docs/meeting-summaries.md).
//
// What this file does, and nothing more:
// - shows every participant a persistent notes indicator and the consent sheet (what is captured,
//   who processes it, who receives the notes, how long they are kept), with Agree / Decline, and
//   "Stop for me" at any time;
// - while the calls service says capture is on *and* this person agreed, records only this
//   device's own microphone track (the processed one the call sends — never anyone else's audio),
//   in ~10 s self-contained chunks;
// - encrypts each chunk on this device to the notes service's per-meeting key, after checking that
//   key's signature against the notes service key pinned below, and uploads it with this
//   participant's capture credential. The calls service relays it and cannot read it;
// - keeps a notes key for this device (a non-extractable P-256 private key in IndexedDB) and gives
//   its public half with a member's "Agree", so the finished notes can be opened here.
// calls.js drives it through window.PigeonpostCallsSummary; nothing here touches peers or media
// routing.
(function () {
  "use strict";

  // The meeting-intel signing key(s), raw uncompressed P-256 points, base64. A session key not
  // signed by one of these is never used.
  const PINNED_KEYS = ["BATLIrbcZxdhMyUSNh+JSa0LGk+zLnah3KtO+l91d3C+oRB+GVUcVE0JPT8tOrbzud0OoKAvaLH5T6keIVqX5Eg="];
  const AUDIO_INFO = "pigeonpost meeting audio v1|";
  const NOTES_INFO = "pigeonpost meeting notes v1|";
  const SIGN_CONTEXT = "pigeonpost-meeting-intel-v1";
  const CHUNK_MS = 10_000;
  const MIN_CHUNK_MS = 3_000;
  const TARGET_BYTES = 90 * 1024; // keep each upload well under the service's limit
  const MAX_BACKLOG = 6; // chunks waiting to upload; older ones are dropped beyond this
  const enc = new TextEncoder();
  const subtle = window.crypto && window.crypto.subtle;

  const b64 = (buf) => {
    const bytes = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
    let s = "";
    for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    return btoa(s);
  };
  const unb64 = (text) => Uint8Array.from(atob(String(text || "")), (c) => c.charCodeAt(0));

  // ---- this device's notes key -------------------------------------------------------------------

  const NotesKeys = (() => {
    const DB = "pigeonpost-meeting-notes";
    let cached = null;
    function db() {
      return new Promise((resolve, reject) => {
        if (!window.indexedDB) return reject(new Error("no_indexeddb"));
        const req = indexedDB.open(DB, 1);
        req.onupgradeneeded = () => req.result.createObjectStore("keys");
        req.onsuccess = () => resolve(req.result);
        req.onerror = () => reject(req.error || new Error("indexeddb"));
      });
    }
    async function read(d) {
      return new Promise((resolve) => {
        const r = d.transaction("keys").objectStore("keys").get("device");
        r.onsuccess = () => resolve(r.result || null);
        r.onerror = () => resolve(null);
      });
    }
    async function write(d, value) {
      return new Promise((resolve, reject) => {
        const tx = d.transaction("keys", "readwrite");
        try { tx.objectStore("keys").put(value, "device"); } catch (e) { reject(e); return; }
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error || new Error("indexeddb"));
      });
    }
    // The device's key: made once, the private half never exported.
    async function get() {
      if (cached) return cached;
      if (!subtle) throw new Error("no_webcrypto");
      const d = await db();
      let rec = await read(d);
      if (rec && rec.key_id && !rec.privateKey && rec.privatePkcs8) {
        // Stored as bytes (below): back to a non-extractable key for this page's lifetime.
        rec = { ...rec, privateKey: await subtle.importKey("pkcs8", unb64(rec.privatePkcs8), { name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]) };
      }
      if (!rec || !rec.privateKey || !rec.key_id) {
        const id = "nk" + b64(window.crypto.getRandomValues(new Uint8Array(12))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
        const pair = await subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
        const raw = await subtle.exportKey("raw", pair.publicKey);
        rec = { key_id: id, privateKey: pair.privateKey, public_key: b64(raw), created_at: Date.now() };
        try {
          await write(d, rec);
        } catch (e) {
          if (!e || e.name !== "DataCloneError") throw e;
          // WebKit in app web views cannot store a CryptoKey in IndexedDB. There the private key is
          // kept as PKCS#8 bytes in this origin's storage on this device (it never leaves it) and
          // re-imported as non-extractable when needed.
          const pair2 = await subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, ["deriveBits"]);
          const pkcs8 = await subtle.exportKey("pkcs8", pair2.privateKey);
          const raw2 = await subtle.exportKey("raw", pair2.publicKey);
          await write(d, { key_id: id, privatePkcs8: b64(pkcs8), public_key: b64(raw2), created_at: Date.now() });
          new Uint8Array(pkcs8).fill(0);
          rec = { key_id: id, privateKey: await subtle.importKey("pkcs8", unb64((await read(d)).privatePkcs8), { name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]), public_key: b64(raw2), created_at: Date.now() };
        }
      }
      cached = rec;
      return rec;
    }
    function label() {
      const ua = navigator.userAgent || "";
      if (document.body && document.body.dataset.page === "native") return /Android/.test(ua) ? "Android app" : /iPhone|iPad/.test(ua) ? "iPhone or iPad app" : "Desktop app";
      return /Android|iPhone|iPad/.test(ua) ? "Mobile browser" : "Web browser";
    }
    async function hkdfKey(secret, salt, info, usage) {
      const base = await subtle.importKey("raw", secret, "HKDF", false, ["deriveKey"]);
      return subtle.deriveKey({ name: "HKDF", hash: "SHA-256", salt: enc.encode(salt), info: enc.encode(info) }, base, { name: "AES-GCM", length: 256 }, false, usage);
    }
    // Open a stored brief with this device's key: `meeting` is GET /v1/meetings/{id}.
    async function open(meeting) {
      const me = await get();
      const wrap = (meeting.keys || []).find((k) => k.key_id === me.key_id);
      if (!wrap) { const e = new Error("no_key_for_device"); e.code = "no_key_for_device"; throw e; }
      const epk = await subtle.importKey("raw", unb64(wrap.epk), { name: "ECDH", namedCurve: "P-256" }, false, []);
      const shared = await subtle.deriveBits({ name: "ECDH", public: epk }, me.privateKey, 256);
      const kek = await hkdfKey(shared, meeting.id, NOTES_INFO + wrap.key_id, ["decrypt"]);
      const cekRaw = await subtle.decrypt({ name: "AES-GCM", iv: unb64(wrap.iv), additionalData: enc.encode(`${meeting.id}|${wrap.key_id}`) }, kek, unb64(wrap.wrapped));
      const cek = await subtle.importKey("raw", cekRaw, "AES-GCM", false, ["decrypt"]);
      new Uint8Array(cekRaw).fill(0);
      const plain = await subtle.decrypt({ name: "AES-GCM", iv: unb64(meeting.iv), additionalData: enc.encode(meeting.id) }, cek, unb64(meeting.ciphertext));
      return JSON.parse(new TextDecoder().decode(plain));
    }
    return { get, label, open, hkdfKey };
  })();
  window.PigeonpostNotesKeys = NotesKeys;

  // ---- the session key the notes service gave this meeting ----------------------------------------

  async function verified(view) {
    const k = view && view.service_key;
    if (!subtle || !k || typeof k.key !== "string" || typeof k.signature !== "string") return false;
    if (k.expires_at * 1000 < Date.now()) return false;
    const msg = enc.encode([SIGN_CONTEXT, k.session, view.meeting, k.key, k.expires_at].join("\n"));
    for (const pinned of PINNED_KEYS) {
      try {
        const pub = await subtle.importKey("raw", unb64(pinned), { name: "ECDSA", namedCurve: "P-256" }, false, ["verify"]);
        if (await subtle.verify({ name: "ECDSA", hash: "SHA-256" }, pub, unb64(k.signature), msg)) return true;
      } catch (_) { /* try the next */ }
    }
    return false;
  }

  function recorderMime() {
    if (!window.MediaRecorder) return null;
    for (const m of ["audio/webm;codecs=opus", "audio/ogg;codecs=opus", "audio/mp4;codecs=mp4a.40.2", "audio/mp4", "audio/webm"]) {
      try { if (MediaRecorder.isTypeSupported(m)) return m; } catch (_) { /* next */ }
    }
    return "";
  }
  const canCapture = () => Boolean(subtle && window.MediaRecorder && window.MediaStream);

  // ---- state ----------------------------------------------------------------------------------------

  let api = null; // from calls.js: { room, session, mic, muted, post, base, authHeader, toast, el, svg, icon, isGuest }
  let view = null; // the newest `summary` view for the room on screen
  let trusted = null; // { session, ok } — the verified service key of the current meeting
  const early = new Map(); // room id -> view that arrived before the room was on screen
  const seqs = new Map(); // meeting id -> next chunk number (never reused within a meeting)
  let cap = null; // the running capture
  let sheetFor = null; // meeting id the consent sheet was last shown for

  function attach(a) {
    api = a;
  }

  function onEvent(data) {
    if (!api || !data || typeof data.room !== "string") return;
    const r = api.room();
    if (!r || r.id !== data.room) { early.set(data.room, data); if (early.size > 8) early.delete(early.keys().next().value); return; }
    apply(data);
  }

  function joined(roomId) {
    const v = early.get(roomId);
    early.delete(roomId);
    if (v) apply(v);
  }

  async function apply(next) {
    const before = view;
    view = next;
    if (next.meeting && (!trusted || trusted.session !== next.service_key?.session)) {
      trusted = { session: next.service_key?.session, ok: null };
      const t = trusted;
      t.ok = await verified(next);
      if (view !== next) { render(); return syncCapture(); }
    }
    render();
    // A newly requested meeting (or one this person has not answered) asks once.
    if ((next.state === "waiting" || next.state === "active") && next.you && next.you.consent === null && sheetFor !== next.meeting) {
      sheetFor = next.meeting;
      openSheet();
    }
    if (next.state === "stopped" && before && before.state !== "stopped" && before.meeting === next.meeting) {
      const mine = before.you && before.you.consent === "yes";
      api.toast(mine && !api.isGuest() ? "Notes stopped. The summary will appear in Meeting notes in a few minutes." : "Notes stopped.");
      closeSheet();
    }
    syncCapture();
  }

  // ---- the UI -------------------------------------------------------------------------------------

  const $ = (id) => document.getElementById(id);

  function ensureUi() {
    if ($("call-notes-banner") || !api) return Boolean($("call-notes-banner"));
    const top = document.querySelector("#call-ui .call-main");
    const controls = $("call-controls");
    if (!top || !controls) return false;
    const { el } = api;
    const banner = el("div", { class: "call-notes-banner", id: "call-notes-banner", role: "status", "aria-live": "polite", hidden: true },
      el("span", { class: "call-notes-dot", "aria-hidden": "true" }),
      el("span", { class: "call-notes-text", id: "call-notes-text" }),
      el("button", { type: "button", class: "call-pill", id: "call-notes-review", text: "Review", hidden: true, onclick: () => openSheet() }),
      el("button", { type: "button", class: "call-pill", id: "call-notes-stopme", text: "Stop for me", hidden: true, onclick: () => answer(false) }),
      el("button", { type: "button", class: "call-pill call-pill-danger", id: "call-notes-off", text: "Turn off notes", hidden: true, onclick: () => setNotes(false) }));
    const sheet = el("div", { class: "call-notes-sheet", id: "call-notes-sheet", role: "dialog", "aria-modal": "true", "aria-labelledby": "call-notes-title", hidden: true },
      el("div", { class: "call-notes-card" },
        el("h3", { id: "call-notes-title" }),
        el("ul", { class: "call-notes-list", id: "call-notes-list" }),
        el("p", { class: "call-notes-warn", id: "call-notes-warn", hidden: true }),
        el("div", { class: "call-notes-actions" },
          el("button", { type: "button", class: "call-pill", id: "call-notes-decline", text: "Decline", onclick: () => answer(false) }),
          el("button", { type: "button", class: "call-pill call-notes-agree", id: "call-notes-agree", text: "Agree", onclick: () => answer(true) }))));
    const ask = el("div", { class: "call-notes-sheet", id: "call-notes-ask", role: "dialog", "aria-modal": "true", "aria-labelledby": "call-notes-ask-title", hidden: true },
      el("div", { class: "call-notes-card" },
        el("h3", { id: "call-notes-ask-title", text: "Take notes for this call?" }),
        el("p", { text: "Everyone in the call is asked first. Nothing is captured until everyone has agreed, and anyone can decline or stop at any time." }),
        el("p", { text: "Each person's own microphone is encrypted for Pigeonpost's notes service, transcribed and summarized on Pigeonpost's server. Members who agree get the summary and transcript, encrypted for their devices." }),
        el("div", { class: "call-notes-actions" },
          el("button", { type: "button", class: "call-pill", text: "Cancel", onclick: () => { $("call-notes-ask").hidden = true; } }),
          el("button", { type: "button", class: "call-pill call-notes-agree", id: "call-notes-ask-go", text: "Ask everyone", onclick: () => { $("call-notes-ask").hidden = true; setNotes(true); } }))));
    top.append(banner, sheet, ask);
    const btn = el("button", { id: "call-notes-btn", class: "call-btn", type: "button", "aria-label": "Meeting notes", title: "Meeting notes", hidden: true, onclick: () => notesButton() },
      api.svg('<path d="M5.5 2.5h6.5l3.5 3.5v11.5h-10z" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"/><path d="M8 10h5M8 13.5h5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>'));
    controls.insertBefore(btn, $("call-hangup"));
    return true;
  }

  function render() {
    if (!ensureUi()) return;
    const v = view;
    const on = v && (v.state === "waiting" || v.state === "active");
    const you = (v && v.you) || {};
    $("call-notes-btn").hidden = !(you.can_request || (on && you.can_control));
    $("call-notes-btn").setAttribute("aria-pressed", String(Boolean(on)));
    const banner = $("call-notes-banner");
    banner.hidden = !on;
    banner.dataset.state = on ? v.state : "off";
    if (!on) return;
    const agreed = you.consent === "yes";
    let text;
    if (v.state === "active") text = agreed ? "Notes on — your microphone is being captured for notes" : "Notes on";
    else if (you.consent !== "yes") text = "Notes requested — paused until everyone agrees";
    else text = v.waiting_for === 1 ? "Notes paused — waiting for 1 person to agree" : `Notes paused — waiting for ${v.waiting_for} people to agree`;
    if (trusted && trusted.ok === false) text = "Notes can't be verified on this device";
    $("call-notes-text").textContent = text;
    $("call-notes-review").hidden = agreed;
    $("call-notes-stopme").hidden = !agreed;
    $("call-notes-off").hidden = !you.can_control;
  }

  function openSheet() {
    if (!ensureUi() || !view || !view.disclosure) return;
    const d = view.disclosure;
    const { el } = api;
    $("call-notes-title").textContent = `${view.host || "The host"} asked to take meeting notes`;
    const list = $("call-notes-list");
    list.textContent = "";
    const rows = [["Processed by", d.processor], ["What is captured", d.capture], ["Audio", d.audio], ["Who gets the notes", d.audience], ["Kept for", d.retention], ["Please note", d.limits], ["Your choice", d.control]];
    for (const [k, text] of rows) if (text) list.append(el("li", {}, el("strong", { text: k }), el("span", { text })));
    const warn = $("call-notes-warn");
    const problems = [];
    if (api.isGuest()) problems.push("You're joining as a guest: you can agree, but you won't receive a copy of the notes.");
    if (trusted && trusted.ok === false) problems.push("This device couldn't verify the notes service, so it won't capture anything.");
    else if (!canCapture()) problems.push("This device can't record for notes. If you agree, others may still be captured; your own microphone won't be.");
    warn.textContent = problems.join(" ");
    warn.hidden = !problems.length;
    $("call-notes-agree").disabled = Boolean(trusted && trusted.ok === false);
    $("call-notes-sheet").hidden = false;
    $("call-notes-agree").focus?.();
  }

  function closeSheet() {
    if ($("call-notes-sheet")) $("call-notes-sheet").hidden = true;
    if ($("call-notes-ask")) $("call-notes-ask").hidden = true;
  }

  function notesButton() {
    const on = view && (view.state === "waiting" || view.state === "active");
    if (on) setNotes(false);
    else { ensureUi(); $("call-notes-ask").hidden = false; }
  }

  async function setNotes(enabled) {
    const r = api.room();
    if (!r) return;
    try {
      const v = await api.post(`/rooms/${r.id}/summary`, { session: api.session(), enabled });
      if (v && v.room) onEvent(v);
    } catch (e) {
      const why = { notes_disabled: "Meeting notes aren't available for this account.", summary_busy: "The notes service is busy. Try again in a few minutes.", summary_unavailable: "The notes service isn't reachable right now.", host_only: "Only the host can turn notes on or off." }[e.code];
      api.toast(why || "Couldn't change meeting notes.");
    }
  }

  async function answer(yes) {
    const r = api.room();
    if (!r || !view) return;
    closeSheet();
    if (!yes) stopCapture(); // stop this device at once, before the network round trip
    const body = { session: api.session(), consent: yes };
    if (yes) {
      body.version = view.version;
      body.disclosure_hash = view.disclosure_hash;
      if (!api.isGuest()) {
        try {
          const k = await NotesKeys.get();
          body.notes_key = { key_id: k.key_id, public_key: k.public_key, label: NotesKeys.label() };
        } catch (_) { /* no storage: this device can't open the notes later */ }
      }
    }
    try {
      const v = await api.post(`/rooms/${r.id}/summary/consent`, body);
      if (v && v.room) onEvent(v);
    } catch (e) {
      if (e.code === "consent_stale") { api.toast("Someone joined or the notes changed. Please review again."); sheetFor = null; return openSheet(); }
      api.toast(yes ? "Couldn't record your answer. Try again." : "Couldn't record your answer; this device has stopped capturing.");
    }
  }

  // ---- capture ------------------------------------------------------------------------------------

  function wanted() {
    const v = view;
    return Boolean(v && v.state === "active" && v.you && v.you.consent === "yes" && v.capture && v.capture.token
      && trusted && trusted.ok === true && trusted.session === v.service_key.session && canCapture());
  }

  async function syncCapture() {
    if (!wanted()) return stopCapture();
    const v = view;
    if (cap && cap.meeting === v.meeting && cap.version === v.capture.version) return;
    stopCapture();
    const c = { meeting: v.meeting, version: v.capture.version, token: v.capture.token, room: v.room, session: v.service_key.session, participant: v.you.participant, queue: [], running: true, uploading: false, chunkMs: CHUNK_MS, mime: recorderMime() };
    cap = c;
    try {
      const svc = await subtle.importKey("raw", unb64(v.service_key.key), { name: "ECDH", namedCurve: "P-256" }, false, []);
      const eph = await subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
      const shared = await subtle.deriveBits({ name: "ECDH", public: svc }, eph.privateKey, 256);
      c.key = await NotesKeys.hkdfKey(shared, c.session, AUDIO_INFO + c.participant, ["encrypt"]);
      c.epk = b64(await subtle.exportKey("raw", eph.publicKey));
    } catch (_) {
      if (cap === c) cap = null;
      return;
    }
    if (cap !== c) return;
    record(c);
  }

  function stopCapture() {
    const c = cap;
    cap = null;
    if (!c) return;
    c.running = false;
    clearTimeout(c.timer);
    c.queue.length = 0; // queued chunks belong to a consent interval that is over
    try { if (c.rec && c.rec.state !== "inactive") c.rec.stop(); } catch (_) { /* gone */ }
    c.key = null;
  }

  function record(c) {
    if (!c.running || cap !== c) return;
    const track = api.mic();
    if (!track || track.readyState === "ended") { c.timer = setTimeout(() => record(c), 1000); return; }
    let rec;
    try {
      rec = new MediaRecorder(new MediaStream([track]), c.mime ? { mimeType: c.mime, audioBitsPerSecond: 24_000 } : { audioBitsPerSecond: 24_000 });
    } catch (_) {
      return;
    }
    const parts = [];
    const startedAt = Date.now();
    c.rec = rec;
    c.allMuted = api.muted();
    rec.ondataavailable = (e) => { if (e.data && e.data.size) parts.push(e.data); };
    rec.onstop = () => {
      const dur = Date.now() - startedAt;
      const silent = c.allMuted;
      if (cap === c && c.running && !silent && parts.length) enqueue(c, new Blob(parts, { type: rec.mimeType || c.mime || "audio/webm" }), startedAt, dur);
      if (cap === c && c.running) record(c);
    };
    try { rec.start(); } catch (_) { return; }
    c.timer = setTimeout(() => { try { if (rec.state !== "inactive") rec.stop(); } catch (_) { /* gone */ } }, c.chunkMs);
  }

  // Blob.arrayBuffer is missing from older WebKit builds.
  function bytesOf(blob) {
    if (typeof blob.arrayBuffer === "function") return blob.arrayBuffer();
    return new Promise((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => resolve(r.result);
      r.onerror = () => reject(r.error || new Error("read"));
      r.readAsArrayBuffer(blob);
    });
  }

  async function enqueue(c, blob, t, dur) {
    // Keep uploads small whatever the browser's encoder chose.
    if (blob.size > TARGET_BYTES) c.chunkMs = Math.max(MIN_CHUNK_MS, Math.floor(c.chunkMs * TARGET_BYTES / blob.size));
    const seq = seqs.get(c.meeting) || 0;
    seqs.set(c.meeting, seq + 1);
    const codec = (blob.type || "audio/webm").slice(0, 60);
    const iv = window.crypto.getRandomValues(new Uint8Array(12));
    const durMs = Math.max(1, Math.min(30_000, Math.round(dur)));
    const aad = `${c.session}|${c.participant}|${seq}|${codec}|${t}|${durMs}`;
    let ct;
    try {
      const plain = new Uint8Array(await bytesOf(blob));
      ct = await subtle.encrypt({ name: "AES-GCM", iv, additionalData: enc.encode(aad) }, c.key, plain);
      plain.fill(0);
    } catch (_) {
      return;
    }
    if (cap !== c) return;
    c.queue.push({ seq, envelope: { v: 1, epk: c.epk, iv: b64(iv), ct: b64(ct), codec, t, dur: durMs }, tries: 0 });
    while (c.queue.length > MAX_BACKLOG) c.queue.shift();
    upload(c);
  }

  async function upload(c) {
    if (c.uploading) return;
    c.uploading = true;
    try {
      while (cap === c && c.queue.length) {
        const item = c.queue[0];
        const res = await fetch(`${api.base}/rooms/${c.room}/summary/audio?session=${encodeURIComponent(api.session())}&seq=${item.seq}`, {
          method: "POST",
          headers: Object.assign({ "content-type": "application/json", "x-capture-token": c.token }, api.authHeader() ? { authorization: api.authHeader() } : {}),
          body: JSON.stringify(item.envelope),
        }).catch(() => null);
        if (cap !== c) return;
        if (res && res.ok) { c.queue.shift(); continue; }
        const status = res ? res.status : 0;
        if (status === 503 || status === 0 || status === 429 || status === 502) {
          if (++item.tries > 3) { c.queue.shift(); continue; }
          await new Promise((r) => setTimeout(r, 2_000 * item.tries));
          continue;
        }
        // Paused, refused or malformed: this interval's audio is not wanted any more.
        c.queue.length = 0;
      }
    } finally {
      c.uploading = false;
    }
  }

  function micChanged() {
    // The processed track changed (device or noise setting): the next chunk records the new one.
    const c = cap;
    if (c && c.rec && c.rec.state !== "inactive") { clearTimeout(c.timer); try { c.rec.stop(); } catch (_) { /* gone */ } }
  }

  function muteChanged() {
    if (cap && !api.muted()) cap.allMuted = false;
  }

  function left() {
    stopCapture();
    closeSheet();
    view = null;
    trusted = null;
    sheetFor = null;
    if ($("call-notes-banner")) $("call-notes-banner").hidden = true;
    if ($("call-notes-btn")) $("call-notes-btn").hidden = true;
  }

  window.PigeonpostCallsSummary = {
    attach, onEvent, joined, micChanged, muteChanged, left,
    inspect: () => ({ state: view && view.state, consent: view && view.you && view.you.consent, capturing: Boolean(cap && cap.running), verified: trusted && trusted.ok, queued: cap ? cap.queue.length : 0, chunkMs: cap ? cap.chunkMs : null, mime: recorderMime() }),
  };
})();
