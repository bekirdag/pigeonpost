// Pigeonpost inbox — a messenger over the hosted postbox.
//
// Shape of the thing: the postbox stores both halves of a conversation. A delivered message is
// sealed to the recipient, and a sent one is sealed a second time to the sender, so
// `/v1/inbox?include_sent=true` returns a thread rather than an inbox. The only state this app keeps
// of its own is `Pending`: the seconds between pressing send and the next poll.
//
// Message bodies arrive from other agents. They are inserted with textContent, never as markup, and
// nothing in a body is ever acted on by this app.
(function () {
  "use strict";

  const cfg = window.PIGEONPOST_INBOX;
  const $ = (id) => document.getElementById(id);

  // ---- small helpers ------------------------------------------------------------------------

  let toastTimer = null;
  function toast(message) {
    const el = $("toast");
    el.textContent = message;
    el.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { el.hidden = true; }, 5200);
  }

  const HOUR = 3600, DAY = 86400;

  function clockTime(unix) {
    return new Date(unix * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }

  // WhatsApp's rule: today shows a clock, this week a weekday, older a date.
  function listTime(unix) {
    const now = Date.now() / 1000;
    const then = new Date(unix * 1000);
    const midnight = new Date(); midnight.setHours(0, 0, 0, 0);
    if (unix >= midnight.getTime() / 1000) return clockTime(unix);
    if (now - unix < 6 * DAY) return then.toLocaleDateString([], { weekday: "short" });
    return then.toLocaleDateString([], { day: "numeric", month: "short" });
  }

  function dayLabel(unix) {
    const then = new Date(unix * 1000); then.setHours(0, 0, 0, 0);
    const today = new Date(); today.setHours(0, 0, 0, 0);
    const days = Math.round((today - then) / (DAY * 1000));
    if (days === 0) return "Today";
    if (days === 1) return "Yesterday";
    if (days < 7) return then.toLocaleDateString([], { weekday: "long" });
    return then.toLocaleDateString([], { day: "numeric", month: "long", year: then.getFullYear() === today.getFullYear() ? undefined : "numeric" });
  }

  const sameDay = (a, b) => new Date(a * 1000).toDateString() === new Date(b * 1000).toDateString();

  // ---- Phase 1e: message actions and receipts (docs/message-actions.md) -----------------------
  //
  // Pure helpers first, so the rules can be tested without a page: which tick a sent message shows,
  // whether it may still be edited, and what a reply quotes. The DOM that uses them is further down.

  const MREF_ALPHABET = "abcdefghijklmnopqrstuvwxyz234567";
  const EDIT_WINDOW_SECS = 24 * HOUR;
  const QUOTE_MAX = 200;

  // `mref_` + 26 base32 characters: the conversation-level id both copies of a message share.
  function newMsgRef() {
    const bytes = new Uint8Array(26);
    crypto.getRandomValues(bytes);
    let out = "mref_";
    for (const b of bytes) out += MREF_ALPHABET[b & 31];
    return out;
  }
  const isMsgRef = (value) => typeof value === "string" && /^mref_[a-z2-7]{26}$/.test(value);

  // The marks inside a sent bubble. `null` means none: received mail, or a postbox without receipts.
  // The agent's gear sits on top of the receipt while it works; `done` falls back to the receipt.
  function tickState(m, flags) {
    if (!m || m.kind !== "out" || !flags || !flags.receipts) return null;
    if (m.status === "sending") return "pending";
    if (m.status === "failed") return null;
    if (flags.agentStatus && m.agent_status) {
      if (m.agent_status.state === "working") return "working";
      if (m.agent_status.state === "failed") return "agent-failed";
    }
    const state = m.receipt && m.receipt.state;
    if (state === "seen") return "seen";
    if (state === "delivered") return "delivered";
    return "sent";
  }

  // Own, sent, on the server, inside the 24-hour window and not yet gone. The server enforces all of
  // this again; the predicate only decides whether the pencil is worth showing.
  function editable(m, now, flags) {
    if (!m || m.kind !== "out" || !flags || !flags.edit) return false;
    if (m.status === "sending" || m.status === "failed") return false;
    if (!isMsgRef(m.msg_ref)) return false;
    const at = Number(m.at) || 0;
    if (!at || now - at > EDIT_WINDOW_SECS) return false;
    if (m.expires_at && m.expires_at <= now) return false;
    return true;
  }

  // What a reply quotes: the first line that says something, without its markdown, at most 200
  // characters. A message that is only a file quotes the file's name.
  function quoteExcerpt(text, attachments) {
    for (const line of String(text == null ? "" : text).split(/\r?\n/)) {
      const plain = line.trim().replace(/^#{1,6}\s*/, "").replace(/^[-*+>]\s+/, "").replace(/\*\*/g, "").replace(/`/g, "").trim();
      if (!plain || plain.startsWith("```")) continue;
      return plain.length > QUOTE_MAX ? plain.slice(0, QUOTE_MAX - 1) + "…" : plain;
    }
    const file = Array.isArray(attachments) && attachments[0];
    if (file) return ("📎 " + (file.filename || file.name || "attachment")).slice(0, QUOTE_MAX);
    return "";
  }

  // The listing's Phase 1e fields, carried onto the model a bubble is drawn from.
  function actionFields(m) {
    return {
      msg_ref: m.msg_ref || null,
      reply_to: m.reply_to || null,
      edited_at: m.edited_at || null,
      edit_count: m.edit_count || 0,
      receipt: m.receipt || null,
      agent_status: m.agent_status || null,
    };
  }

  const ICONS = {
    copy: '<svg viewBox="0 0 20 20" aria-hidden="true"><rect x="7" y="7" width="10" height="10" rx="2.2" fill="none" stroke="currentColor" stroke-width="1.6"/><path d="M13 4.6A2 2 0 0011.2 3.5H5.5a2 2 0 00-2 2v5.7A2 2 0 004.6 13" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>',
    reply: '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M8 5L3.5 9.5 8 14" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/><path d="M4 9.5h7.5a5 5 0 015 5v1" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>',
    edit: '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M13.6 3.6a2 2 0 012.8 2.8L7 15.8l-3.6.8.8-3.6 9.4-9.4z" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"/><path d="M12.2 5l2.8 2.8" fill="none" stroke="currentColor" stroke-width="1.6"/></svg>',
    check: '<svg viewBox="0 0 16 12" aria-hidden="true"><path d="M1.5 6.5l3.5 3.5 7-8" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>',
    checks: '<svg viewBox="0 0 20 12" aria-hidden="true"><path d="M1.5 6.5l3.5 3.5 7-8" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/><path d="M9 9.2l.8.8 7-8" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>',
    clock: '<svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.4"/><path d="M8 4.8V8l2.2 1.4" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/></svg>',
    gear: '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M8.6 2.5h2.8l.4 2a5.6 5.6 0 011.5.9l1.9-.7 1.4 2.4-1.5 1.3a5.6 5.6 0 010 1.7l1.5 1.3-1.4 2.4-1.9-.7a5.6 5.6 0 01-1.5.9l-.4 2H8.6l-.4-2a5.6 5.6 0 01-1.5-.9l-1.9.7-1.4-2.4 1.5-1.3a5.6 5.6 0 010-1.7L3.4 7.1l1.4-2.4 1.9.7a5.6 5.6 0 011.5-.9z" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/><circle cx="10" cy="10" r="2.2" fill="none" stroke="currentColor" stroke-width="1.5"/></svg>',
    warn: '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M10 3l7.5 13h-15z" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"/><path d="M10 8v3.5M10 13.8v.2" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>',
  };

  // The marks themselves, as a node. `seenAt` labels the green ticks with when.
  function tickNode(kind, seenAt) {
    if (!kind) return null;
    const span = document.createElement("span");
    span.className = "ticks ticks-" + kind;
    span.setAttribute("role", "img");
    const label = {
      pending: "Sending",
      sent: "Sent",
      delivered: "Delivered",
      seen: seenAt ? "Seen " + clockTime(seenAt) : "Seen",
      working: "Agent working on this",
      "agent-failed": "Agent run failed",
    }[kind];
    span.setAttribute("aria-label", label);
    span.title = label;
    span.innerHTML = { pending: ICONS.clock, sent: ICONS.check, delivered: ICONS.checks, seen: ICONS.checks,
      working: ICONS.gear, "agent-failed": ICONS.warn }[kind];
    if (kind === "working") {
      // Shown only when motion is reduced, where the gear stands still and the word says it instead.
      const word = document.createElement("span");
      word.className = "ticks-word";
      word.textContent = "Working…";
      span.append(word);
    }
    return span;
  }

  // One small icon button for the row under a bubble.
  function actionButton(kind, label, onClick) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "msg-action " + kind;
    button.title = label;
    button.setAttribute("aria-label", label);
    button.innerHTML = ICONS[kind];
    button.addEventListener("click", onClick);
    return button;
  }

  // Copy, with the brief confirmation the old in-bubble button gave.
  function copyButton(getText) {
    const button = actionButton("copy", "Copy message", async () => {
      try {
        await navigator.clipboard.writeText(getText());
        button.classList.add("copied");
        button.innerHTML = ICONS.check;
        setTimeout(() => { button.classList.remove("copied"); button.innerHTML = ICONS.copy; }, 1400);
      } catch (_) {
        // Clipboard access can be refused outright; say nothing rather than throw a dialog.
      }
    });
    return button;
  }

  window.PigeonpostMessageActions = { newMsgRef, isMsgRef, tickState, editable, quoteExcerpt, actionFields, tickNode, actionButton, copyButton, ICONS, EDIT_WINDOW_SECS };

  // Default inboxes must identify their owner: /alp and /bekir are different people. The rule
  // itself ("/bekir/main" -> "Bekir", "/wodo/hr" -> "Wodo HR") lives in names.js, shared with calls.js.
  function displayName(peer) {
    return window.PigeonpostNames.displayName(peer);
  }

  function normaliseAddressInput(input) {
    const before = input.value;
    const rest = before.replace(/^[\s/]+/, "");
    const after = rest ? "/" + rest : (before.includes("/") ? "/" : "");
    if (after === before) return;
    const start = input.selectionStart, end = input.selectionEnd;
    input.value = after;
    if (start !== null && end !== null) {
      const shift = after.length - before.length;
      const clamp = (n) => Math.max(0, Math.min(after.length, n + shift));
      input.setSelectionRange(clamp(start), clamp(end));
    }
  }

  // A contact's alias wins over the name derived from the handle, in text and on the face alike.
  function peerName(peer) {
    const contact = peer && state.contacts ? contactFor(peer) : null;
    return (contact && contact.alias) || displayName(peer);
  }

  function initials(peer, name) {
    return window.PigeonpostNames.initials(name || peerName(peer));
  }

  // Stable per-peer colour so a thread keeps its face between sessions.
  function tone(peer) {
    let h = 0;
    for (let i = 0; i < peer.length; i++) h = (h * 31 + peer.charCodeAt(i)) >>> 0;
    return (h % 6) + 1;
  }

  function paintAvatar(el, peer, name) {
    el.textContent = initials(peer, name);
    el.dataset.tone = String(tone(peer));
    // A profile picture, when there is one this account may see; the initials stay underneath.
    Avatars.decorate(el, peer);
  }

  // ---- session ------------------------------------------------------------------------------

  // localStorage rather than sessionStorage: some browsers drop session storage across the
  // cross-site sign-in round-trip, which loses the PKCE verifier and turns every exchange into a
  // silent invalid_grant loop.
  const LS = window.localStorage;
  const K = { token: "ppi_token", refresh: "ppi_refresh", verifier: "ppi_pkce", state: "ppi_state", identity: "ppi_identity" };

  const getToken = () => LS.getItem(K.token);
  const setToken = (t) => LS.setItem(K.token, t);
  const getRefresh = () => LS.getItem(K.refresh);
  const setRefresh = (t) => (t ? LS.setItem(K.refresh, t) : LS.removeItem(K.refresh));

  function signOut() {
    [K.token, K.refresh, K.verifier, K.state, K.identity].forEach((k) => LS.removeItem(k));
    window.PigeonpostCalls?.stop();
    closeViewer();
    stopLive();
    // The stream cursor and the fallback verdict belong to the account that was signed in, not to
    // the browser. Hiding a tab must keep them — resuming from the cursor is the whole point — but
    // signing out and back in as somebody else must not, or the next account's stream starts after
    // a row number that means nothing in its mailbox, and one postbox's buffering proxy has
    // condemned another to long-polling.
    resetLive();
    mailboxController.abort();
    mailboxController = new AbortController();
    sessionVersion += 1;
    closeMailboxSheets();
    drafts.clear();
    resetComposer();
    Pending.clear();
    acked.clear();
    resetConversationView();
    $("messages").textContent = "";
    prunePreviews();
    $("threads").textContent = "";
    state = freshState();
    resetPhase1b();
    window.PigeonpostGroups?.reset(); // Phase 3a
    window.PigeonpostCalendar?.reset(); // Phase 3B
    const banner = $("offline-banner");
    if (banner) banner.hidden = true;
    render();
  }

  function randomString(len) {
    const a = new Uint8Array(len);
    crypto.getRandomValues(a);
    return Array.from(a, (b) => ("0" + (b & 0xff).toString(16)).slice(-2)).join("");
  }

  async function sha256b64url(input) {
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(input));
    let str = "";
    new Uint8Array(digest).forEach((b) => (str += String.fromCharCode(b)));
    return btoa(str).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }

  const authUrl = (path) => cfg.oidc.issuer + "/protocol/openid-connect" + path;
  const redirectUri = () => window.location.origin + (cfg.oidc.redirectPath || "/");

  async function login() {
    const verifier = randomString(48);
    const csrf = randomString(12);
    LS.setItem(K.verifier, verifier);
    LS.setItem(K.state, csrf);
    // offline_access keeps this signed in the way a messenger is expected to be: a mail app that
    // logs you out every half hour is a mail app nobody opens.
    const scope = (cfg.oidc.scope || "openid") + " offline_access";
    const url = authUrl("/auth")
      + `?client_id=${encodeURIComponent(cfg.oidc.clientId)}`
      + `&response_type=code&scope=${encodeURIComponent(scope)}`
      + `&redirect_uri=${encodeURIComponent(redirectUri())}`
      + `&code_challenge=${await sha256b64url(verifier)}&code_challenge_method=S256`
      + `&state=${csrf}`;
    window.location.href = url;
  }

  // Exchange the authorization code with the realm directly. This is a public PKCE client, so there
  // is no secret to protect and no backend to route through — but it does mean the realm must list
  // this origin under the client's web origins, or the browser blocks the exchange.
  async function completeLoginIfReturning() {
    const params = new URLSearchParams(window.location.search);
    const strip = () => history.replaceState({}, "", cfg.oidc.redirectPath || "/");

    if (params.get("error")) {
      toast("Sign-in did not complete: " + params.get("error"));
      strip();
      return;
    }
    const code = params.get("code");
    if (!code) return;

    const expected = LS.getItem(K.state);
    if (expected && params.get("state") !== expected) {
      toast("Sign-in could not be verified. Please try again.");
      LS.removeItem(K.verifier); LS.removeItem(K.state); strip();
      return;
    }

    const form = new URLSearchParams({
      grant_type: "authorization_code",
      client_id: cfg.oidc.clientId,
      code,
      redirect_uri: redirectUri(),
      code_verifier: LS.getItem(K.verifier) || "",
    });
    try {
      const res = await fetch(authUrl("/token"), {
        method: "POST",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: form,
      });
      const body = await res.json().catch(() => ({}));
      if (res.ok && body.access_token) {
        setToken(body.access_token);
        setRefresh(body.refresh_token);
        scheduleRenewal(body.expires_in);
      } else {
        // Say why rather than looping silently — a misconfigured redirect URI is otherwise
        // indistinguishable from a wrong password.
        toast("Could not complete sign-in" + (body.error_description ? ": " + body.error_description : "."));
      }
    } catch (_) {
      toast("Could not reach the sign-in service.");
    }
    LS.removeItem(K.verifier); LS.removeItem(K.state);
    strip();
  }

  // One in-flight refresh, shared: a burst of 401s must not spend a rotating refresh token twice.
  let refreshInFlight = null;
  let renewalTimer = null;

  function scheduleRenewal(expiresIn) {
    clearTimeout(renewalTimer);
    const seconds = Number(expiresIn);
    if (!seconds || seconds < 30) return;
    renewalTimer = setTimeout(() => { renewSession(); }, (seconds - 25) * 1000);
  }

  // Seconds left on a stored access token, read from its own `exp`.
  //
  // Renewal used to be scheduled only when a token arrived — at sign-in or at a refresh — which
  // left the commonest case unscheduled: a reload picks the token up from storage, and no timer
  // exists for it. The session then ran until something 401'd, and recovering reactively means
  // spending the refresh token at whatever moment a poll happens to fail. With a realm that
  // rotates refresh tokens that is exactly when concurrent callers race and one loses, so the
  // session died "after a while" instead of renewing quietly.
  //
  // Not a security check: the postbox validates every token itself. This only decides when to ask
  // for the next one, so an unreadable token simply means "renew now".
  function secondsLeftOn(token) {
    try {
      const payload = token.split(".")[1];
      const json = atob(payload.replace(/-/g, "+").replace(/_/g, "/"));
      const exp = Number(JSON.parse(json).exp);
      if (!exp) return 0;
      return exp - Math.floor(Date.now() / 1000);
    } catch (_) {
      return 0;
    }
  }

  // Pick up a stored session: renew it now if it is spent, otherwise schedule the renewal it never
  // got. Called on boot, and whenever the tab comes back after being hidden — a background tab's
  // timers are throttled or coalesced, so a laptop that slept through the renewal wakes up holding
  // a dead token and would otherwise discover it by failing.
  async function resumeSession() {
    const token = getToken();
    if (!token) return;
    const left = secondsLeftOn(token);
    if (left < 60) await renewSession();
    else scheduleRenewal(left);
  }

  async function renewSession() {
    const refresh = getRefresh();
    const version = sessionVersion;
    if (!refresh) return false;
    if (!refreshInFlight) {
      refreshInFlight = (async () => {
        try {
          const res = await fetch(authUrl("/token"), {
            method: "POST",
            headers: { "content-type": "application/x-www-form-urlencoded" },
            body: new URLSearchParams({
              grant_type: "refresh_token",
              client_id: cfg.oidc.clientId,
              refresh_token: refresh,
            }),
          });
          const body = await res.json().catch(() => ({}));
          if (version !== sessionVersion) return false;
          if (res.ok && body.access_token) {
            setToken(body.access_token);
            if (body.refresh_token) setRefresh(body.refresh_token); // the realm rotates these
            scheduleRenewal(body.expires_in);
            return true;
          }
          setRefresh(null); // dead or expired — stop trying it
          return false;
        } catch (_) {
          return false; // a network blip is not a dead session; keep the token and let the caller retry
        } finally {
          refreshInFlight = null;
        }
      })();
    }
    return refreshInFlight;
  }

  // ---- postbox API --------------------------------------------------------------------------

  class ApiError extends Error {
    constructor(status, code, detail) {
      super(detail || code || `postbox ${status}`);
      this.status = status;
      this.code = code;
    }
  }

  // Files chosen but not yet sent. Uploaded on send rather than on pick: an upload counts against
  // the mailbox quota the moment it lands, and a file chosen and then thought better of should
  // cost nothing.
  const staged = [];

  function wireAttach() {
    const button = $("attach-btn");
    const input = $("file-input");
    if (!button || !input) return;
    button.addEventListener("click", () => input.click());
    input.addEventListener("change", () => {
      stageFiles(input.files);
      // Cleared so choosing the same file twice in a row still fires a change.
      input.value = "";
    });
    $("compose").addEventListener("paste", (e) => {
      if (!state.openPeer) return;
      const clipboard = e.clipboardData;
      let files = Array.from(clipboard?.files || []);
      // Some browsers expose image bytes only through items. These are alternate
      // representations of the same clipboard, so never append both lists.
      if (!files.length) files = Array.from(clipboard?.items || [])
        .filter(item => item.kind === "file").map(item => item.getAsFile()).filter(Boolean);
      if (!files.length) return; // Ordinary text keeps the browser's selection/undo behaviour.
      e.preventDefault();
      stageFiles(files);
    });
  }

  function stageFiles(files) {
    const chosen = Array.from(files || []);
    if (!chosen.length) return;
    for (const file of chosen) staged.push(file);
    renderStaged();
  }

  // Dragging a file onto the conversation is the same act as choosing one with the paperclip, and
  // it is what people try first. What makes it worth wiring carefully is what the browser does
  // with a drop nobody handled: it navigates the tab to the file, which throws away a half-written
  // message and the thread it was being written in. So every file drop anywhere in the window is
  // swallowed here, and one that arrives with a conversation open is staged.
  function wireDrop() {
    const veil = $("drop-veil");
    if (!veil) return;

    // `types` is an array in current browsers and a DOMStringList in the ones that are not; this
    // reads both. A drag of text or of a link is left alone entirely — dragging words about inside
    // the message box is a thing people do, and it must keep working.
    const carriesFiles = (e) =>
      !!e.dataTransfer && Array.prototype.indexOf.call(e.dataTransfer.types || [], "Files") !== -1;

    // Browsers fire dragenter/dragleave for every element the cursor crosses, so a plain pair of
    // handlers strobes. Counting entries against leaves is what holds the overlay still.
    let depth = 0;
    const hide = () => { depth = 0; veil.hidden = true; };

    document.addEventListener("dragenter", (e) => {
      if (!carriesFiles(e)) return;
      e.preventDefault();
      depth += 1;
      veil.hidden = !state.openPeer;
    });
    document.addEventListener("dragleave", (e) => {
      if (!carriesFiles(e)) return;
      depth -= 1;
      if (depth <= 0) hide();
    });
    document.addEventListener("dragend", hide);
    document.addEventListener("dragover", (e) => {
      if (!carriesFiles(e)) return;
      // Not optional: the default action of a dragover is to refuse the drop, and without this the
      // drop event never happens at all.
      e.preventDefault();
      if (e.dataTransfer) e.dataTransfer.dropEffect = state.openPeer ? "copy" : "none";
    });
    document.addEventListener("drop", (e) => {
      if (!carriesFiles(e)) return;
      e.preventDefault();
      hide();
      if (!state.openPeer) {
        toast("Open a conversation first, then drop the file.");
        return;
      }
      const dropped = droppedFiles(e.dataTransfer);
      stageFiles(dropped.files);
      if (dropped.folders && !dropped.files.length) toast("Folders cannot be attached, only files.");
      else if (dropped.folders) toast("Folders were left out — only the files were attached.");
    });
  }

  // A dropped folder is handed over as a `File` with no type and no readable bytes, which fails at
  // upload time with nothing useful to say. `webkitGetAsEntry` is the only way to tell one from a
  // file at drop time, and it must be called before the event returns; where it is missing the
  // drop is taken at face value.
  function droppedFiles(dt) {
    const items = dt && dt.items ? Array.from(dt.items) : [];
    if (!items.length || typeof items[0].webkitGetAsEntry !== "function") {
      return { files: Array.from((dt && dt.files) || []), folders: 0 };
    }
    const files = [];
    let folders = 0;
    for (const item of items) {
      if (item.kind !== "file") continue;
      const entry = item.webkitGetAsEntry();
      if (entry && entry.isDirectory) { folders += 1; continue; }
      const file = item.getAsFile();
      if (file) files.push(file);
    }
    return { files, folders };
  }

  function renderStaged() {
    const list = $("pending-files");
    if (!list) return;
    list.textContent = "";
    list.hidden = staged.length === 0;
    staged.forEach((file, index) => {
      const li = document.createElement("li");
      const name = document.createElement("span");
      name.className = "pf-name";
      name.textContent = file.name;
      const size = document.createElement("span");
      size.className = "pf-size";
      size.textContent = readableBytes(file.size);
      const drop = document.createElement("button");
      drop.type = "button";
      drop.className = "pf-drop";
      drop.setAttribute("aria-label", "Remove " + file.name);
      drop.textContent = "\u00d7";
      drop.addEventListener("click", () => {
        staged.splice(index, 1);
        renderStaged();
      });
      addPreview(li, { name: file.name, type: file.type, bytes: file.size, load: async () => file }, true);
      li.append(name, size, drop);
      list.append(li);
    });
    prunePreviews();
    updateSendAvailability();
  }

  function updateSendAvailability() {
    $("send-btn").disabled = (!$("compose").value.trim() && !staged.length)
      || !state.openPeer || sendingDrafts.has(composerKey());
  }

  function readableBytes(n) {
    if (!Number.isFinite(n)) return "";
    const units = ["B", "KB", "MB", "GB"];
    let value = n;
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
      value /= 1024;
      unit += 1;
    }
    return (value < 10 && unit > 0 ? value.toFixed(1) : Math.round(value)) + " " + units[unit];
  }


  // Preview only known passive formats. A name cannot override an explicit content type.
  function previewType(type, name) {
    let mime = String(type || "").split(";")[0].trim().toLowerCase();
    if (!mime || mime === "application/octet-stream") {
      mime = { png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif",
        webp: "image/webp", bmp: "image/bmp", avif: "image/avif", mp4: "video/mp4",
        m4v: "video/mp4", mov: "video/quicktime", webm: "video/webm", mp3: "audio/mpeg",
        m4a: "audio/mp4", wav: "audio/wav", ogg: "audio/ogg", pdf: "application/pdf",
        txt: "text/plain", csv: "text/csv", md: "text/plain", json: "application/json" }[String(name || "").split(".").pop().toLowerCase()];
    }
    if (/^image\/(png|jpeg|gif|webp|bmp|avif)$/.test(mime)) return { kind: "image", mime };
    if (/^video\/(mp4|webm|quicktime)$/.test(mime)) return { kind: "video", mime };
    if (/^audio\/(mpeg|mp4|wav|x-wav|ogg|webm)$/.test(mime)) return { kind: "audio", mime };
    if (mime === "application/pdf") return { kind: "pdf", mime };
    if (["text/plain", "text/csv", "text/markdown", "application/json"].includes(mime)) return { kind: "text", mime };
    return null;
  }

  const previewRecords = new Map();
  let previewActive = 0;
  const previewQueue = [];
  const previewObserver = typeof IntersectionObserver === "function" ? new IntersectionObserver(entries => {
    for (const entry of entries) {
      if (entry.isIntersecting) {
        previewObserver.unobserve(entry.target);
        const record = previewRecords.get(entry.target);
        if (record) { previewQueue.push(record); pumpPreviews(); }
      }
    }
  }, { rootMargin: "100px" }) : null;

  function prunePreviews() {
    for (const [node, record] of previewRecords) if (!node.isConnected) {
      record.disposed = true;
      record.controller.abort();
      record.cancel?.();
      if (record.url) URL.revokeObjectURL(record.url);
      previewObserver?.unobserve(node);
      previewRecords.delete(node);
    }
  }

  function addPreview(parent, { name, type, bytes, load, onOpen }, compact = false) {
    const format = previewType(type, name);
    if (!format || !Number.isFinite(bytes) || bytes > 25 * 1024 * 1024 || bytes <= 0) return;
    const node = document.createElement("div");
    node.className = "attachment-preview" + (compact ? " compact" : "");
    node.setAttribute("aria-label", "Preview of " + name);
    if (onOpen) {
      // The preview is also the way in: click or Enter opens the file inside the app.
      node.classList.add("openable");
      node.tabIndex = 0;
      node.setAttribute("role", "button");
      node.setAttribute("aria-label", "Open " + name);
      node.addEventListener("click", onOpen);
      node.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onOpen(); } });
    }
    node.textContent = "Loading preview…";
    parent.append(node);
    const record = { node, format, load, name, controller: new AbortController(), disposed: false };
    previewRecords.set(node, record);
    if (previewObserver) previewObserver.observe(node);
    else queueMicrotask(() => { previewQueue.push(record); pumpPreviews(); });
  }

  function pumpPreviews() {
    while (previewActive < 3 && previewQueue.length) {
      const record = previewQueue.shift();
      if (record.disposed) continue;
      previewActive++;
      renderPreview(record).catch(() => {
        if (!record.disposed) record.node.textContent = "Preview unavailable";
      }).finally(() => { previewActive--; pumpPreviews(); });
    }
  }

  async function renderPreview(record) {
    const { node, format, controller } = record;
    const blob = await record.load(controller.signal);
    if (record.disposed || controller.signal.aborted) return;
    if (!blob.size || blob.size > 25 * 1024 * 1024) throw new Error("Preview size limit");
    if (format.kind === "text") {
      const text = document.createElement("pre");
      text.textContent = (await blob.slice(0, 4096).text()).slice(0, 1200);
      if (!record.disposed) node.replaceChildren(text);
      return;
    }
    const canvas = document.createElement("canvas");
    let source, cleanup = () => {};
    if (format.kind === "pdf") {
      const pdfjs = await import("./pdfjs/pdf.mjs");
      pdfjs.GlobalWorkerOptions.workerSrc = new URL("./pdfjs/pdf.worker.mjs", location.href).href;
      const task = pdfjs.getDocument({ data: new Uint8Array(await blob.arrayBuffer()),
        isEvalSupported: false, useWasm: false, disableFontFace: true,
        standardFontDataUrl: new URL("./pdfjs/standard_fonts/", location.href).href,
        cMapUrl: new URL("./pdfjs/cmaps/", location.href).href, cMapPacked: true });
      record.cancel = () => { void task.destroy(); };
      try {
        const pdf = await task.promise;
        const page = await pdf.getPage(1), initial = page.getViewport({ scale: 1 });
        const viewport = page.getViewport({ scale: Math.min(440 / initial.width, 300 / initial.height) });
        canvas.width = Math.max(1, Math.ceil(viewport.width)); canvas.height = Math.max(1, Math.ceil(viewport.height));
        await page.render({ canvasContext: canvas.getContext("2d"), viewport }).promise;
      } finally { await task.destroy(); record.cancel = null; }
    } else {
      const rawURL = URL.createObjectURL(new Blob([blob], { type: format.mime }));
      let media;
      cleanup = () => { if (media) { media.removeAttribute("src"); media.load(); } URL.revokeObjectURL(rawURL); };
      try {
        if (format.kind === "image") {
          source = new Image(); source.decoding = "async"; source.src = rawURL;
          await source.decode();
        } else {
          media = document.createElement(format.kind === "video" ? "video" : "audio");
          media.preload = "auto"; media.muted = true;
          if (format.kind === "video") media.playsInline = true;
          await new Promise((resolve, reject) => {
            const timer = setTimeout(() => done(new Error("Preview timed out")), 12000);
            const abort = () => done(new Error("Preview cancelled"));
            const done = error => {
              clearTimeout(timer); controller.signal.removeEventListener("abort", abort);
              media.onloadeddata = media.onerror = null;
              error ? reject(error) : resolve();
            };
            controller.signal.addEventListener("abort", abort, { once: true });
            media.onloadeddata = () => done(); media.onerror = () => done(new Error("Unsupported media"));
            media.src = rawURL; media.load();
          });
          if (format.kind === "audio") {
            const label = document.createElement("span");
            label.textContent = "♫ " + record.name + (Number.isFinite(media.duration) ? ` · ${Math.round(media.duration)} sec` : "");
            if (!record.disposed) node.replaceChildren(label);
            return;
          }
          source = media;
        }
        const width = source.naturalWidth || source.videoWidth, height = source.naturalHeight || source.videoHeight;
        if (!width || !height || width * height > 64 * 1024 * 1024) throw new Error("Image too large");
        const scale = Math.min(1, 440 / width, 300 / height);
        canvas.width = Math.max(1, Math.round(width * scale)); canvas.height = Math.max(1, Math.round(height * scale));
        canvas.getContext("2d").drawImage(source, 0, 0, canvas.width, canvas.height);
      } finally { cleanup(); }
    }
    if (record.disposed) return;
    canvas.setAttribute("role", "img"); canvas.setAttribute("aria-label", "Preview of " + record.name);
    node.replaceChildren(canvas);
    if (format.kind === "video") {
      const play = document.createElement("span"); play.className = "preview-play"; play.textContent = "▶"; play.setAttribute("aria-hidden", "true"); node.append(play);
    }
  }

  // ---- opening and saving attachments --------------------------------------------------------
  //
  // Formats the browser can show safely open in a viewer inside the app: images, video, audio,
  // PDF (rendered by the bundled pdf.js with script evaluation off) and plain text (as text, never
  // markup). SVG and HTML are deliberately not among them. Everything else downloads.

  const VIEWER_MAX_BYTES = 200 * 1024 * 1024;
  const TEXT_VIEW_CHARS = 2_000_000;
  const DOWNLOAD_ICON = '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M10 3v9.5M6 9l4 4 4-4M4 16.5h12" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>';
  let viewerState = null;

  // Phones get the share sheet for pictures and videos, which is where "Save to Photos" lives;
  // everything else, and every computer, gets an ordinary download into the downloads folder.
  async function downloadAttachment(item) {
    try {
      const blob = await item.load();
      if (!item.context.current()) return;
      const format = previewType(item.type, item.name);
      const mobile = window.matchMedia("(pointer: coarse)").matches;
      if (mobile && format && (format.kind === "image" || format.kind === "video") && navigator.canShare) {
        const asFile = new File([blob], item.name, { type: blob.type || format.mime });
        if (navigator.canShare({ files: [asFile] })) {
          try { await navigator.share({ files: [asFile] }); return; }
          catch (e) { if (e && e.name === "AbortError") return; }
        }
      }
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = item.name;
      document.body.append(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 30_000);
    } catch (_) {
      if (item.context.current()) toast("Could not download that file.");
    }
  }

  async function openViewer(item, format, trigger) {
    closeViewer();
    const viewer = $("viewer"), body = $("viewer-body");
    const controller = new AbortController();
    const state = viewerState = { item, controller, url: null, pdf: null, observer: null, trigger: trigger || document.activeElement };
    $("viewer-name").textContent = item.name;
    $("viewer-meta").textContent = [readableBytes(item.bytes), { image: "Image", video: "Video", audio: "Audio", pdf: "PDF", text: "Text" }[format.kind]].filter(Boolean).join(" · ");
    body.dataset.kind = format.kind;
    body.replaceChildren(Object.assign(document.createElement("p"), { className: "viewer-loading", textContent: "Opening…" }));
    viewer.hidden = false;
    $("viewer-close").focus({ preventScroll: true });
    try {
      const blob = await item.load(controller.signal);
      if (viewerState !== state) return;
      if (format.kind === "text") {
        const text = await blob.text();
        const pre = document.createElement("pre");
        pre.className = "viewer-text";
        pre.textContent = text.length > TEXT_VIEW_CHARS ? text.slice(0, TEXT_VIEW_CHARS) : text;
        body.replaceChildren(pre);
        if (text.length > TEXT_VIEW_CHARS) body.append(Object.assign(document.createElement("p"), { className: "viewer-note", textContent: "Showing the beginning of this file. Download it to see all of it." }));
        return;
      }
      if (format.kind === "pdf") return renderPdf(state, blob);
      state.url = URL.createObjectURL(new Blob([blob], { type: format.mime }));
      let media;
      if (format.kind === "image") {
        media = document.createElement("img");
        media.alt = item.name;
        media.decoding = "async";
      } else {
        media = document.createElement(format.kind);
        media.controls = true;
        media.preload = "metadata";
        if (format.kind === "video") media.playsInline = true;
      }
      media.className = "viewer-media";
      media.addEventListener("error", () => { if (viewerState === state) body.replaceChildren(viewerFallback("This file can’t be shown here.")); }, { once: true });
      media.src = state.url;
      body.replaceChildren(media);
    } catch (_) {
      if (viewerState === state && !controller.signal.aborted) body.replaceChildren(viewerFallback("Couldn’t open this file."));
    }
  }

  function viewerFallback(text) {
    const box = document.createElement("div");
    box.className = "viewer-fallback";
    const p = document.createElement("p");
    p.textContent = text;
    const b = document.createElement("button");
    b.type = "button";
    b.className = "viewer-btn";
    b.textContent = "Download instead";
    b.addEventListener("click", () => viewerState && downloadAttachment(viewerState.item));
    box.append(p, b);
    return box;
  }

  // Every page, at the width of the window, drawn as it scrolls into view.
  async function renderPdf(state, blob) {
    const body = $("viewer-body");
    const pdfjs = await import("./pdfjs/pdf.mjs");
    pdfjs.GlobalWorkerOptions.workerSrc = new URL("./pdfjs/pdf.worker.mjs", location.href).href;
    const task = pdfjs.getDocument({ data: new Uint8Array(await blob.arrayBuffer()),
      isEvalSupported: false, useWasm: false, disableFontFace: true,
      standardFontDataUrl: new URL("./pdfjs/standard_fonts/", location.href).href,
      cMapUrl: new URL("./pdfjs/cmaps/", location.href).href, cMapPacked: true });
    state.pdf = task;
    const pdf = await task.promise;
    if (viewerState !== state) return;
    const pages = document.createElement("div");
    pages.className = "viewer-pages";
    body.replaceChildren(pages);
    const width = () => Math.min(pages.clientWidth || body.clientWidth || 800, 1100);
    const count = Math.min(pdf.numPages, 500);
    const first = await pdf.getPage(1);
    const ratio = first.getViewport({ scale: 1 }).height / first.getViewport({ scale: 1 }).width;
    state.observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting || entry.target.dataset.drawn) continue;
        entry.target.dataset.drawn = "1";
        state.observer.unobserve(entry.target);
        drawPdfPage(state, pdf, Number(entry.target.dataset.page), entry.target, width());
      }
    }, { root: body, rootMargin: "800px 0px" });
    for (let n = 1; n <= count; n++) {
      const slot = document.createElement("div");
      slot.className = "viewer-page";
      slot.dataset.page = String(n);
      slot.style.aspectRatio = String(1 / ratio);
      slot.setAttribute("aria-label", "Page " + n + " of " + pdf.numPages);
      slot.setAttribute("role", "img");
      pages.append(slot);
      state.observer.observe(slot);
    }
    if (pdf.numPages > count) body.append(Object.assign(document.createElement("p"), { className: "viewer-note", textContent: `Showing the first ${count} of ${pdf.numPages} pages. Download the file to see the rest.` }));
  }

  async function drawPdfPage(state, pdf, n, slot, cssWidth) {
    try {
      const page = await pdf.getPage(n);
      if (viewerState !== state) return;
      const base = page.getViewport({ scale: 1 });
      const scale = (cssWidth / base.width) * Math.min(window.devicePixelRatio || 1, 2);
      const viewport = page.getViewport({ scale });
      const canvas = document.createElement("canvas");
      canvas.width = Math.ceil(viewport.width);
      canvas.height = Math.ceil(viewport.height);
      slot.style.aspectRatio = String(base.width / base.height);
      await page.render({ canvasContext: canvas.getContext("2d"), viewport }).promise;
      if (viewerState === state) slot.replaceChildren(canvas);
    } catch (_) { /* a page that will not draw stays blank */ }
  }

  function closeViewer() {
    const state = viewerState;
    if (!state) return;
    viewerState = null;
    state.controller.abort();
    state.observer?.disconnect();
    if (state.pdf) void state.pdf.destroy();
    const body = $("viewer-body");
    for (const media of body.querySelectorAll("video, audio")) { media.pause(); media.removeAttribute("src"); media.load(); }
    body.textContent = "";
    if (state.url) URL.revokeObjectURL(state.url);
    $("viewer").hidden = true;
    if (state.trigger && state.trigger.isConnected) state.trigger.focus({ preventScroll: true });
  }

  // The bytes are the whole body; the metadata rides in headers. Not `api()` — that one sends and
  // expects JSON, and a file is neither.
  async function uploadFile(file, context) {
    const res = await fetch(cfg.postbox + "/v1/attachments", {
      method: "POST",
      headers: {
        authorization: "Bearer " + getToken(),
        accept: "application/json",
        "content-type": "application/octet-stream",
        "x-pigeonpost-identity": context.address,
        "x-pigeonpost-filename": headerSafe(file.name),
        "x-pigeonpost-media-type": headerSafe(file.type || "application/octet-stream"),
      },
      body: file,
      signal: context.signal,
    });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) throw new ApiError(res.status, body.error, body.detail);
    return body.id;
  }

  // A filename is somebody's text and headers are a line-based protocol. Anything that could end
  // the line, or that cannot survive one, is dropped rather than escaped.
  function headerSafe(text) {
    return String(text == null ? "" : text)
      .replace(/[^\u0020-\u007e]/g, "")
      .replace(/["\\]/g, "")
      .slice(0, 120);
  }

  async function api(path, opts) {
    const o = opts || {};
    const version = sessionVersion;
    const signal = o.signal || mailboxController.signal;
    const call = async () => {
      const res = await fetch(cfg.postbox + path, {
        method: o.method || "GET",
        headers: Object.assign(
          { authorization: `Bearer ${getToken()}`, accept: "application/json" },
          o.body ? { "content-type": "application/json" } : {},
        ),
        body: o.body ? JSON.stringify(o.body) : undefined,
        signal,
      });
      const body = await res.json().catch(() => ({}));
      if (!res.ok) throw new ApiError(res.status, body.error, body.detail);
      return body;
    };
    try {
      return await call();
    } catch (e) {
      if (version === sessionVersion && !signal.aborted && e instanceof ApiError && e.status === 401 && (await renewSession()) && version === sessionVersion && !signal.aborted) return call();
      throw e;
    }
  }

  // `?identity=` picks which mailbox an account acts as. Always sent once one is chosen: an account
  // with several mailboxes is refused rather than guessed at, which is the right call server-side
  // and a confusing error here if we let it happen.
  const withIdentity = (path, address = state.me && state.me.address) => {
    if (!address) return path;
    return path + (path.includes("?") ? "&" : "?") + "identity=" + encodeURIComponent(address);
  };

  // ---- messages in flight ---------------------------------------------------------------------

  // Sent messages now come from the server: the postbox keeps a copy of each one sealed to the
  // sender, and `/v1/inbox?include_sent=true` returns both halves of a conversation. What is left here
  // is only the gap between pressing send and the next poll — an optimistic row so the message
  // appears immediately, dropped once the server's own copy arrives.
  //
  // In memory on purpose. A message that failed to send is worth showing until the page is
  // reloaded and worth forgetting after: it does not exist anywhere else, and persisting it would
  // recreate the per-device history this replaced.
  const Pending = {
    rows: [],

    add(record) {
      this.rows.push(record);
      return record;
    },

    // Drop optimistic rows the server has now confirmed. Matched on the id the send call returned,
    // so a repeated message is never mistaken for its own echo.
    //
    // A send whose answer was lost has no id to match, but it has the `msg_ref` it was sent under,
    // and that reference names exactly one message in this mailbox. When the listing shows a copy
    // of ours carrying it, the send did commit: the "not confirmed" row is answered and goes.
    reconcile(serverIds, serverRefs = new Set()) {
      this.rows = this.rows.filter((r) => {
        if (r.status !== "sending" && r.msg_ref && serverRefs.has(r.mailbox + "\n" + r.msg_ref)) return false;
        return r.status === "failed" || !(r.sent_copy_id && serverIds.has(r.sent_copy_id));
      });
    },

    find(localId) {
      return this.rows.find((r) => r.local_id === localId) || null;
    },

    forMailbox(address) {
      return this.rows.filter((r) => r.mailbox === address);
    },

    clear() {
      this.rows = [];
    },
  };

  // ---- state --------------------------------------------------------------------------------

  function freshState() {
    return {
      me: null,          // { address, handle }
      identities: [],    // [{ address, handle, label }]
      openingInbox: false,
      inbound: [],       // messages from the server
      contacts: [],      // contact rows, including wildcards
      policy: null,
      openPeer: null,    // peer key of the open conversation
      openThread: null,  // thread id within that peer, or null while it has only the default one
      serverThreads: [], // /v1/threads, so a thread opened with nothing said in it still shows
      filter: "",
      showInfo: false,
      senderAction: null,
      contactsReady: false,
      contactsRetrying: false,
      archived: new Set(), // peers filed out of sight, from the server so it holds across devices
      viewingArchive: false,
      vocabulary: null,  // which verbs may be granted, per the server
      offline: false,    // the last attempt to reach the postbox did not arrive
      loading: false,
      hasLoaded: false,
      loadError: null,
      inboxRequest: 0,
      acceptedInboxRequest: 0,
      sectionRequests: {},
      leftView: "inbox", // inbox | calls | filtered | calendar (Phase 3B)
      calls: { entries: [], unseen: 0, filter: "all", next: null, loaded: false, loading: false, filtered: null },
      filtered: [],      // messages the server filed as filtered
    };
  }

  // How much larger the conversation text is than its default. Persisted per device, because it is
  // a property of the screen someone is reading on rather than of the account.
  const SIZE_KEY = "ppi_msg_scale";
  function messageScale() {
    const stored = Number(LS.getItem(SIZE_KEY));
    return stored >= 0.8 && stored <= 2 ? stored : 1;
  }
  function applyMessageScale(scale) {
    LS.setItem(SIZE_KEY, String(scale));
    document.documentElement.style.setProperty("--msg-scale", String(scale));
    const label = $("size-value");
    if (label) label.textContent = Math.round(scale * 100) + "%";
  }
  // Light, dark, or whatever the device is set to. Per device like the text size, and applied by
  // theme.js before the first paint; this is the same rule again for when it changes here (and for
  // a page that loaded without theme.js). "system" removes the attribute so the stylesheet's
  // prefers-color-scheme block decides.
  const THEME_KEY = "ppi_theme";
  const THEMES = ["system", "light", "dark"];
  function storedTheme() {
    let value = null;
    try { value = window.localStorage.getItem(THEME_KEY); } catch (_) { /* storage blocked */ }
    return THEMES.includes(value) ? value : "system";
  }
  function applyTheme(theme, persist = false) {
    if (!THEMES.includes(theme)) theme = "system";
    if (persist) {
      try {
        if (theme === "system") window.localStorage.removeItem(THEME_KEY);
        else window.localStorage.setItem(THEME_KEY, theme);
      } catch (_) { /* still applies for this visit */ }
    }
    const root = document.documentElement;
    if (theme === "system") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", theme);
    document.querySelector('meta[name="color-scheme"]')?.setAttribute("content", theme === "system" ? "light dark" : theme);
    syncThemeChoice();
  }
  function syncThemeChoice() {
    const current = document.documentElement.getAttribute("data-theme") || "system";
    for (const input of document.querySelectorAll('#theme-choice input[name="theme"]')) input.checked = input.value === current;
  }
  applyTheme(storedTheme());

  let state = freshState();
  let sessionVersion = 0;
  let identitiesRequest = 0;
  let mailboxController = new AbortController();

  function mailboxContext() {
    const owner = state, address = owner.me?.address, signal = mailboxController.signal;
    return { owner, address, signal, current: () => state === owner && state.me?.address === address && !signal.aborted };
  }

  function renderLoading() {
    $("inbox-status").hidden = !state.loading;
    $("inbox-status-text").textContent = state.hasLoaded ? "Updating inbox…" : "Loading inbox…";
    $("threads").setAttribute("aria-busy", String(state.loading));
    $("retry-inbox").hidden = !(state.offline || state.loadError);
    $("retry-inbox").disabled = state.loading;
    $("offline-banner").hidden = !(state.offline || state.loadError);
    $("offline-banner").textContent = state.loadError || (state.hasLoaded
      ? "Offline — showing what was last loaded." : "Could not reach your inbox. Try again when you’re connected.");
  }

  // Who a message is a conversation *with* — the other end, whichever way it went. The server says
  // so directly now; `sender_handle`/`from` are the pre-conversation shape, kept as a fallback so a
  // cached page against an older postbox still groups received mail correctly.
  const peerKeyOf = (msg) => msg.peer_handle || msg.peer || msg.sender_handle || msg.from;

  // A pending row is addressed however the user typed it. Normalise through what the server has
  // told us about this peer, so sending to /k/… and hearing back from /bekir/agent1 is one
  // conversation rather than two.
  function normalisePeer(target) {
    if (!target) return target;
    for (const m of state.inbound) {
      if ((m.peer === target || m.from === target) && (m.peer_handle || m.sender_handle)) {
        return m.peer_handle || m.sender_handle;
      }
      if (m.peer_handle === target || m.sender_handle === target) return target;
    }
    return target;
  }

  function contactFor(peer) {
    const exact = state.contacts.find((c) => c.peer === peer);
    if (exact) return exact;
    const parts = peer.split("/").filter(Boolean);
    if (parts.length >= 2) {
      const wildcard = "/" + parts[0] + "/*";
      return state.contacts.find((c) => c.peer === wildcard) || null;
    }
    return null;
  }

  // The account's own mailboxes, minus whichever one is currently acting. On a namespace these are
  // the sub-agents — /bekir/su_iam, /bekir/docdex — and they are the people the owner most wants to
  // talk to, so they are listed whether or not they have ever written.
  //
  // No entitlement check is needed or wanted here. An account holds the mailboxes it holds: a free
  // account has one anonymous mailbox and this list comes out empty on its own, while a namespace
  // owner sees their fleet. Gating it again in the browser would only add a second, weaker answer to
  // a question the server has already settled.
  function ownAgents() {
    const acting = state.me && state.me.address;
    return state.identities.filter((id) => id.address !== acting);
  }

  const identityKey = (id) => id.handle || id.address;

  function identityName(id) {
    if (id.handle) return displayName(id.handle);
    return id.label || displayName(id.address);
  }

  // Everything the app knows about who has written and who has been written to, newest last.
  function buildThreads() {
    const threads = new Map();
    const touch = (peer) => {
      if (!threads.has(peer)) {
        threads.set(peer, { peer, messages: [], unread: 0, held: 0, last: 0 });
      }
      return threads.get(peer);
    };

    // One list, both directions. `direction` comes from the server; its absence means an older
    // postbox that only ever returned received mail.
    for (const m of state.inbound) {
      const t = touch(peerKeyOf(m));
      if (m.direction === "out") {
        t.messages.push({
          kind: "out",
          id: m.message_id,
          at: m.sent_at || m.received_at,
          body: m.body,
          status: "sent",
          thread_id: m.thread_id,
          attachments: m.attachments,
          expires_at: m.expires_at || null, // Phase 3a: disappearing messages
          ...actionFields(m), // Phase 1e
        });
        continue;
      }
      t.messages.push({
        kind: "in",
        thread_id: m.thread_id,
        id: m.message_id,
        at: m.received_at,
        body: m.body,
        read: m.read,
        autonomy: m.autonomy,
        verb: m.verb,
        held_because: m.held_because,
        alias: m.alias,
        standing: m.sender_standing,
        tier: m.sender_tier,
        known: m.sender_known,
        matched: m.matched_contact,
        address: m.from,
        attachments: m.attachments,
        expires_at: m.expires_at || null, // Phase 3a: disappearing messages
        ...actionFields(m), // Phase 1e
      });
      if (!m.read) t.unread += 1;
      if (m.autonomy === "review" && m.verb) t.held += 1;
    }

    // Messages sent since the last poll, plus any that failed outright.
    for (const m of Pending.forMailbox(state.me ? state.me.address : "")) {
      const t = touch(normalisePeer(m.to));
      t.messages.push({
        kind: "out",
        id: m.local_id,
        at: m.at,
        body: m.body,
        status: m.status,
        thread_id: m.thread_id,
        attachments: m.attachments,
        msg_ref: m.msg_ref || null, // Phase 1e: a reply or edit can name it before the poll
        reply_to: m.reply_to || null,
        uncertain: Boolean(m.uncertain),
      });
    }

    // A contact you have never exchanged mail with still deserves a row — that is how you start a
    // conversation with an agent you have only been told about. Wildcards are policy, not people.
    for (const c of state.contacts) {
      if (c.peer.endsWith("/*")) continue;
      touch(c.peer);
    }

    // Mark the threads that are your own agents, but do not *create* rows for them.
    //
    // This list is correspondence. A fleet of a dozen agents that have never written to each other
    // would otherwise fill it with a dozen empty conversations, burying the handful that are real —
    // and the more agents someone runs, the worse it gets. Every mailbox on the account is still one
    // click away in the identity picker, which is where "switch to my other mailbox" belongs;
    // that is a different question from "who have I been talking to".
    for (const id of ownAgents()) {
      const t = threads.get(identityKey(id));
      if (!t) continue;
      t.mine = true;
      t.identity = id;
    }

    // One row per message, whatever it came from.
    //
    // A message reaches this list from two places — the server's listing and the optimistic row
    // held between pressing send and the next poll — and they are retired against each other by id.
    // That reconciliation is correct, but it is timing-dependent, and a conversation showing the
    // same sentence twice is the kind of wrong that makes someone distrust the whole page. Identity
    // is the message id, so enforce it here where every source has already been merged, rather than
    // trusting each source to have behaved.
    for (const t of threads.values()) {
      const seen = new Set();
      t.messages = t.messages.filter((m) => {
        const key = m.id;
        if (!key) return true; // a failed local row has no server id and is still worth showing
        if (seen.has(key)) return false;
        seen.add(key);
        return true;
      });
      t.messages.sort((a, b) => a.at - b.at);
      t.last = t.messages.length ? t.messages[t.messages.length - 1].at : 0;
      t.contact = contactFor(t.peer);
    }

    // Recency first, as a messenger does — but a fleet the owner has never written to would then
    // sort in creation order, which is arbitrary. Fall back to the name so an untouched list is at
    // least alphabetical and stays put between renders.
    return [...threads.values()].sort((a, b) => {
      if (b.last !== a.last) return b.last - a.last;
      return threadName(a).localeCompare(threadName(b));
    });
  }

  function threadName(t) {
    if (t.mine && t.identity) return identityName(t.identity);
    return (t.contact && t.contact.alias) || displayName(t.peer);
  }

  // ---- rendering ----------------------------------------------------------------------------

  function render() {
    const signedIn = Boolean(getToken());
    // A session can exist before its first mailbox. The setup controls live in #signin, so
    // hiding that panel just because a token exists strands every new account.
    const ready = signedIn && Boolean(state.me);
    $("signin").hidden = ready;
    $("signin-btn").hidden = signedIn;
    $("app").hidden = !ready;
    if (!signedIn) {
      $("create-inbox-btn").hidden = true;
      $("signin-note").textContent = "";
    }
    if (!ready) return;
    renderMe();
    renderLoading();
    renderThreadList();
    renderSubs();
    renderThread();
    window.PigeonpostGroups?.render(); // Phase 3a
    window.PigeonpostCalendar?.render(); // Phase 3B
  }

  function renderSubs() {
    const pane = $("pane-subs");
    const visible = subsVisible();
    // Hidden rather than empty: an unused column of whitespace between the names and the messages
    // is a worse answer than not having the column.
    pane.hidden = !visible;
    $("app").setAttribute("data-subs", String(visible));
    // On a phone this pane is a screen, and it is open when a peer is selected but no thread has
    // been picked yet. On a wide screen both are on show at once, so "open" only tracks the phone.
    pane.setAttribute(
      "data-open",
      String(visible && onPhone() && state.openThread === null),
    );
    if (!visible) return;

        const conversation = buildThreads().find((t) => t.peer === state.openPeer);
    $("subs-peer").textContent = conversation
      ? threadName(conversation)
      : displayName(state.openPeer);

    const list = $("subs");
    list.textContent = "";
    const current = currentSubthread(state.openPeer);
    for (const t of subthreadsFor(state.openPeer)) {
      const li = document.createElement("li");
      const row = document.createElement("button");
      row.type = "button";
      row.className = "sub-row";
      if (current && t.id === current.id) row.setAttribute("aria-current", "true");

      const title = document.createElement("span");
      title.className = "sub-title" + (t.title ? "" : " is-default");
      title.textContent = subthreadName(t);

      const meta = document.createElement("div");
      meta.className = "sub-meta";
      const when = document.createElement("span");
      when.textContent = t.messages.length ? listTime(t.last) : "no messages yet";
      meta.append(when);
      if (t.unread) {
        const badge = document.createElement("span");
        badge.className = "sub-unread";
        badge.textContent = String(t.unread);
        meta.append(badge);
      }

      row.append(title, meta);
      row.addEventListener("click", () => openSubthread(t.id));
      li.append(row);
      list.append(li);
    }
  }

  const addressCopyIcon = '<svg viewBox="0 0 20 20" aria-hidden="true"><rect x="6" y="6" width="11" height="11" rx="2" fill="none" stroke="currentColor" stroke-width="1.5"/><path d="M13 6V4a1 1 0 00-1-1H4a1 1 0 00-1 1v8a1 1 0 001 1h2" fill="none" stroke="currentColor" stroke-width="1.5"/></svg>';
  const addressCopiedIcon = '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M4 10l4 4 8-8" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>';
  const addressCopyTimers = new WeakMap();

  function configureAddressCopy(button, address) {
    clearTimeout(addressCopyTimers.get(button));
    button.dataset.address = address || "";
    button.disabled = !address;
    button.innerHTML = addressCopyIcon;
    button.title = "Copy address";
    button.setAttribute("aria-label", address ? `Copy address ${address}` : "Copy address");
    button.onclick = async (event) => {
      // Replacing the clicked SVG detaches the event target before document's outside-click
      // handler runs. Keep a copy click inside the mailbox picker even while its icon changes.
      event.stopPropagation();
      if (!address) return;
      clearTimeout(addressCopyTimers.get(button));
      button.innerHTML = addressCopyIcon;
      try {
        await navigator.clipboard.writeText(address);
        if (button.dataset.address !== address) return;
        button.innerHTML = addressCopiedIcon;
        button.title = "Address copied";
        toast("Address copied");
        addressCopyTimers.set(button, setTimeout(() => configureAddressCopy(button, address), 2000));
      } catch {
        toast("Couldn’t copy. Select the address and copy it manually.");
      }
    };
  }

  function renderMe() {
    if (!state.me) return;
    const name = state.me.handle || state.me.address;
    $("me-name").textContent = displayName(name);
    $("identity-btn").title = "Change mailbox · " + name;
    $("identity-btn").dataset.address = name;
    $("identity-btn").setAttribute("aria-label", "Acting as " + displayName(name) + ". Change mailbox");
    paintAvatar($("me-avatar"), name);
    $("identity-btn").disabled = false;
  }

  function orderedIdentities() {
    const primary = state.identities.find(id => id.handle === cfg.primaryNamespace + "/main")
      || state.identities.find(id => id.handle?.startsWith("/github/"))
      || state.identities.find(id => /^\/[^/]+\/main$/.test(id.handle || ""));
    const parts = (id) => (id.handle || "").split("/").filter(Boolean);
    return [...state.identities].sort((a, b) => {
      if (a === primary || b === primary) return a === b ? 0 : a === primary ? -1 : 1;
      if (Boolean(a.handle) !== Boolean(b.handle)) return a.handle ? -1 : 1;
      const aa = parts(a), bb = parts(b);
      const root = (aa[0] || "").localeCompare(bb[0] || "");
      if (root) return root;
      const rank = (p) => p.length === 1 || (p.length === 2 && p[1] === "main") ? 0 : 1;
      return rank(aa) - rank(bb) || identityKey(a).localeCompare(identityKey(b));
    });
  }

  function renderIdentityMenu() {
    const menu = $("identity-menu");
    menu.textContent = "";
    for (const id of orderedIdentities()) {
      const li = document.createElement("li");
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "identity-option";
      btn.setAttribute("aria-pressed", String(state.me && id.address === state.me.address));

      const av = document.createElement("span");
      av.className = "avatar";
      paintAvatar(av, id.handle || id.address, identityName(id));
      av.style.width = av.style.height = "30px";
      av.style.fontSize = "11.5px";

      const text = document.createElement("span");
      text.className = "mi-text";
      const nm = document.createElement("span");
      nm.className = "mi-name";
      nm.textContent = id.handle ? displayName(id.handle) : (id.label || "unnamed mailbox");
      const sub = document.createElement("span");
      sub.className = "mi-sub";
      sub.textContent = id.handle || id.address;
      text.append(nm, sub);

      btn.append(av, text);
      btn.onclick = () => {
        menu.hidden = true;
        $("identity-btn").setAttribute("aria-expanded", "false");
        if (!state.me || id.address !== state.me.address) switchIdentity(id);
      };
      const copy = document.createElement("button");
      copy.type = "button";
      copy.className = "icon-btn copy-address";
      configureAddressCopy(copy, identityKey(id));
      li.append(btn, copy);
      menu.append(li);
    }
  }

  function renderThreadList() {
    if (renderSideViews()) return;
    const list = $("threads");
    // The archive is a different view of the same list, not a different list: filed conversations
    // are exactly the ones the inbox omits, so one predicate decides both and they can never
    // disagree about where a thread lives.
    const threads = buildThreads().filter(
      (t) => state.archived.has(t.peer) === state.viewingArchive,
    );
    $("archive-banner").hidden = !state.viewingArchive;
    $("threads-empty").textContent = state.viewingArchive
      ? "Nothing archived."
      : "No mail yet. When an agent writes to this mailbox, it appears here.";
    const needle = state.filter.trim().toLowerCase();
    const shown = needle
      ? threads.filter((t) => {
          const c = t.contact;
          const hay = [t.peer, threadName(t), c && c.alias, t.identity && t.identity.label,
            ...t.messages.slice(-8).map((m) => m.body)]
            .filter(Boolean).join(" ").toLowerCase();
          return hay.includes(needle);
        })
      : threads;

    list.textContent = "";
    $("threads-empty").hidden = shown.length > 0 || state.loading || Boolean(state.loadError) || state.offline;
    if (needle && !shown.length) $("threads-empty").textContent = "No matching conversations. Try another name or address.";

    // Your own agents sit at the top under their own heading — the ones you have actually
    // corresponded with. A namespace owner's own fleet is who they most want to find, and burying
    // it below every stranger who ever wrote in would be the wrong way round.
    const mine = shown.filter((t) => t.mine);
    const others = shown.filter((t) => !t.mine);

    if (mine.length) {
      list.append(groupHeading("Your agents"));
      for (const t of mine) list.append(threadRow(t));
    }
    if (others.length) {
      if (mine.length) list.append(groupHeading("Conversations"));
      for (const t of others) list.append(threadRow(t));
    }
  }

  function groupHeading(label) {
    const li = document.createElement("li");
    li.className = "group-head";
    li.setAttribute("role", "presentation");
    li.textContent = label;
    return li;
  }

  function threadRow(t) {
    const li = document.createElement("li");
    const row = document.createElement("button");
    row.type = "button";
    row.className = "thread-row";
    if (t.peer === state.openPeer) row.setAttribute("aria-current", "true");

    const av = document.createElement("span");
    av.className = "avatar";
    paintAvatar(av, t.peer, threadName(t));

    const text = document.createElement("div");
    text.className = "tr-text";

    const top = document.createElement("div");
    top.className = "tr-top";
    const name = document.createElement("span");
    name.className = "tr-name";
    name.textContent = threadName(t);
    const time = document.createElement("span");
    time.className = "tr-time";
    time.textContent = t.last ? listTime(t.last) : "";
    top.append(name, time);

    const bottom = document.createElement("div");
    bottom.className = "tr-bottom";
    const preview = document.createElement("span");
    preview.className = "tr-preview";
    const last = t.messages[t.messages.length - 1];
    // A silent agent should say what it is, not "no messages yet" — the handle is the useful fact,
    // and an unnamed mailbox is worth flagging because handle-based trust will never match it.
    preview.textContent = last
      ? (last.kind === "out" ? "You: " : "") + previewOf(last)
      : (t.mine && t.identity && !t.identity.handle ? "No handle — fleet trust will not match it" : t.peer);
    const badges = document.createElement("span");
    badges.className = "tr-badges";
    if (t.held) badges.append(pill("held", "pill-held"));
    if (t.contact && t.contact.admission === "block") badges.append(pill("blocked", "pill-blocked"));
    if (t.unread) {
      const dot = document.createElement("span");
      dot.className = "dot";
      dot.textContent = String(t.unread);
      badges.append(dot);
    }
    bottom.append(preview, badges);

    text.append(top, bottom);
    row.append(av, text);
    row.onclick = () => openThread(t.peer);
    li.append(row);
    return li;
  }

  function pill(label, className) {
    const el = document.createElement("span");
    el.className = "pill " + className;
    el.textContent = label;
    return el;
  }

  // A scoped request is JSON on the wire. In a list it should read as what it asks for.
  function requestEnvelope(body) {
    if (!body || body[0] !== "{") return null;
    try {
      const parsed = JSON.parse(body);
      if (parsed && parsed.v === 1 && typeof parsed.verb === "string") return parsed;
    } catch (_) { /* prose that happens to start with a brace */ }
    return null;
  }

  function previewOf(m) {
    const envelope = requestEnvelope(m.body);
    if (envelope) {
      // A narrow verb is the information: "asks to run tests" says what a peer wants. But every
      // message these clients send is `full_access`, so previewing that would make every line of
      // the list identical — there the words somebody typed are the information.
      if (envelope.verb === "full_access" && envelope.note) {
        return String(envelope.note).replace(/\s+/g, " ").slice(0, 140);
      }
      return "asks to " + envelope.verb.replace(/_/g, " ");
    }
    const reply = autoReply(m.body);
    return plainText(reply ? reply.body : m.body).slice(0, 140);
  }

  function renderThread() {
    // With several threads on a peer the phone stops at the list of them, so "a peer is selected"
    // is no longer the same question as "there are messages to show".
    const open = Boolean(state.openPeer);
    $("pane-thread").dataset.open = String((open && (!onPhone() || state.openThread !== null || !subthreadsFor(state.openPeer).length)) || Boolean(window.PigeonpostGroups?.isOpen()) || Boolean(window.PigeonpostCalendar?.isOpen())); // Phase 3B: calendar
    $("thread-head").hidden = !open;
    window.PigeonpostCalls?.renderHead();
    $("composer").hidden = !open;
    $("thread-empty").hidden = open;
    if (!(open && state.showInfo) && !$("peer-info-sheet").hidden) closeSheet("peer-info-sheet");
    for (const id of ["peer-name-btn", "peer-info-btn"]) $(id).setAttribute("aria-expanded", String(open && state.showInfo));

    // The same control both ways: in the archive it is the way back out, which is where somebody
    // looking at a filed conversation would go looking for it.
    const filed = open && state.archived.has(state.openPeer);
    const archiveBtn = $("archive-btn");
    archiveBtn.hidden = !open;
    archiveBtn.title = filed ? "Move back to your inbox" : "Archive this conversation";
    archiveBtn.setAttribute("aria-label", archiveBtn.title);

    if (!open) {
      $("blocked-banner").hidden = true;
      resetConversationView();
      $("messages").textContent = "";
      $("load-older").hidden = true;
      $("jump-latest").hidden = true;
      $("find-bar").hidden = true;
      return;
    }

    const conversation = buildThreads().find((t) => t.peer === state.openPeer)
      || { peer: state.openPeer, messages: [], contact: contactFor(state.openPeer) };
    // Narrowed to the chosen thread, which is the whole point of having them: an answer about the
    // deploy should not be read next to last week's unrelated question.
    const showing = subsVisible() ? currentSubthread(state.openPeer) : null;
    const narrowed = showing
      ? { ...conversation, messages: conversation.messages.filter((m) => (m.thread_id || "") === showing.id) }
      : conversation;
    // Missed calls read as small lines in the conversation they belong to, in the default thread.
    const thread = !showing || showing.is_default
      ? { ...narrowed, messages: withMissedCalls(narrowed.messages, conversation) }
      : narrowed;
    renderBlockedBanner(conversation);

    $("peer-name").textContent = threadName(thread);
    $("peer-name-btn").setAttribute("aria-label", "About " + threadName(thread));
    $("peer-sub").textContent = thread.mine ? thread.peer + " · your mailbox" : thread.peer;
    paintAvatar($("peer-avatar"), thread.peer, threadName(thread));
    $("delete-thread-btn").hidden = !showing?.id;
    renderMessages(thread.messages, showing?.id || "");

    if (state.showInfo) renderPeerInfo(conversation);

  }

  // A small DOM window, with an explicit reading position. Refreshing mail never grants permission
  // to move that position. Search reads the snapshot but renders only the area around its hit.
  const PAGE_SIZE = 10;
  let conversationView = null;
  let findTimer = null;
  let layoutFrame = null;

  function resetConversationView() {
    clearTimeout(findTimer);
    if (layoutFrame !== null) cancelAnimationFrame(layoutFrame);
    layoutFrame = null;
    conversationView = null;
    $("find-input").value = "";
    $("find-bar").hidden = true;
    $("find-btn").setAttribute("aria-expanded", "false");
  }

  function readingAnchor() {
    const top = $("thread-scroll").getBoundingClientRect().top;
    for (const row of $("messages").querySelectorAll("[data-message-id]")) {
      const box = row.getBoundingClientRect();
      if (box.bottom > top) return { id: row.dataset.messageId, offset: box.top - top };
    }
    return null;
  }

  function messageRow(id) {
    return [...$("messages").querySelectorAll("[data-message-id]")].find(row => row.dataset.messageId === id);
  }

  function positionMessages(view, anchor, target) {
    if (conversationView !== view) return;
    const scroll = $("thread-scroll");
    const row = target ? messageRow(target) : anchor && messageRow(anchor.id);
    if (target && row) {
      scroll.scrollTop += row.getBoundingClientRect().top - scroll.getBoundingClientRect().top
        - (scroll.clientHeight - row.getBoundingClientRect().height) / 2;
    } else if (view.follow) {
      scroll.scrollTop = scroll.scrollHeight;
    } else if (row) {
      scroll.scrollTop += row.getBoundingClientRect().top - scroll.getBoundingClientRect().top - anchor.offset;
    }
    view.lastTop = scroll.scrollTop;
    view.anchor = readingAnchor();
    $("jump-latest").hidden = view.follow && !view.endId;
  }

  function scheduleMessageLayout() {
    if (!conversationView || layoutFrame !== null) return;
    const view = conversationView;
    layoutFrame = requestAnimationFrame(() => {
      layoutFrame = null;
      if (conversationView !== view) return;
      positionMessages(view, view.anchor);
    });
  }

  function highlightMessage(row, query, current) {
    row.classList.toggle("found-message", current);
    if (!query || !row.querySelector(".bubble")) return;
    // Build marks from text nodes, never HTML from the query or message. Inline markup stays inert.
    const walker = document.createTreeWalker(row.querySelector(".bubble"), NodeFilter.SHOW_TEXT);
    const nodes = [];
    while (walker.nextNode()) {
      if (!walker.currentNode.parentElement.closest(".meta, .decision")) nodes.push(walker.currentNode);
    }
    for (const node of nodes) {
      const value = node.textContent, lower = value.toLocaleLowerCase(), needle = query.toLocaleLowerCase();
      let at = lower.indexOf(needle), from = 0;
      if (at < 0) continue;
      const fragment = document.createDocumentFragment();
      while (at >= 0) {
        fragment.append(document.createTextNode(value.slice(from, at)));
        const mark = document.createElement("mark");
        mark.textContent = value.slice(at, at + needle.length);
        fragment.append(mark);
        from = at + needle.length;
        at = lower.indexOf(needle, from);
      }
      fragment.append(document.createTextNode(value.slice(from)));
      node.replaceWith(fragment);
    }
  }

  function renderMessages(messages, subject) {
    const key = JSON.stringify([state.me.address, state.openPeer, subject]);
    if (conversationView?.key !== key) {
      resetConversationView();
      conversationView = { key, limit: PAGE_SIZE, follow: true, endId: null, query: "", hits: [],
        hit: 0, count: messages.length, cache: new Map(), signature: "", anchor: null, lastTop: 0 };
    }
    const view = conversationView;
    view.messages = messages;
    if (!view.follow && !view.endId) view.limit += Math.max(0, messages.length - view.count);
    view.count = messages.length;
    view.hits = view.query ? messages.filter(m => plainText(copyTextOf(m)).toLocaleLowerCase().includes(view.query.toLocaleLowerCase())) : [];
    view.hit = Math.min(view.hit, Math.max(0, view.hits.length - 1));
    const found = view.hits[view.hit]?.id;
    if (view.seek && found) {
      const index = messages.findIndex(m => m.id === found);
      view.endId = messages[Math.min(messages.length - 1, index + 5)].id;
      view.limit = PAGE_SIZE;
      view.follow = false;
      view.target = found;
    }
    view.seek = false;
    const endIndex = view.endId ? messages.findIndex(m => m.id === view.endId) : -1;
    const end = endIndex < 0 ? messages.length : endIndex + 1;
    const start = Math.max(0, end - view.limit);
    const visible = messages.slice(start, end);
    view.start = start;
    $("load-older").hidden = start === 0;
    $("find-count").textContent = view.query ? (view.hits.length ? `${view.hit + 1} of ${view.hits.length}` : "No matches") : "";
    $("find-prev").disabled = $("find-next").disabled = !view.hits.length;
    const signature = JSON.stringify([visible, view.query, found]);
    if (signature !== view.signature) {
      const anchor = view.anchor || readingAnchor(), list = $("messages"), rows = [], cache = new Map();
      const flagsNow = actionFlags(), flagsKey = JSON.stringify(flagsNow), nowSecs = Date.now() / 1000; // Phase 1e
      let previous = null;
      for (const m of visible) {
        if (!previous || !sameDay(previous.at, m.at)) {
          // Day separators are reused like bubbles. A fresh one on every refresh made the loop
          // below move every message after it, and moving a node drops any text selection inside
          // it: a reader dragging across a reply lost the selection whenever mail arrived.
          const day = dayLabel(m.at), sepKey = "\u0000day:" + day;
          let sep = view.cache.get(sepKey)?.row;
          if (!sep) {
            const label = document.createElement("span");
            sep = document.createElement("li");
            sep.className = "sep";
            label.className = "daybreak";
            label.textContent = day;
            sep.append(label);
          }
          rows.push(sep);
          cache.set(sepKey, { row: sep });
        }
        // `read` flips when a message is displayed and acknowledged, and changes nothing drawn, so
        // it must not rebuild the bubble (and lose a selection in it) a moment after it appears.
        const { read: _read, ...drawn } = m;
        const fingerprint = JSON.stringify([drawn, view.query, m.id === found, flagsKey, editable(m, nowSecs, flagsNow)]);
        let entry = view.cache.get(m.id);
        if (entry?.fingerprint !== fingerprint) {
          const row = messageNode(m);
          highlightMessage(row, view.query, m.id === found);
          entry = { row, fingerprint };
        }
        rows.push(entry.row);
        cache.set(m.id, entry);
        previous = m;
      }
      // Reuse unchanged bubbles so a refresh preserves selection and attachment/copy controls.
      // Rows leaving the window go first: inserting in front of one that is about to go would move
      // every row after it, and moving a node drops a text selection inside it. A followed
      // conversation slides its window by one on every arrival, so this is the common case.
      const keep = new Set(rows);
      for (const child of [...list.children]) if (!keep.has(child)) child.remove();
      rows.forEach((row, index) => { if (list.children[index] !== row) list.insertBefore(row, list.children[index] || null); });
      while (list.children.length > rows.length) list.lastElementChild.remove();
      prunePreviews();
      view.cache = cache;
      view.signature = signature;
      positionMessages(view, anchor, view.target);
      view.target = null;
      scheduleMessageLayout();
      const context = mailboxContext();
      queueMicrotask(() => { if (context.current() && conversationView === view) ackVisible(); });
    }
    $("jump-latest").hidden = view.follow && !view.endId;
  }

  function loadOlderMessages() {
    const view = conversationView;
    if (!view || !view.start) return;
    view.anchor = readingAnchor();
    view.follow = false;
    view.limit += PAGE_SIZE;
    renderThread();
    ackVisible();
  }

  function jumpToLatest() {
    const view = conversationView;
    if (!view) return;
    view.follow = true;
    view.endId = null;
    view.limit = PAGE_SIZE;
    view.query = "";
    $("find-input").value = "";
    renderThread();
    positionMessages(view);
    ackVisible();
  }

  function openFind() {
    if (!conversationView) return;
    $("find-bar").hidden = false;
    $("find-btn").setAttribute("aria-expanded", "true");
    $("find-input").focus();
  }

  function closeFind() {
    clearTimeout(findTimer);
    if (conversationView) conversationView.query = "";
    $("find-input").value = "";
    $("find-bar").hidden = true;
    $("find-btn").setAttribute("aria-expanded", "false");
    renderThread();
    $("find-btn").focus();
  }

  function findStep(delta) {
    const view = conversationView;
    if (!view?.hits.length) return;
    view.hit = (view.hit + delta + view.hits.length) % view.hits.length;
    view.seek = true;
    renderThread();
    ackVisible();
  }

  const drafts = new Map();
  const sendingDrafts = new Set();
  function composerKey() {
    return state.openPeer ? JSON.stringify([state.me?.address, state.openPeer, currentSubthread(state.openPeer)?.id || ""]) : null;
  }
  function stashDraft() {
    const key = composerKey();
    if (!key) return;
    if ($("compose").value || staged.length) drafts.set(key, { text: $("compose").value, files: [...staged] });
    else drafts.delete(key);
  }
  function resetComposer() {
    $("compose").value = "";
    $("compose").style.height = "auto";
    $("compose").disabled = false;
    $("send-btn").disabled = true;
    staged.length = 0;
    renderStaged();
  }
  function restoreDraft() {
    if (composeMode) setComposeMode(null); // Phase 1e
    resetComposer();
    const key = composerKey(), draft = drafts.get(key);
    if (draft) { $("compose").value = draft.text; staged.push(...draft.files); renderStaged(); }
    $("compose").disabled = sendingDrafts.has(key);
    $("compose").dispatchEvent(new Event("input"));
  }

  let deletingThread = null;
  function askDeleteThread() {
    const thread = currentSubthread(state.openPeer);
    if (!thread?.id) return;
    deletingThread = { context: mailboxContext(), peer: state.openPeer, thread };
    $("delete-thread-detail").textContent = `Delete “${subthreadName(thread)}” and its ${thread.messages.length} message${thread.messages.length === 1 ? "" : "s"} from this mailbox? The other person keeps their copy.`;
    $("delete-thread-error").hidden = true;
    $("delete-thread-confirm").disabled = false;
    openSheet("delete-thread-sheet");
    $("delete-thread-cancel").focus();
  }
  async function deleteThread() {
    const target = deletingThread, button = $("delete-thread-confirm");
    if (!target?.context.current() || button.disabled) return;
    button.disabled = true;
    try {
      await api(withIdentity("/v1/threads/" + encodeURIComponent(target.thread.id), target.context.address), { method: "DELETE" });
      if (!target.context.current()) return;
      drafts.delete(JSON.stringify([target.context.address, target.peer, target.thread.id]));
      Pending.rows = Pending.rows.filter(row => row.mailbox !== target.context.address || row.thread_id !== target.thread.id);
      state.serverThreads = state.serverThreads.filter(t => t.thread_id !== target.thread.id);
      state.inbound = state.inbound.filter(m => m.thread_id !== target.thread.id);
      state.acceptedInboxRequest = ++state.inboxRequest;
      if (deletingThread === target) closeSheet("delete-thread-sheet");
      const selected = state.openPeer === target.peer && state.openThread === target.thread.id;
      if (selected) {
        state.openThread = subthreadsFor(target.peer)[0]?.id ?? null;
        resetConversationView();
      }
      render();
      if (selected) restoreDraft();
      await loadAll();
    } catch (e) {
      if (!target.context.current() || deletingThread !== target) return;
      $("delete-thread-error").textContent = "Could not delete this thread. Please try again.";
      $("delete-thread-error").hidden = false;
    } finally {
      if (target.context.current() && deletingThread === target) button.disabled = false;
    }
  }

  function messageNode(m) {
    if (m.kind === "system") return systemLine(m);
    const li = document.createElement("li");
    li.dataset.messageId = m.id;
    if (m.kind === "out") li.className = "mine";
    if (m.expires_at) { li.classList.add("ephemeral"); li.dataset.expires = String(m.expires_at); } // Phase 3a

    const bubble = document.createElement("div");
    bubble.className = "bubble";

    // Both directions. A request you sent is still a request, and showing it as raw JSON in your
    // own thread would make the composer look like it had done something strange.
    const envelope = requestEnvelope(m.body);

    if (envelope) {
      const req = document.createElement("div");
      req.className = "request";
      // The verb is a header worth showing only when it says something. Everything these clients
      // send is `full_access`, so labelling every message with it is a banner repeated on every
      // line — it tells the reader nothing they did not already know.
      if (envelope.verb !== "full_access") {
        const verb = document.createElement("div");
        verb.className = "verb";
        verb.textContent = verbTitle(envelope.verb);
        req.append(verb);
      }
      // Web and mobile compose `args: {task: text}` alongside `note: text` — the same sentence
      // twice, because the agent reads the arg and a human reads the note. Printing both would
      // show the reader their own words echoed under a slab of JSON, so the slab is dropped
      // whenever it carries nothing the note does not already say.
      const argKeys = envelope.args ? Object.keys(envelope.args) : [];
      const note = String(envelope.note == null ? "" : envelope.note).trim();
      const echoesNote =
        argKeys.length === 1 &&
        typeof envelope.args[argKeys[0]] === "string" &&
        envelope.args[argKeys[0]].trim() === note &&
        note !== "";
      if (argKeys.length && !echoesNote) {
        const args = document.createElement("pre");
        args.className = "args";
        args.textContent = JSON.stringify(envelope.args, null, 2);
        req.append(args);
      }
      if (envelope.note) {
        const why = document.createElement("p");
        why.className = "why";
        why.textContent = envelope.note;
        req.append(why);
      }
      const decision = document.createElement("div");
      decision.className = "decision";
      if (m.autonomy === "auto") {
        decision.append(pill("auto", "pill-auto"));
      } else {
        decision.append(pill("held", "pill-held"));
        if (m.held_because) {
          const reason = document.createElement("span");
          reason.className = "reason";
          reason.textContent = heldReason(m.held_because);
          decision.append(reason);
        }
      }
      if (m.kind === "in") req.append(decision);
      bubble.append(req);
    } else {
      // Bodies are other agents' text — rendered as markdown, and still never as markup: see
      // `renderMarkdown`, which builds nodes and sets textContent on every one of them.
      const text = document.createElement("div");
      text.className = "text";
      const reply = autoReply(m.body);
      if (reply) {
        // The two header lines are machinery. They say the same thing on every unattended reply
        // and nobody wants them in the message, or on the clipboard.
        const caption = document.createElement("div");
        caption.className = "auto-caption";
        caption.textContent = reply.answered
          ? "answered " + verbTitle(reply.answered).toLowerCase() + ", unattended"
          : "answered unattended";
        text.append(caption);
        renderMarkdown(text, reply.body);
      } else {
        renderMarkdown(text, m.body);
      }
      bubble.append(text);
    }

    // Passive, bounded thumbnails accompany the original authenticated download.
    if (Array.isArray(m.attachments) && m.attachments.length) {
      const files = document.createElement("ul");
      files.className = "files";
      for (const file of m.attachments) {
        const li = document.createElement("li");
        const link = document.createElement("a");
        link.className = "file";
        link.href = cfg.postbox + "/v1/attachments/" + encodeURIComponent(file.id);
        link.textContent = file.filename || "attachment";
        // The token cannot ride in a plain link, so the bytes are fetched with it. Same reason the
        // endpoint is authenticated at all: a file is not public because its id is known.
        const context = mailboxContext();
        const load = async signal => {
          if (!context.current()) throw new Error("Mailbox changed");
          const res = await fetch(link.href, { headers: { authorization: "Bearer " + getToken(),
            "x-pigeonpost-identity": context.address }, signal: signal || context.signal });
          if (!res.ok || !context.current()) throw new Error("Unavailable");
          const blob = await res.blob();
          if (!context.current()) throw new Error("Mailbox changed");
          return blob;
        };
        const item = { name: file.filename || "attachment", type: file.media_type, bytes: file.bytes, load, context };
        const format = previewType(file.media_type, file.filename);
        const viewable = format && Number.isFinite(file.bytes) && file.bytes > 0 && file.bytes <= VIEWER_MAX_BYTES;
        // Something the app can show opens in the app; anything else downloads as before.
        link.addEventListener("click", (event) => {
          event.preventDefault();
          if (viewable) openViewer(item, format, link); else downloadAttachment(item);
        });
        const size = document.createElement("span");
        size.className = "file-size";
        size.textContent = readableBytes(file.bytes);
        const save = document.createElement("button");
        save.type = "button";
        save.className = "file-download";
        save.title = "Download " + item.name;
        save.setAttribute("aria-label", "Download " + item.name);
        save.innerHTML = DOWNLOAD_ICON;
        save.addEventListener("click", () => downloadAttachment(item));
        addPreview(li, { name: file.filename, type: file.media_type, bytes: file.bytes, load, onOpen: viewable ? () => openViewer(item, format, link) : null });
        const label = document.createElement("div"); label.className = "file-label";
        label.append(link, size, save); li.append(label);
        files.append(li);
      }
      bubble.append(files);
    }

    const meta = document.createElement("div");
    meta.className = "meta";
    if (m.kind === "out" && m.status === "failed") {
      const failed = document.createElement("span");
      failed.className = "failed";
      // Two different facts. Refused means the postbox answered no; not confirmed means the answer
      // never arrived, and the message may well be with them already — so the row says which, and
      // offers the same two ways out either way: send it again, or let it go.
      failed.textContent = m.uncertain ? "not confirmed" : "not sent";
      meta.append(failed);
      if (Pending.find(m.id)) {
        const retry = document.createElement("button");
        retry.type = "button";
        retry.className = "linkish send-retry";
        retry.textContent = "Retry";
        retry.addEventListener("click", (e) => { e.stopPropagation(); retrySend(m.id); });
        const discard = document.createElement("button");
        discard.type = "button";
        discard.className = "linkish send-discard";
        discard.textContent = "Discard";
        discard.addEventListener("click", (e) => { e.stopPropagation(); discardSend(m.id); });
        meta.append(retry, discard);
      }
    }
    if (m.kind === "in") meta.append(blockSenderButton(m));
    // Phase 1e: "edited", the time and the ticks sit together at the bubble's bottom right.
    const spacer = document.createElement("span");
    spacer.className = "meta-spacer";
    meta.append(spacer);
    if (m.edited_at) {
      const edited = document.createElement("span");
      edited.className = "edited";
      edited.textContent = "edited";
      edited.title = "Edited " + clockTime(m.edited_at);
      meta.append(edited);
    }
    const time = document.createElement("span");
    time.className = "time";
    time.textContent = clockTime(m.at);
    meta.append(time);
    const ticks = tickNode(tickState(m, actionFlags()), m.receipt && m.receipt.seen_at);
    if (ticks) {
      if (m.kind === "out" && m.msg_ref && m.status !== "sending") {
        ticks.classList.add("ticks-button");
        ticks.tabIndex = 0;
        ticks.addEventListener("click", () => openMessageInfo(m));
        ticks.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); openMessageInfo(m); } });
      }
      meta.append(ticks);
    }
    bubble.append(meta);
    if (m.reply_to) bubble.prepend(replyQuoteNode(m));
    bubble.addEventListener("contextmenu", (e) => openMessageMenu(e, m));

    // The action row lives under the bubble, outside it — the owner's reference look. Received
    // mail offers copy and reply, your own offers edit (while it still can be) and copy.
    const stack = document.createElement("div");
    stack.className = "msg-stack";
    const actions = document.createElement("div");
    actions.className = "msg-actions";
    const flags = actionFlags();
    const canReply = flags.reply && isMsgRef(m.msg_ref) && m.status !== "sending" && m.status !== "failed";
    if (m.kind === "out") {
      if (editable(m, Date.now() / 1000, flags)) actions.append(actionButton("edit", "Edit message", () => startEdit(m)));
      actions.append(copyButton(() => copyTextOf(m)));
    } else {
      actions.append(copyButton(() => copyTextOf(m)));
      if (canReply) actions.append(actionButton("reply", "Reply", () => startReply(m)));
    }
    stack.append(bubble, actions);
    li.append(stack);
    return li;
  }

  // The quoted parent above a reply. Clicking it goes to the parent; when the parent is gone the
  // replying client's own quote is what is left to show.
  function replyQuoteNode(m) {
    const r = m.reply_to || {};
    const quote = document.createElement("button");
    quote.type = "button";
    quote.className = "reply-quote";
    const who = document.createElement("span");
    who.className = "reply-who";
    who.textContent = r.direction === "out" ? "You" : (r.from ? displayName(r.from) : $("peer-name")?.textContent || "");
    const line = document.createElement("span");
    line.className = "reply-line";
    line.textContent = r.excerpt || r.quote || "Original message unavailable";
    quote.append(who, line);
    quote.setAttribute("aria-label", "Replying to " + who.textContent + ": " + line.textContent + ". Show the original.");
    quote.addEventListener("click", () => jumpToMessage(r.msg_ref, r.message_id));
    return quote;
  }

  // Go to a message by its reference, paging older history in when it is not on screen, and
  // highlight it for a moment so the eye lands on it.
  function jumpToMessage(ref, id) {
    const view = conversationView;
    if (!view || !view.messages) return;
    const index = view.messages.findIndex((x) => (ref && x.msg_ref === ref) || (id && x.id === id));
    if (index < 0) { toast("The original message is no longer here."); return; }
    const target = view.messages[index];
    let row = messageRow(target.id);
    if (!row) {
      view.endId = view.messages[Math.min(view.messages.length - 1, index + 5)].id;
      view.limit = PAGE_SIZE;
      view.follow = false;
      view.target = target.id;
      renderThread();
      row = messageRow(target.id);
    } else {
      positionMessages(view, null, target.id);
    }
    if (!row) return;
    row.classList.remove("flash");
    void row.offsetWidth;
    row.classList.add("flash");
    setTimeout(() => row.classList.remove("flash"), 1600);
  }

  // Replying to or editing one message. One mode at a time, and it belongs to one conversation.
  let composeMode = null;
  function setComposeMode(mode) {
    composeMode = mode;
    const bar = $("compose-context");
    if (!mode) {
      bar.hidden = true;
      $("composer").classList.remove("editing");
      return;
    }
    bar.hidden = false;
    bar.dataset.kind = mode.kind;
    $("composer").classList.toggle("editing", mode.kind === "edit");
    $("compose-context-icon").innerHTML = mode.kind === "edit" ? ICONS.edit : ICONS.reply;
    $("compose-context-title").textContent = mode.kind === "edit"
      ? "Editing message"
      : "Replying to " + (mode.m.kind === "out" ? "yourself" : (mode.m.alias || $("peer-name").textContent || displayName(mode.m.address || state.openPeer)));
    $("compose-context-line").textContent = quoteExcerpt(copyTextOf(mode.m), mode.m.attachments);
    $("compose-context-close").setAttribute("aria-label", mode.kind === "edit" ? "Cancel editing" : "Cancel reply");
  }
  function cancelComposeMode() {
    if (!composeMode) return false;
    const wasEdit = composeMode.kind === "edit";
    const draft = composeMode.draft;
    setComposeMode(null);
    if (wasEdit) {
      $("compose").value = draft || "";
      $("compose").dispatchEvent(new Event("input"));
    }
    return true;
  }
  function startReply(m) {
    if (composeMode?.kind === "edit") cancelComposeMode();
    setComposeMode({ kind: "reply", m, peer: state.openPeer });
    $("compose").focus({ preventScroll: true });
  }
  function startEdit(m) {
    const draft = composeMode?.kind === "edit" ? composeMode.draft : $("compose").value;
    setComposeMode({ kind: "edit", m, peer: state.openPeer, draft });
    $("compose").value = copyTextOf(m);
    $("compose").dispatchEvent(new Event("input"));
    $("compose").focus({ preventScroll: true });
    const end = $("compose").value.length;
    try { $("compose").setSelectionRange(end, end); } catch (_) { /* not a text field in tests */ }
  }

  async function sendEdit(text) {
    const mode = composeMode;
    const context = mailboxContext();
    if (!mode || mode.kind !== "edit") return;
    if (text === copyTextOf(mode.m).trim()) { cancelComposeMode(); return; }
    try {
      await api("/v1/send", { method: "POST", body: { to: mode.peer, body: composeBody(text), from: context.address, edit_of: mode.m.msg_ref } });
      if (!context.current()) return;
      if (composeMode === mode) {
        const draft = mode.draft;
        setComposeMode(null);
        $("compose").value = draft || "";
        $("compose").dispatchEvent(new Event("input"));
      }
      await loadInbox().catch(() => {});
      if (context.current()) render();
    } catch (e) {
      if (!context.current()) return;
      const known = {
        edit_window_closed: "Messages can only be edited for 24 hours.",
        reference_not_found: "That message is no longer there to edit.",
        feature_disabled: "Editing is not available on this postbox.",
      };
      toast(e instanceof ApiError ? known[e.code] || sendFailure(e) : "Could not reach the postbox.");
    }
  }

  // Message info: when it was sent, delivered and seen, what the agent did, and every version.
  async function openMessageInfo(m) {
    const body = $("msg-info-body");
    body.textContent = "";
    const context = mailboxContext();
    const row = (label, value) => {
      const p = document.createElement("p");
      p.className = "info-row";
      const k = document.createElement("span"); k.className = "info-key"; k.textContent = label;
      const v = document.createElement("span"); v.className = "info-value"; v.textContent = value;
      p.append(k, v);
      body.append(p);
    };
    const when = (unix) => (unix ? dayLabel(unix) + " · " + clockTime(unix) : "—");
    openSheet("msg-info-sheet");
    $("msg-info-done").focus();
    let info = null;
    try {
      info = await api(withIdentity("/v1/messages/" + encodeURIComponent(m.id) + "/info", context.address), { signal: context.signal });
    } catch (_) { /* fall back to what the listing said */ }
    if (!context.current() || $("msg-info-sheet").hidden) return;
    body.textContent = "";
    const receipt = (info && info.receipt) || m.receipt;
    const agent = (info && info.agent_status) || m.agent_status;
    row(m.kind === "out" ? "Sent" : "Received", when((info && info.sent_at) || m.at));
    if (m.kind === "out" && has("receipts")) {
      row("Delivered", when(receipt && receipt.delivered_at));
      row("Seen", receipt && receipt.seen_at ? when(receipt.seen_at) : "—");
    }
    if (agent && has("agent_status")) {
      row("Agent", ({ working: "Working on it", done: "Finished", failed: "Run failed" }[agent.state] || agent.state) + " · " + when(agent.at));
    }
    const versions = (info && Array.isArray(info.versions)) ? info.versions : [];
    if (versions.length > 1) {
      const h = document.createElement("h3");
      h.className = "info-head";
      h.textContent = "Edit history";
      body.append(h);
      const list = document.createElement("ol");
      list.className = "info-versions";
      versions.forEach((v, i) => {
        const li = document.createElement("li");
        const t = document.createElement("span"); t.className = "info-key";
        t.textContent = (i === 0 ? "Original · " : "Edit " + i + " · ") + when(v.at);
        const text = document.createElement("span"); text.className = "info-version-text";
        text.textContent = copyTextOf({ body: v.body });
        li.append(t, text);
        list.append(li);
      });
      body.append(list);
    }
    if (!info) {
      const note = document.createElement("p");
      note.className = "field-note";
      note.textContent = "The full history could not be loaded.";
      body.append(note);
    }
  }

  // Right-click (and long-press, where the browser turns it into a context menu): the full list.
  function openMessageMenu(event, m) {
    if (window.getSelection && String(window.getSelection()).trim()) return; // copying a selection
    if (m.kind === "system") return;
    event.preventDefault();
    const menu = $("msg-menu");
    menu.textContent = "";
    const flags = actionFlags();
    const item = (label, fn) => {
      const b = document.createElement("button");
      b.type = "button"; b.setAttribute("role", "menuitem"); b.textContent = label;
      b.addEventListener("click", () => { closeMessageMenu(); fn(); });
      menu.append(b);
    };
    const live = isMsgRef(m.msg_ref) && m.status !== "sending" && m.status !== "failed";
    if (flags.reply && live) item("Reply", () => startReply(m));
    item("Copy", () => navigator.clipboard?.writeText(copyTextOf(m)).catch(() => {}));
    if (editable(m, Date.now() / 1000, flags)) item("Edit", () => startEdit(m));
    if (live && actionsOn()) item("Info", () => openMessageInfo(m));
    if (m.kind === "in" && state.openPeer) item("Block sender", () => askBlock({ peer: state.openPeer, name: $("peer-name").textContent, origin: "message", messageId: m.id }));
    menu.hidden = false;
    const x = Math.min(event.clientX, window.innerWidth - menu.offsetWidth - 8);
    const y = Math.min(event.clientY, window.innerHeight - menu.offsetHeight - 8);
    menu.style.left = Math.max(8, x) + "px";
    menu.style.top = Math.max(8, y) + "px";
    menu.querySelector("button")?.focus();
  }
  function closeMessageMenu() { $("msg-menu").hidden = true; }

  // The wire name is a protocol token. Showing it raw made a sentence somebody typed look like an
  // envelope they had not asked for. An unknown verb keeps its wire name: a verb this build does
  // not know is still worth seeing exactly as it arrived.
  // What Copy puts on the clipboard: the words, not the wrapper. A request copied verbatim is
  // JSON to unpick and an auto-reply carries two header lines nobody wants to paste anywhere.
  function copyTextOf(m) {
    const envelope = requestEnvelope(m.body);
    if (envelope && envelope.args) {
      const task = envelope.args.task || envelope.args.question;
      if (typeof task === "string" && task.trim()) return task;
    }
    const reply = autoReply(m.body);
    if (reply) return reply.body;
    return m.body;
  }

  // An unattended reply, split from its header. Returns null for anything else, so a message that
  // merely mentions the marker is not mistaken for one.
  function autoReply(body) {
    const text = String(body == null ? "" : body);
    if (!text.startsWith("pigeonpost-auto-reply v1")) return null;
    const lines = text.split("\n");
    const header = lines.shift();
    if (lines.length && lines[0].startsWith("Generated unattended")) lines.shift();
    while (lines.length && !lines[0].trim()) lines.shift();
    const answered = /answered=([a-z_]+)/.exec(header);
    return { answered: answered ? answered[1] : null, body: lines.join("\n") };
  }

  // Markdown with its punctuation taken off, for one line in a list. A preview is a glance, and
  // `## Pinned it` glanced at is two stray hashes. The thread renders the markup properly; here it
  // is noise costing characters the sentence needed.
  function plainText(raw) {
    const out = [];
    let inFence = false;
    for (const line of String(raw == null ? "" : raw).split(/\r?\n/)) {
      let s = line.trim();
      if (s.startsWith("```")) {
        inFence = !inFence;
        continue;
      }
      // Code summarises nothing. Skipping it lets the prose above surface instead.
      if (inFence) continue;
      // A table underline says nothing at a glance, and a table row says what it says without the
      // pipes holding it up. Only a fenced row is treated this way: a sentence that happens to
      // contain a pipe keeps it.
      if (/^\|.*\|$/.test(s)) {
        if (tableAlignments(s) !== null) continue;
        s = s.slice(1, -1).split("|").map((c) => c.trim()).filter(Boolean).join(" — ");
      }
      s = s.replace(/^#{1,6}\s*/, "").replace(/^[-*+>]\s+/, "");
      s = s.replace(/\*\*/g, "").replace(/`/g, "");
      s = s.trim();
      if (s) out.push(s);
    }
    return out.join(" ").replace(/\s+/g, " ");
  }

  function verbTitle(verb) {
    const titles = {
      full_access: "Full permissions",
      make_change: "Do this work",
      report_status: "Report status",
      answer_question: "Answer a question",
      run_tests: "Run the tests",
      read_file: "Read a file",
      git_push: "Push",
      deploy: "Deploy",
    };
    return titles[verb] || verb;
  }

  // Markdown, built as DOM nodes.
  //
  // Never `innerHTML`, at any point, for any part of this: a message body is another agent's text
  // and the one rule this file has always kept is that it is inserted as text, never as markup.
  // Rendering it must not become a way around that. Every node below is created and has its
  // `textContent` set; nothing is ever parsed as HTML, and no link is made clickable.
  //
  // The grammar is what agents actually send — headings, bullets, numbered lists, fenced code,
  // quotes, rules, tables, and inline emphasis. Anything unrecognised is shown as the literal line
  // it was.
  // A renderer that silently drops what it cannot parse loses somebody's message, which is far
  // worse than an unstyled one.
  function renderMarkdown(container, raw) {
    const lines = String(raw == null ? "" : raw).split(/\r?\n/);
    let paragraph = [];
    let list = null; // {el, ordered}

    const flushParagraph = () => {
      if (!paragraph.length) return;
      const p = document.createElement("p");
      p.className = "md-p";
      inline(p, paragraph.join("\n"));
      container.append(p);
      paragraph = [];
    };
    const flushList = () => {
      if (list) container.append(list.el);
      list = null;
    };
    const flush = () => {
      flushParagraph();
      flushList();
    };

    for (let i = 0; i < lines.length; i += 1) {
      const line = lines[i];
      const trimmed = line.trim();

      // Fenced code first: what is inside is literal, including anything that would otherwise
      // read as a heading or a bullet.
      if (trimmed.startsWith("```")) {
        flush();
        const body = [];
        i += 1;
        for (; i < lines.length; i += 1) {
          if (lines[i].trim().startsWith("```")) break;
          body.push(lines[i]);
        }
        const pre = document.createElement("pre");
        pre.className = "md-code";
        pre.textContent = body.join("\n");
        container.append(pre);
        continue;
      }

      if (!trimmed) {
        flush();
        continue;
      }

      if ((trimmed === "---" || trimmed === "***" || trimmed === "___") && !paragraph.length) {
        flush();
        container.append(document.createElement("hr"));
        continue;
      }

      // `#text` is a hashtag or an issue number, not a heading. The space is required.
      const heading = /^(#{1,6})\s+(.*)$/.exec(trimmed);
      if (heading) {
        flush();
        const level = Math.min(heading[1].length, 6);
        const h = document.createElement("div");
        h.className = "md-h md-h" + (level <= 2 ? level : 3);
        inline(h, heading[2].trim());
        container.append(h);
        continue;
      }

      if (trimmed === ">" || trimmed.startsWith("> ")) {
        flush();
        const q = document.createElement("blockquote");
        q.className = "md-quote";
        inline(q, trimmed.slice(1).trim());
        container.append(q);
        continue;
      }

      // A table needs its underline. `| yes | no |` on its own is a line somebody typed, and only
      // the `|---|---|` beneath it says the pipes were a grid — so a sentence containing a pipe
      // stays the sentence it was. Rows run until a blank line or a line with no pipe in it.
      if (trimmed.includes("|") && tableAlignments(lines[i + 1]) !== null) {
        flush();
        const header = tableCells(trimmed);
        const aligns = tableAlignments(lines[i + 1]);
        i += 1;
        const rows = [];
        while (i + 1 < lines.length) {
          const next = lines[i + 1].trim();
          if (!next || !next.includes("|")) break;
          i += 1;
          rows.push(tableCells(next));
        }
        container.append(buildTable(header, aligns, rows));
        continue;
      }

      const bullet = /^[-*+]\s+(.*)$/.exec(trimmed);
      const numbered = /^(\d{1,3})[.)]\s+(.*)$/.exec(trimmed);
      if (bullet || numbered) {
        flushParagraph();
        const ordered = Boolean(numbered);
        if (list && list.ordered !== ordered) flushList();
        if (!list) {
          list = { el: document.createElement(ordered ? "ol" : "ul"), ordered };
          list.el.className = "md-list";
        }
        const li = document.createElement("li");
        inline(li, (bullet ? bullet[1] : numbered[2]).trim());
        list.el.append(li);
        continue;
      }

      flushList();
      paragraph.push(line);
    }
    flush();
  }

  // Inline emphasis, one pass, text nodes only. Anything that does not close is left as the
  // characters it was rather than swallowing the rest of the line.
  function inline(target, text) {
    const pattern = /(\*\*[^*]+\*\*|`[^`]+`|\*[^*]+\*|_[^_]+_)/g;
    let last = 0;
    let match;
    while ((match = pattern.exec(text)) !== null) {
      if (match.index > last) {
        target.append(document.createTextNode(text.slice(last, match.index)));
      }
      const token = match[0];
      let el;
      if (token.startsWith("**")) {
        el = document.createElement("strong");
        el.textContent = token.slice(2, -2);
      } else if (token.startsWith("`")) {
        el = document.createElement("code");
        el.textContent = token.slice(1, -1);
      } else {
        el = document.createElement("em");
        el.textContent = token.slice(1, -1);
      }
      target.append(el);
      last = match.index + token.length;
    }
    if (last < text.length) target.append(document.createTextNode(text.slice(last)));
  }

  // The cells of one row. A leading and a trailing pipe fence the row rather than being empty cells
  // either side of it, so they come off before the split.
  function tableCells(line) {
    let s = String(line).trim();
    if (s.startsWith("|")) s = s.slice(1);
    if (s.endsWith("|")) s = s.slice(0, -1);
    return s.split("|").map((cell) => cell.trim());
  }

  // `|---|:--:|` and nothing else: every cell dashes, optionally anchored by a colon at one end or
  // both. Returns one alignment per column, or `null` if this line is not a table underline — which
  // is what keeps the row above it prose.
  function tableAlignments(line) {
    if (line == null) return null;
    const s = String(line).trim();
    if (!s.includes("|") || !s.includes("-")) return null;
    const cells = tableCells(s);
    if (!cells.length) return null;
    const out = [];
    for (const cell of cells) {
      const spec = /^(:?)(-+)(:?)$/.exec(cell);
      if (!spec) return null;
      out.push(spec[1] && spec[3] ? "center" : spec[3] ? "right" : spec[1] ? "left" : "");
    }
    return out;
  }

  // A table, in a block that scrolls.
  //
  // The wrapper is the point rather than decoration: a table's width is its columns', and a bubble
  // is 560px at the widest. Without something to scroll, a wide table is either squeezed until
  // every cell wraps to a word a line — which is a table with its one useful property gone — or
  // clipped by `.messages`, which hides its own horizontal overflow on purpose so that one wide
  // message cannot pan the whole conversation sideways.
  function buildTable(header, aligns, rows) {
    const wrap = document.createElement("div");
    wrap.className = "md-table-wrap";
    const table = document.createElement("table");
    table.className = "md-table";

    const cell = (tag, text, column) => {
      const el = document.createElement(tag);
      if (aligns[column]) el.style.textAlign = aligns[column];
      inline(el, text == null ? "" : text);
      return el;
    };

    const thead = document.createElement("thead");
    const headRow = document.createElement("tr");
    header.forEach((text, column) => headRow.append(cell("th", text, column)));
    thead.append(headRow);
    table.append(thead);

    const tbody = document.createElement("tbody");
    for (const row of rows) {
      const tr = document.createElement("tr");
      // A short row is padded out and a long one kept whole. The header says what shape the table
      // is, but dropping the extra cell would drop somebody's data to make the shape true.
      const width = Math.max(header.length, row.length);
      for (let column = 0; column < width; column += 1) tr.append(cell("td", row[column], column));
      tbody.append(tr);
    }
    table.append(tbody);

    wrap.append(table);
    return wrap;
  }

  function heldReason(code) {
    const reasons = {
      sender_not_auto: "this sender was never granted autonomy",
      verb_denied: "that verb was not granted to this sender",
      verb_never_auto: "this verb is never auto-accepted, whoever asks",
      not_a_request: "not a scoped request",
      unknown_verb: "not a verb this postbox knows",
    };
    return reasons[code] || code.replace(/_/g, " ");
  }

  let peerInfoTrigger = null;
  let blockingPeer = null;

  function grantableVerbs() {
    const vocab = state.vocabulary;
    return (vocab?.grantable || []).filter(verb => !(vocab.never_auto || []).includes(verb));
  }

  function renderPeerInfo(thread) {
    const c = thread.contact;
    const exact = state.contacts.find(row => row.peer === thread.peer);
    const blocked = c?.admission === "block";
    const verbs = grantableVerbs();
    const full = c?.admission === "allow" && c.autonomy === "auto" && verbs.length > 0
      && verbs.every(verb => (c.allowed_verbs || []).includes(verb));
    const busy = Boolean(state.senderAction?.pending);
    const unavailable = busy || !state.contactsReady;
    const lastIn = [...thread.messages].reverse().find(m => m.kind === "in");
    const addresses = $("peer-addresses"), facts = $("peer-facts");
    addresses.textContent = facts.textContent = "";
    const add = (dl, term, value, mono) => {
      const dt = document.createElement("dt"), dd = document.createElement("dd");
      dt.textContent = term;
      dd.textContent = value;
      if (mono) dd.className = "mono";
      dl.append(dt, dd);
    };
    $("peer-info-title").textContent = threadName(thread);
    paintAvatar($("peer-info-avatar"), thread.peer);
    add(addresses, "Address", thread.identity?.address || lastIn?.address || thread.peer, true);
    if (thread.peer.startsWith("/") && !thread.peer.startsWith("/k/")) add(addresses, "Handle", thread.peer, true);
    if (thread.mine) add(addresses, "Mailbox", "yours — on this account");
    add(facts, "Contact", c ? (c.peer === thread.peer ? "yes" : "via " + c.peer) : "not a contact");
    add(facts, "Granted verbs", c?.allowed_verbs?.length ? c.allowed_verbs.join(", ") : "none");
    if (lastIn?.standing) add(facts, "Standing", lastIn.standing + (lastIn.tier ? " · " + lastIn.tier : ""));

    $("peer-known").checked = Boolean(exact);
    $("peer-known").disabled = unavailable;
    $("peer-retry").hidden = state.contactsReady;
    $("peer-retry").disabled = state.contactsRetrying;
    $("peer-full").checked = Boolean(full);
    $("peer-full").disabled = unavailable || !c || blocked || !verbs.length;
    $("peer-requests").disabled = unavailable || !exact;
    $("peer-trust-note").textContent = blocked
      ? "Blocked. Their mail is refused, so trust does not apply until you unblock them."
      : c && !exact
        ? "Covered by " + c.peer + ". Marking this sender known gives them a row of their own, which outranks that rule."
        : full
          ? "Their requests are acted on without asking you. The postbox still refuses what it never auto-accepts for anyone, and the receiving machine's own permission tier and limits still apply on top."
          : "A known sender is admitted by name. Full permissions lets their requests be acted on without waiting for you; everything else is held.";
    $("peer-archive").textContent = state.archived.has(thread.peer) ? "Move back to the inbox" : "Archive this conversation";
    $("peer-archive").disabled = busy;
    $("peer-block").textContent = blocked ? "Unblock this sender" : "Block this sender";
    $("peer-block").className = (blocked ? "btn-ghost" : "btn-danger") + " peer-action";
    $("peer-block").disabled = unavailable;
    $("peer-actions-note").textContent = thread.mine
      ? "Archiving hides a conversation; nothing is deleted and their mail still arrives. Blocking one of your own agents refuses its mail from here on — the way to stop one talking to you is usually to stop running it."
      : "Archiving hides a conversation; nothing is deleted and their mail still arrives. Blocking refuses it from here on. Neither is announced to them.";
    $("peer-own-mailbox").hidden = !(thread.mine && thread.identity);
    $("peer-open-mailbox").onclick = () => switchIdentity(thread.identity);
    $("peer-mailbox-note").textContent = "You are writing to this agent from " + (state.me.handle || state.me.address)
      + ". Opening the mailbox instead shows the mail it has received.";
    $("peer-action-status").hidden = !busy;
    const error = state.senderAction?.peer === thread.peer ? state.senderAction.error : null;
    for (const id of ["peer-action-error", "peer-block-error"]) {
      $(id).hidden = !error;
      $(id).textContent = error || "";
    }
    $("peer-block-confirm").disabled = busy;
    $("peer-block-confirm").textContent = busy ? "Blocking…" : "Block";
  }

  function openPeerInfo(trigger) {
    if (!state.openPeer) return;
    peerInfoTrigger = trigger;
    state.showInfo = true;
    renderThread();
    openSheet("peer-info-sheet");
    $("peer-info-close").focus();
  }

  // These shortcuts write an exact sender row, never the namespace rule that may cover them.
  // Apply only a confirmed write and invalidate contact snapshots requested before it completed.
  async function updateSender(peer, action) {
    if (!peer || !state.contactsReady || state.senderAction?.pending) return;
    const context = mailboxContext(), existing = contactFor(peer), focused = document.activeElement;
    const verbs = grantableVerbs();
    if (action === "full" && (!existing || existing.admission === "block" || !verbs.length)) return;
    const row = { peer, alias: existing?.alias || null, admission: "allow",
      autonomy: existing?.autonomy || "review", allowed_verbs: existing?.allowed_verbs || [] };
    if (action === "full") { row.autonomy = "auto"; row.allowed_verbs = verbs; }
    if (action === "review" || action === "block") { row.autonomy = "review"; row.allowed_verbs = []; }
    if (action === "block") row.admission = "block";
    state.senderAction = { peer, pending: true, error: null };
    renderThread();
    try {
      // DELETE reads `peer` from the query string on the server; the body copy is harmless.
      await api(action === "forget" ? withIdentity("/v1/contacts?peer=" + encodeURIComponent(peer), context.address) : "/v1/contacts", { method: action === "forget" ? "DELETE" : "PUT",
        body: { ...(action === "forget" ? { peer } : row), identity: context.address }, signal: context.signal });
      if (!context.current()) return;
      state.sectionRequests.contacts = (state.sectionRequests.contacts || 0) + 1;
      state.contacts = state.contacts.filter(contact => contact.peer !== peer);
      if (action !== "forget") state.contacts.push(row);
      toast(action === "forget" ? "Sender removed. Their namespace rule or default policy now applies."
        : action === "block" ? "Blocked. Their new messages will be refused."
          : action === "full" ? "Full permissions saved."
            : action === "review" ? "Back to review. Their requests wait for you." : "Marked as known.");
    } catch (e) {
      if (context.current()) state.senderAction.error = "Could not save this sender. " + (e?.message || "Please try again.");
    } finally {
      if (context.current()) {
        state.senderAction.pending = false;
        renderContactList();
        render();
        if (!state.senderAction.error && action === "block" && blockingPeer === peer) closeSheet("peer-block-sheet");
        // Disabling a control during its save can move browser focus to the document body.
        // Restore it only if the person is still in this panel and has not focused elsewhere.
        if (state.showInfo && state.openPeer === peer && document.activeElement === document.body
            && focused?.closest("#" + sheetStack.at(-1)) && !focused.closest("[hidden]") && !focused.disabled) focused.focus();
      }
    }
  }

  // ---- threads within one peer ----------------------------------------------------------------

  // The conversations with one peer, most recently active first.
  //
  // Built from the messages rather than from the server's list alone, so it is right even against a
  // postbox that has no thread routes; the server's list is merged in on top because a thread
  // somebody opened and has not written in yet exists only there.
  function subthreadsFor(peer) {
    const conversation = buildThreads().find((t) => t.peer === peer);
    const byId = new Map();
    const touch = (id) => {
      if (!byId.has(id)) {
        byId.set(id, { id, title: null, is_default: false, messages: [], unread: 0, last: 0 });
      }
      return byId.get(id);
    };

    for (const m of (conversation ? conversation.messages : [])) {
      // A message with no thread comes from a postbox older than threads. Grouping those under one
      // key keeps them together as the single conversation they were.
      const t = touch(m.thread_id || "");
      t.messages.push(m);
      if (m.kind === "in" && !m.read) t.unread += 1;
      if (m.at > t.last) t.last = m.at;
    }

    for (const st of state.serverThreads) {
      if (normalisePeer(st.peer) !== peer) continue;
      const t = touch(st.thread_id);
      t.title = st.title || null;
      t.is_default = Boolean(st.is_default);
      if (st.last_at > t.last) t.last = st.last_at;
    }

    return [...byId.values()].sort((a, b) => {
      if (b.last !== a.last) return b.last - a.last;
      // A pair of untouched threads would otherwise sort arbitrarily and jump between renders.
      return subthreadName(a).localeCompare(subthreadName(b));
    });
  }

  const subthreadName = (t) => t.title || "Default thread";

  // Shown whenever a peer is open, including when they have only the default thread.
  //
  // It used to appear only at two threads or more, on the reasoning that one conversation should
  // look exactly as it did before threads existed. That was a dead end: the button that opens a
  // second thread lives in this pane, so a peer with one thread could never get a second one. An
  // action you can only reach once you have already done it is not an action.
  function subsVisible() {
    return Boolean(state.openPeer);
  }

  // The thread whose messages the content pane shows.
  //
  // Falls back to the most recent rather than to nothing: selecting a peer has always opened a
  // conversation, and landing on an empty pane because no thread was named would be a step
  // backwards for every mailbox that never opens a second one.
  function currentSubthread(peer) {
    const subs = subthreadsFor(peer);
    if (!subs.length) return null;
    const chosen = subs.find((t) => t.id === state.openThread);
    return chosen || subs[0];
  }

  // ---- actions ------------------------------------------------------------------------------

  const onPhone = () => window.matchMedia("(max-width: 780px)").matches;

  // Back, one step. On a phone that is messages → threads → names, because skipping the middle
  // screen on the way out of a peer with several conversations loses your place in it.
  function closeThread() {
    stashDraft();
    if (onPhone() && state.openThread !== null && subsVisible()) {
      state.openThread = null;
      state.showInfo = false;
      render();
      resetComposer();
      return;
    }
    state.openPeer = null;
    state.openThread = null;
    state.showInfo = false;
    render();
    resetComposer();
  }

  function openSubthread(id) {
    stashDraft();
    state.openThread = id;
    state.showInfo = false;
    if (onPhone()) history.replaceState({ thread: state.openPeer, sub: id }, "");
    render();
    restoreDraft();
    if (!onPhone()) $("compose").focus({ preventScroll: true });
    ackVisible();
  }

  // Open a new conversation with this peer, named.
  //
  // A dialog rather than `window.prompt`: the prompt cannot be styled, cannot be dismissed by
  // clicking away, and reads as the browser interrupting rather than the app asking.
  function newSubthread() {
    if (!state.openPeer) return;
    const input = $("thread-title-input");
    input.value = "";
    $("thread-error").hidden = true;
    openSheet("thread-sheet");
    input.focus();
  }

  async function createSubthread() {
    const context = mailboxContext();
    const peer = state.openPeer;
    const title = $("thread-title-input").value.trim();
    if (!peer || $("thread-create").disabled) return;
    if (!title) {
      // Said in the dialog rather than as a toast that appears somewhere else: the thing to fix
      // is right here.
      $("thread-error").textContent = "Give the thread a name.";
      $("thread-error").hidden = false;
      $("thread-title-input").focus();
      return;
    }
    const button = $("thread-create");
    button.disabled = true;
    try {
      const made = await api("/v1/threads", {
        method: "POST",
        body: { peer, title, identity: context.address },
      });
      if (!context.current()) return;
      await loadThreads();
      if (!context.current() || state.openPeer !== peer) return;
      // Selected straight away, and the pane it belongs in appears with it: opening a thread and
      // then having to find it in a list that just appeared is two steps for one intention.
      stashDraft();
      state.openThread = made.thread_id;
      closeSheet("thread-sheet");
      render();
      restoreDraft();
      if (!onPhone()) $("compose").focus({ preventScroll: true });
    } catch (e) {
      if (!context.current()) return;
      $("thread-error").textContent =
        e instanceof ApiError ? e.message : "Could not open the thread.";
      $("thread-error").hidden = false;
    } finally {
      if (context.current()) button.disabled = false;
    }
  }

  async function openThread(peer) {
    window.PigeonpostGroups?.close(); // Phase 3a
    if (state.leftView === "calendar") state.leftView = "inbox"; // Phase 3B: a conversation replaces the calendar
    stashDraft();
    const wasClosed = !state.openPeer;
    state.openPeer = peer;
    state.showInfo = false;
    // A peer with one conversation opens straight into it, exactly as before threads existed. With
    // more than one, the phone stops at the list of them and waits to be told which; a wide screen
    // shows the list and the most recent at the same time, so nothing is a dead end.
    const subs = subthreadsFor(peer);
    // A phone stops at the list only when there is a choice to make. With one thread there is
    // nothing to choose, so it opens straight into it — the list is one step back if you want it,
    // which is where the button for a second thread is.
    state.openThread = subs.length > 1 && onPhone() ? null : subs[0] ? subs[0].id : null;
    // On a phone the thread covers the list, so the system back gesture has to close it rather than
    // leave the app. Push one entry the first time a thread opens; switching between threads
    // replaces it, so back is always one step out to the list.
    if (onPhone()) {
      if (wasClosed) history.pushState({ thread: peer }, "");
      else history.replaceState({ thread: peer }, "");
    }
    render();
    restoreDraft();
    // Desktop only. On a phone, focusing the composer raises the keyboard over the thread you just
    // opened — you came to read it, and typing is a second decision you make by tapping the box.
    if (!onPhone()) $("compose").focus({ preventScroll: true });

    await ackVisible();
  }

  // Opening a conversation is reading it. Acknowledging clears the unread mark server-side, which
  // is also what tells an agent sharing this mailbox that the message has been dealt with — so it
  // is a real decision, not just a UI flourish.
  //
  // Scoped to what is actually on screen: with several threads open on a peer, marking the other
  // threads read because one of them was looked at would clear a mark nobody has seen.
  async function ackVisible() {
    const context = mailboxContext();
    const peer = state.openPeer;
    if (!peer || document.visibilityState === "hidden" || (onPhone() && $("pane-thread").dataset.open !== "true")) return;
    const displayed = new Set([...$("messages").querySelectorAll("[data-message-id]")].map(row => row.dataset.messageId));
    const showing = subsVisible() ? currentSubthread(peer) : null;
    const unread = state.inbound.filter(
      (m) =>
        peerKeyOf(m) === peer &&
        m.direction !== "out" && displayed.has(m.message_id) &&
        !m.read &&
        (!showing || (m.thread_id || "") === showing.id),
    );
    if (!unread.length) return;
    for (const m of unread) {
      m.read = true;
      acked.add(m.message_id);
    }
    renderThreadList();
    renderSubs();
    for (const m of unread) {
      if (!context.current()) return;
      try {
        await api(withIdentity("/v1/ack", context.address), { method: "POST", body: actionsOn()
          ? { message_id: m.message_id, identity: context.address, with_edits: true }
          : { message_id: m.message_id, identity: context.address } });
      } catch (_) {
        if (context.current()) { acked.delete(m.message_id); m.read = false; }
      }
    }
  }

  // Everything sent from this app asks for work, at the most it can ask for.
  //
  // There is no picker, deliberately. Choosing a verb is a decision about someone else's machine
  // made by the person with the least information: the sender cannot see what the recipient
  // granted or what tier it runs at, and guessing low only guarantees the message sits in review.
  // So ask for the most and let the recipient decide — its grant, its permission tier, its branch
  // allowlist and its daily ceiling all still apply, and anything it will not do is held for a
  // human exactly as prose used to be.
  //
  // Agent-to-agent traffic still uses the narrower verbs; they are a protocol, not a UI.
  //
  // `full_access` rather than `make_change`: somebody messaging their own fleet is asking for the
  // job to be finished, not for a scoped subset of it. Under `make_change` an agent would do the
  // work, commit it, and stop short of publishing — which read back as "I was not allowed".
  function composeBody(text) {
    return JSON.stringify({
      v: 1,
      verb: "full_access",
      args: { task: text },
      note: text,
    });
  }

  async function sendMessage(text, reply) {
    const context = mailboxContext();
    const to = state.openPeer;
    const draftKey = composerKey();
    const originalText = text;
    const files = [...staged];
    // Whichever thread is on screen. Sending into the conversation you are reading is the only
    // behaviour that does not surprise: the alternative is a reply that leaves the thread it
    // answers.
    const showing = currentSubthread(to);
    const threadId = showing && showing.id ? showing.id : null;
    // Uploaded before the send names them. A failure here stops the message rather than sending
    // it without the files it was about: half a message is worse than none, because the sender has
    // no way to know which half arrived.
    let attachments = [];
    if (files.length) {
      try {
        attachments = await Promise.all(files.map(file => uploadFile(file, context)));
      } catch (e) {
        if (!context.current()) return;
        if (composerKey() === draftKey && !$("compose").value) $("compose").value = originalText;
        // `ApiError`'s message is the postbox's own detail — "this mailbox holds 96 MB of 100 MB"
        // says what to do about it, and `e.detail` (which this read, and which the class never
        // sets) said nothing at all.
        toast(e instanceof ApiError ? e.message || "Could not upload that file." : "Could not upload that file.");
        return;
      }
      if (!context.current()) return;
    }
    text = composeBody(text);
    // Phase 1e: the message's own reference, made here so a retry cannot send it twice and a reply
    // to it works before the next poll. A reply names its parent and carries a short quote.
    const msgRef = actionsOn() ? newMsgRef() : null;
    const replyFields = reply && isMsgRef(reply.msg_ref) && has("message_reply")
      ? { reply_to: reply.msg_ref, quote: quoteExcerpt(copyTextOf(reply), reply.attachments) }
      : null;
    const record = Pending.add({
      msg_ref: msgRef,
      reply_to: replyFields ? { msg_ref: replyFields.reply_to, quote: replyFields.quote, excerpt: replyFields.quote,
        direction: reply.kind === "out" ? "out" : "in", from: reply.address || null } : null,
      local_id: "local_" + randomString(8),
      mailbox: context.address,
      to,
      body: text,
      at: Math.floor(Date.now() / 1000),
      status: "sending",
      thread_id: threadId,
      attachments: files.map((file, i) => ({ id: attachments[i], filename: file.name, media_type: file.type, bytes: file.size })),
      request: {
        to, body: text, from: context.address, attachments,
        ...(threadId ? { thread_id: threadId } : {}),
        ...(msgRef ? { msg_ref: msgRef } : {}),
        ...(replyFields || {}),
      },
    });
    render();

    try {
      await deliver(record);
      drafts.delete(draftKey);
      if (context.current() && composerKey() === draftKey) {
        if ($("compose").value.trim() === originalText) $("compose").value = "";
        // A picker/drop can add files while the request is in flight. Remove only
        // the captured files, and keep the entire draft on upload/send failure.
        for (const file of files) {
          const index = staged.indexOf(file);
          if (index !== -1) staged.splice(index, 1);
        }
        renderStaged();
        jumpToLatest();
      }
      // Nothing to reconcile against if the postbox did not keep a copy; drop the optimistic row
      // and let the next poll be the truth.
      if (!record.sent_copy_id) Pending.reconcile(new Set());
      if (context.current()) loadInbox().then(render).catch(() => {});
    } catch (e) {
      if (context.current()) toast(sendFailure(e));
    }
    if (context.current()) render();
  }

  // One attempt at a pending record's send, exactly as it was first made: same sender, recipient,
  // body, thread and `msg_ref`. The record keeps the request it was born with, so a retry can never
  // pick up a different mailbox, a different conversation or a different reference — with the
  // reference, a retry of a send that did commit is answered as that same message, not a second.
  //
  // A refusal is the postbox's answer and final. Anything else — the network, a lost response, a
  // gateway in between — leaves the outcome unknown, and the record says so.
  async function deliver(record) {
    record.status = "sending";
    record.uncertain = false;
    try {
      const sent = await api("/v1/send", { method: "POST", body: record.request });
      // The id of the server's own copy. Holding it is what lets the optimistic row retire the
      // moment that copy comes back, instead of the message appearing twice for a poll.
      record.sent_copy_id = sent.sent_copy_id || null;
      record.status = "sent";
      return sent;
    } catch (e) {
      record.status = "failed";
      record.uncertain = isUncertain(e);
      throw e;
    }
  }

  // Whether a failed send might nonetheless have been delivered.
  const isUncertain = (e) => !(e instanceof ApiError) || e.status >= 500;

  async function retrySend(localId) {
    const record = Pending.find(localId);
    const context = mailboxContext();
    // Only from the mailbox it was written in: the rows are listed per mailbox, so this is a guard
    // against a stale button rather than a choice anyone can make.
    if (!record || record.status !== "failed" || record.mailbox !== context.address) return;
    const promise = deliver(record);
    render();
    try {
      await promise;
      if (!record.sent_copy_id) Pending.rows = Pending.rows.filter((r) => r !== record);
      if (context.current()) loadInbox().then(render).catch(() => {});
    } catch (e) {
      if (context.current()) toast(sendFailure(e));
    }
    if (context.current()) render();
  }

  function discardSend(localId) {
    const record = Pending.find(localId);
    if (!record || record.status !== "failed" || record.mailbox !== mailboxContext().address) return;
    Pending.rows = Pending.rows.filter((r) => r !== record);
    render();
  }

  function sendFailure(e) {
    if (!(e instanceof ApiError)) return "Could not reach the postbox.";
    const known = {
      not_admitted: "They are not accepting mail from this mailbox.",
      recipient_unresolved: "No mailbox at that address.",
      recipient_inbox_full: "Their inbox is full.",
      stranger_rate_limited: e.message,
      unauthorized: "Your session expired. Sign in again.",
    };
    return known[e.code] || e.message;
  }

  async function switchIdentity(identity) {
    if (identity.address === state.me?.address) return;
    stashDraft();
    stopLive();
    mailboxController.abort();
    mailboxController = new AbortController();
    closeMailboxSheets();
    LS.setItem(K.identity, identity.address);
    state = { ...freshState(), identities: state.identities, me: identity };
    const context = mailboxContext();
    acked.clear();
    resetComposer();
    $("search").value = "";
    resetConversationView();
    renderIdentityMenu();
    render();
    window.PigeonpostGroups?.onMailbox(); // Phase 3a
    window.PigeonpostCalendar?.onMailbox(); // Phase 3B
    window.PigeonpostNotes?.onMailbox(); // Phase 7
    // The stream regardless: an opening that did not arrive is exactly when the mailbox most needs
    // something retrying behind it.
    try { await loadAll(); }
    catch (e) { if (context.current() && e instanceof ApiError && e.status === 401) signOut(); }
    finally { if (context.current()) startLive(); }
  }

  // ---- loading -------------------------------------------------------------------------------

  async function loadIdentities() {
    const version = sessionVersion, request = ++identitiesRequest;
    const { identities } = await api("/v1/identities");
    // `/v1/identities` reports the address and the operator's own label. The handle — the thing
    // trust actually matches on — is only knowable from the server, per mailbox.
    const resolved = await Promise.all((identities || []).map(async (id) => {
      try {
        const who = await api("/v1/whoami?identity=" + encodeURIComponent(id.address));
        return { address: id.address, label: id.label, handle: who.handle || null };
      } catch (_) {
        return { address: id.address, label: id.label, handle: null };
      }
    }));
    if (version !== sessionVersion || request !== identitiesRequest) return;
    state.identities = resolved;

    // Default to the operator's own named mailbox rather than whichever address the server
    // happened to list first. A handle is a mailbox somebody deliberately named — usually the one
    // they think of as "my inbox" — while an anonymous /k/ address is typically an agent's. An
    // explicit earlier choice still wins over both.
    const remembered = state.me?.address || LS.getItem(K.identity);
    const named = resolved.filter((i) => i.handle);

    // Within the operator's own namespace, `main` is the one that answers for the namespace itself
    // — mail addressed to `/bekir` rather than to `/bekir/agent1` lands there. So it is what
    // "my inbox" means, and opening any other mailbox of theirs by default would hide exactly the
    // mail a person sent them directly. Falling back to the first named mailbox in the namespace
    // keeps this working before a `main` exists.
    const inNamespace = (i) =>
      cfg && cfg.primaryNamespace && i.handle.startsWith(cfg.primaryNamespace + "/");
    // A /github/<login> mailbox is personal by construction: the postbox only mints one for a
    // login the account has proved it controls. So for someone who signed in that way it is "their"
    // inbox, in the same sense /bekir/main is for a bought namespace.
    const preferred =
      named.find((i) => inNamespace(i) && i.handle === cfg.primaryNamespace + "/main") ||
      named.find(inNamespace) ||
      named.find((i) => i.handle.startsWith("/github/")) ||
      named.find((i) => i.handle.endsWith("/main")) ||
      null;
    const chosen =
      resolved.find((i) => i.address === remembered) ||
      preferred ||
      named[0] ||
      resolved[0] ||
      null;
    if (chosen) {
      if (state.me && state.me.address !== chosen.address) await switchIdentity(chosen);
      else state.me = chosen;
    }
    renderIdentityMenu();
  }

  // The four opening calls, and one verdict about the network settled after all of them.
  //
  // `Promise.all` was wrong twice over. It rejected the moment one of the four did, so a postbox
  // that was unreachable for the second the page loaded took `startLive()` down with it — no
  // stream, no long poll, no retry of any kind. The banner went up and stayed up, over a mailbox
  // nothing was ever going to refresh again, until someone reloaded the page. And because these
  // four run together and each decides the banner for itself, whichever finished last decided it:
  // when the order fell the other way a `/v1/contacts` that arrived cleared the banner a failed
  // `/v1/inbox` had just raised, and the page went back to claiming it was current over a listing
  // it had not managed to load.
  async function loadAll() {
    const context = mailboxContext();
    const results = await Promise.allSettled([loadInbox(), loadContacts(), loadArchive(), loadThreads(), loadCallLog(), loadFiltered()]);
    if (!context.current()) return;
    render();
    // The last word, so the answer no longer depends on which call finished first. The other three
    // absorb their own failures — one route being unavailable is not the account being offline —
    // so what is left to settle is whether the inbox itself arrived, which is what the banner is
    // about. Refused is not the same as did not arrive: a postbox that answered is reachable.
    // Only `loadInbox` rethrows, and only an answer — a 401 above all — is something the caller
    // has to act on. A request that never arrived is weather, and the live loop is what retries it.
    const refused = results.find((r) => r.status === "rejected" && r.reason instanceof ApiError);
    if (refused) throw refused.reason;
  }

  async function loadInbox(signal) {
    // include_sent turns the listing into a conversation. Opt-in on the wire, because every other
    // caller of this endpoint reads it as mail addressed to them.
    //
    // include_read matters just as much here and pulls the other way from the agent case. A
    // polling agent wants only what is new, so acknowledged mail leaves its listing. A person
    // reading a thread wants the thread — hiding a message the moment it was acknowledged would
    // make conversations lose their own history as they are read.
    const context = mailboxContext();
    if (!context.address) return;
    const request = ++state.inboxRequest;
    state.loadingRequest = request;
    state.loading = true;
    renderLoading();
    let body;
    try {
      body = await api(
        withIdentity("/v1/inbox", context.address) + "&include_sent=true&include_read=true" + actionsQuery(),
        { signal },
      );
    } catch (e) {
      if (!context.current() || signal?.aborted || request !== state.loadingRequest) return;
      // Still thrown — a caller that wants to sign out on a 401 needs to see it. This only records
      // that the postbox was not reached, which every caller of this would otherwise swallow.
      if (!(e instanceof ApiError)) setOffline(true);
      else state.loadError = e.status === 401 ? "Your session expired. Sign in again." : "Could not load your inbox. Please try again.";
      throw e;
    } finally {
      if (context.current() && request === state.loadingRequest) {
        state.loading = false;
        renderLoading();
      }
    }
    if (!context.current() || signal?.aborted || request < state.acceptedInboxRequest) return;
    state.acceptedInboxRequest = request;
    state.hasLoaded = true;
    state.loadError = null;
    setOffline(false);
    adopt(body);
    renderLoading();
  }

  // Messages this browser has acknowledged but has not yet seen the server report as read.
  //
  // In memory only, and empty again the moment the server agrees. It is a correction to listings
  // that were already in flight, not a second record of what has been read.
  const acked = new Set();

  // Take a server listing as the truth, and retire any optimistic row it now accounts for.
  function adopt(body) {
    const incoming = body.messages || [];
    // A listing is adopted whole, which is what makes the poll safe against a thread somebody is
    // reading. But a stream event or long poll opened *before* an ack was sent carries the state
    // from before it — so adopting it verbatim brings the unread mark back a second after it
    // cleared. That is the "it still says new after I read it" everyone sees.
    for (const m of incoming) {
      if (!m.read && acked.has(m.message_id)) m.read = true;
      else if (m.read) acked.delete(m.message_id);
    }
    state.inbound = incoming;
    state.policy = body.policy || null;
    const mailbox = state.me ? state.me.address : "";
    Pending.reconcile(
      new Set(state.inbound.map((m) => m.message_id)),
      new Set(state.inbound.filter((m) => m.direction === "out" && m.msg_ref).map((m) => mailbox + "\n" + m.msg_ref)),
    );
  }

  // A server that answered is a fact; a request that never arrived is weather.
  //
  // These three used to empty their slice of the state on any failure, which is right for the
  // first case — a postbox without the route genuinely has no threads — and wrong for the second,
  // where it threw away a perfectly good copy of something that had not changed. On a train that
  // is a browser that blanks its own contact list and its own archive, so filed conversations
  // reappear in the inbox, while the phone beside it holds still. `api` throws `ApiError` only
  // when the server answered; anything else is the network.
  const answered = (e) => e instanceof ApiError;

  async function loadSection(key, path, accept) {
    const context = mailboxContext();
    if (!context.address) return;
    const request = (state.sectionRequests[key] || 0) + 1;
    state.sectionRequests[key] = request;
    const current = () => context.current() && state.sectionRequests[key] === request;
    try {
      const body = await api(withIdentity(path, context.address));
      if (current()) accept(body);
    } catch (e) {
      // Keep the last good snapshot on a temporary failure. Only an unsupported route means empty.
      if (current() && answered(e) && [404, 501].includes(e.status)) accept({});
    }
  }

  async function loadThreads() {
    return loadSection("threads", "/v1/threads", body => { state.serverThreads = body.threads || []; });
  }

  async function loadContacts() {
    return loadSection("contacts", "/v1/contacts", body => {
      state.contacts = body.contacts || [];
      state.contactsReady = true;
      state.vocabulary = body.vocabulary || null;
      state.policy = body.policy || state.policy;
    });
  }

  async function loadArchive() {
    return loadSection("archive", "/v1/archive", body => {
      state.archived = new Set(body.archived || []);
    });
  }

  // The banner, not a dialog and not an empty screen: what is loaded stays readable and stays
  // scrollable, and the one thing that changes is that the page stops claiming to be current.
  // Sending is still allowed — the composer's own failure path is what says a message did not go,
  // and it says it about that message rather than about the whole app.
  function setOffline(off) {
    const next = Boolean(off);
    if (state.offline === next) return;
    state.offline = next;
    renderLoading();
  }

  // ---- archive --------------------------------------------------------------------------------

  // `#archive` in the URL, so the view can be linked to and survives a reload — that link is what
  // Settings hands out. It is still the owner's own archive behind their own sign-in: a URL that
  // showed somebody's filed mail without asking who was asking would be a way to leak it.
  function showArchive(on) {
    state.viewingArchive = Boolean(on);
    state.filter = "";
    $("search").value = "";
    closeThread();
    if (on && location.hash !== "#archive") history.replaceState(history.state, "", "#archive");
    if (!on && location.hash === "#archive") history.replaceState(history.state, "", location.pathname);
    render();
  }

  async function setArchived(peer, archived) {
    const context = mailboxContext();
    // Move it in the UI first: filing something is a gesture that should feel instant, and the
    // server call is a formality that either confirms it or is undone below.
    if (archived) state.archived.add(peer);
    else state.archived.delete(peer);
    if (state.openPeer === peer) closeThread();
    render();
    try {
      await api("/v1/archive", {
        method: "PUT",
        body: { peer, archived, identity: context.address },
      });
      if (!context.current()) return;
      toast(archived ? "Archived." : "Moved back to your inbox.");
    } catch (e) {
      if (!context.current()) return;
      if (archived) state.archived.delete(peer);
      else state.archived.add(peer);
      render();
      toast("Could not update the archive.");
    }
  }

  // ---- live mail -------------------------------------------------------------------------------
  //
  // `GET /v1/events` is a Server-Sent Events stream carrying metadata only — which mailbox got
  // mail, from whom, when — and no bodies. One stream per account rather than a held request per
  // mailbox, which is what `pigeonpost agentd` already holds.
  //
  // Read with `fetch`, not `EventSource`. Not a preference: `EventSource` cannot set a header, and
  // this stream authenticates with the same bearer token as every other call. The alternative is
  // the access token in the query string, where it lands in proxy logs and browser history, and
  // that is not a trade worth a shorter function. What `EventSource` gives — frame parsing and
  // `Last-Event-ID` resume — is about thirty lines, and doing it here means an `AbortController`
  // that actually stops the stream.
  //
  // The long poll below stays as the fallback, and reaching it is not an edge case. A postbox
  // older than this route answers 404 and a capability token is refused with `use_api_key`; both
  // say so and are easy. The third is the one worth the code: a proxy that buffers the response
  // body accepts the connection, returns 200, and then delivers nothing — an inbox that looks
  // connected and never updates, which is worse than one that admits it is polling. The server
  // sends a keep-alive every 15 seconds, so silence longer than that is not quiet, it is broken.

  let live = null;             // AbortController for the open stream, if any
  let liveCursor = null;       // last event id seen, so a reconnect resumes rather than replays
  let liveWanted = false;      // whether we should be streaming at all
  let pollFallback = false;    // the stream is unusable here; the long poll is the inbox
  // Hiding a tab and showing it again is two events with real time between them, but not always
  // enough for the aborted loop to have unwound. The generation is what makes the old one stop
  // instead of racing the new one over the same cursor.
  let liveGen = 0;
  // Silence longer than this means nothing is coming through — the server keep-alives every 15s.
  const LIVE_SILENCE_MS = 45000;
  let liveStrikes = 0;

  function resetLive() {
    liveCursor = null;
    pollFallback = false;
    liveStrikes = 0;
  }

  function stopLive() {
    liveWanted = false;
    liveGen += 1;
    if (live) live.abort();
    live = null;
    stopPolling();
  }

  function startLive() {
    window.PigeonpostCalls?.start();
    if (liveWanted || !state.me) return;
    liveWanted = true;
    if (pollFallback) { startPolling(); return; }
    runLive(++liveGen);
  }

  async function runLive(gen) {
    let backoff = 1000;
    let renewals = 0;
    while (liveWanted && gen === liveGen && !pollFallback) {
      live = new AbortController();
      try {
        // The cursor goes in the query as well as the header. The server offers it precisely
        // because not every client can set headers, and sending both costs nothing.
        // Phase 3a: `typed=1` asks for typed events (groups, expiry) whatever the account flag says.
        const path = "/v1/events?typed=1" + (liveCursor === null ? "" : "&last_event_id=" + encodeURIComponent(liveCursor));
        const res = await fetch(cfg.postbox + path, {
          headers: Object.assign(
            { authorization: `Bearer ${getToken()}`, accept: "text/event-stream" },
            liveCursor === null ? {} : { "last-event-id": String(liveCursor) },
          ),
          signal: live.signal,
        });
        if (!liveWanted || gen !== liveGen) return;
        // One renewal per expiry, not one per answer. A token the server keeps refusing after a
        // successful refresh is a disagreement no amount of refreshing settles, and retrying it in
        // a tight loop is how a client turns its own bug into the server's outage.
        if (res.status === 401) {
          if (renewals < 1 && (await renewSession())) { renewals += 1; continue; }
          signOut();
          return;
        }
        renewals = 0;
        // 404 is a postbox without the route; `use_api_key` is a capability token, which this
        // stream does not accept. Neither gets better by retrying.
        if (res.status === 404 || res.status === 400 || res.status === 403) {
          pollFallback = true;
          break;
        }
        if (!res.ok || !res.body) throw new Error("events " + res.status);

        // Mail can land between the listing that drew the screen and the stream opening, and the
        // server starts a cursor-less stream at *now*. One refresh on connect closes that gap.
        loadInbox().then(render).catch(() => {});
        backoff = 1000;

        // Armed on every byte, keep-alive included, so it measures the stream rather than the mail.
        let silent = false;
        let watchdog = null;
        const controller = live;
        const rearm = () => {
          if (watchdog) clearTimeout(watchdog);
          watchdog = setTimeout(() => { silent = true; controller.abort(); }, LIVE_SILENCE_MS);
        };
        rearm();
        try {
          await readEventStream(res.body, rearm, (event, id, data) => {
            if (!liveWanted || gen !== liveGen) return;
            if (id !== null) liveCursor = id;
            if (event !== "mail") { typedEvent(event, data); return; }
            // The stream is per account and the screen shows one mailbox. Refreshing for a sibling
            // mailbox's mail would be a fetch that changes nothing.
            let mailbox = null;
            try { mailbox = JSON.parse(data).mailbox; } catch (_) { /* a shape we do not know */ }
            if (mailbox && state.me && mailbox !== state.me.address) return;
            loadInbox().then(render).catch(() => {});
          });
        } catch (e) {
          // The watchdog aborts by design, so its own abort is not a network failure and must not
          // be reported as one. Anything else is.
          if (!silent) throw e;
        } finally {
          if (watchdog) clearTimeout(watchdog);
        }
        if (!liveWanted || gen !== liveGen) return;
        // Twice, not once: one silent stream is a bad minute, two in a row is this network.
        if (silent) {
          if (++liveStrikes >= 2) { pollFallback = true; break; }
        } else {
          liveStrikes = 0;
        }
        // A stream that ended without an error is a middlebox or a restarted server. Reconnect
        // from the cursor, which is the whole point of keeping one.
      } catch (e) {
        if (!liveWanted || gen !== liveGen) return;
        setOffline(true);
        await new Promise((r) => setTimeout(r, backoff));
        backoff = Math.min(backoff * 2, 30000);
      }
    }
    live = null;
    if (liveWanted && gen === liveGen && pollFallback) startPolling();
  }

  // A minimal SSE reader: frames separated by a blank line, `id:`/`event:`/`data:` fields, `data:`
  // repeatable and joined with newlines. Comment lines (`:`) are the keep-alive and are dropped.
  async function readEventStream(body, onActivity, onEvent) {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return;
      onActivity();
      buffer += decoder.decode(value, { stream: true });
      let cut;
      while ((cut = buffer.indexOf("\n\n")) !== -1) {
        const frame = buffer.slice(0, cut);
        buffer = buffer.slice(cut + 2);
        let event = "message";
        let id = null;
        const data = [];
        for (const raw of frame.split("\n")) {
          const line = raw.endsWith("\r") ? raw.slice(0, -1) : raw;
          if (!line || line.startsWith(":")) continue;
          const at = line.indexOf(":");
          const field = at === -1 ? line : line.slice(0, at);
          let value2 = at === -1 ? "" : line.slice(at + 1);
          if (value2.startsWith(" ")) value2 = value2.slice(1);
          if (field === "event") event = value2;
          else if (field === "data") data.push(value2);
          else if (field === "id" && /^\d+$/.test(value2)) id = Number(value2);
          // Typed events carry `<mail>.<event>` ids; the cursor is opaque to this page.
          else if (field === "id" && /^\d+\.\d+$/.test(value2)) id = value2;
        }
        if (data.length || id !== null) onEvent(event, id, data.join("\n"));
      }
    }
  }

  // The fallback. The postbox holds the request open until mail lands or the budget runs out, so
  // this is a live inbox without a socket and without hammering the server.
  let polling = false;
  let pollController = null;
  let pollGeneration = 0;

  function stopPolling() {
    polling = false;
    pollGeneration += 1;
    if (pollController) pollController.abort();
    pollController = null;
  }

  async function startPolling() {
    if (polling || !state.me) return;
    polling = true;
    const gen = ++pollGeneration;
    const context = mailboxContext();
    let backoff = 1000;
    while (polling && gen === pollGeneration && context.current()) {
      pollController = new AbortController();
      const request = ++state.inboxRequest;
      try {
        // `include_read=true` is not optional here, even though this is the *polling* call.
        //
        // The server drops acknowledged inbound mail from a listing that does not ask for it —
        // right for an agent draining what is new, wrong for a person reading a thread. And
        // `adopt` takes each listing as the whole truth. Omitting it therefore did not merely fail
        // to add new mail: the first poll after load replaced a full conversation with only its
        // unread part, so messages appeared and then vanished a few seconds later. The two calls
        // must ask the same question, or they answer each other.
        const path = withIdentity("/v1/inbox") + "&include_sent=true&include_read=true" + actionsQuery()
          + "&wait=" + encodeURIComponent(cfg.waitSeconds || 25);
        const body = await api(path, { signal: pollController.signal });
        if (!polling || gen !== pollGeneration || !context.current()) break;
        if (request < state.acceptedInboxRequest) continue;
        state.acceptedInboxRequest = request;
        state.hasLoaded = true;
        state.loadError = null;
        setOffline(false);
        adopt(body);
        render();
        backoff = 1000;
      } catch (e) {
        if (!polling || gen !== pollGeneration || !context.current()) break;
        if (e instanceof ApiError && e.status === 401) { signOut(); return; }
        // Anything else — offline, proxy hiccup — is temporary. Back off rather than spin.
        if (!(e instanceof ApiError)) setOffline(true);
        await new Promise((r) => setTimeout(r, backoff));
        backoff = Math.min(backoff * 2, 30000);
      }
    }
  }

  // ---- wiring -------------------------------------------------------------------------------

  // ---- sheets ---------------------------------------------------------------------------------

  // Escape closes the topmost sheet, and a click on the backdrop does too. Both are what people
  // try first, and a dialog that ignores them reads as stuck.
  // Most recently opened last, so Escape closes what is actually on top. A fixed list closes
  // whichever happens to be first in it, which is only the topmost by luck.
  const sheetStack = [];
  function closeMailboxSheets() {
    for (const id of [...sheetStack]) closeSheet(id);
    deletingThread = null;
    $("identity-menu").hidden = true;
    $("identity-btn").setAttribute("aria-expanded", "false");
    $("thread-create").disabled = false;
    $("contact-save").disabled = false;
  }
  function openSheet(id) {
    $(id).hidden = false;
    const at = sheetStack.indexOf(id);
    if (at !== -1) sheetStack.splice(at, 1);
    sheetStack.push(id);
    syncSheetFocus();
  }
  function syncSheetFocus() {
    $("app").inert = sheetStack.length > 0;
    for (const id of sheetStack) {
      $(id).inert = id !== sheetStack.at(-1);
      $(id).style.zIndex = String(40 + sheetStack.indexOf(id));
    }
  }
  function closeSheet(id) {
    $(id).hidden = true;
    const at = sheetStack.indexOf(id);
    if (at !== -1) sheetStack.splice(at, 1);
    syncSheetFocus();
    if (id === "settings-sheet") $("settings-btn").focus();
    if (id === "peer-info-sheet") {
      if (!$("peer-block-sheet").hidden) closeSheet("peer-block-sheet");
      if (!$("contact-sheet").hidden) closeSheet("contact-sheet");
      state.showInfo = false;
      for (const button of ["peer-name-btn", "peer-info-btn"]) $(button).setAttribute("aria-expanded", "false");
      if (!$("thread-head").hidden) peerInfoTrigger?.focus({ preventScroll: true });
    } else if (id === "peer-block-sheet") {
      blockingPeer = null;
      if (state.showInfo) $("peer-block").focus();
    } else if (id === "contact-sheet" && state.showInfo) $("peer-requests").focus();
  }
  function wireSheet(id, onBackdrop) {
    const wrap = $(id);
    wrap.addEventListener("mousedown", (e) => {
      if (e.target === wrap) (onBackdrop || (() => closeSheet(id)))();
    });
  }

  // ---- new conversation -------------------------------------------------------------------------

  function openNewConversation() {
    $("new-peer").value = "/";
    $("new-body").value = "";
    $("new-error").hidden = true;
    $("new-send").disabled = false;
    openSheet("new-sheet");
    $("new-peer").focus();
  }

  async function sendNewConversation() {
    if ($("new-send").disabled) return;
    const context = mailboxContext();
    normaliseAddressInput($("new-peer"));
    const to = $("new-peer").value.trim();
    const body = $("new-body").value.trim();
    const fail = (message) => {
      const el = $("new-error");
      el.textContent = message;
      el.hidden = false;
    };
    if (to === "/" || !to) return fail("Who is it for?");
    if (!/^\/[^\s/]+(?:\/[^\s/]+)*$/.test(to)) return fail("Enter a post address such as /bekir or /bekir/agent1.");
    if (!body) return fail("Write something to send.");

    $("new-send").disabled = true;
    // Through `composeBody` for the same reason the composer is: the first message to a peer is
    // still a message from this app, and sending it as prose is what left it classified
    // `not_a_request` and parked for a human. That the *opening* message was the one going out
    // bare is what made this look like the recipient was broken rather than the sender — every
    // reply after it was already an envelope, so nothing further in the thread showed it.
    //
    // Sent as a pending record, like the composer's, so the opening message has the same answer to
    // a lost response: its own `msg_ref`, a row in the new conversation, and Retry or Discard.
    const msgRef = actionsOn() ? newMsgRef() : null;
    const record = {
      msg_ref: msgRef,
      reply_to: null,
      local_id: "local_" + randomString(8),
      mailbox: context.address,
      to,
      body: composeBody(body),
      at: Math.floor(Date.now() / 1000),
      status: "sending",
      thread_id: null,
      attachments: [],
    };
    record.request = { to, body: record.body, from: context.address, ...(msgRef ? { msg_ref: msgRef } : {}) };
    let sent;
    try {
      sent = await deliver(record);
    } catch (e) {
      if (!context.current()) return;
      $("new-send").disabled = false;
      // Refused: nothing was started, and what to fix is in this sheet.
      if (!record.uncertain) return fail(sendFailure(e));
      // Not confirmed: the conversation may well exist on their side already. Open it here, in the
      // mailbox it was written from, with the message waiting on Retry or Discard — leaving it in a
      // sheet that says "could not reach" invites typing it all again as a second message.
      Pending.add(record);
      closeSheet("new-sheet");
      openStarted(normalisePeer(to));
      return;
    }
    if (!context.current()) return;
    if (sent.sent_copy_id) Pending.add(record);
    closeSheet("new-sheet");
    $("new-send").disabled = false;
    // Shown at once from the record, then from the server's listing. A reload that fails is the
    // live loop's to retry; it must not leave the conversation that was just started unopened.
    const first = normalisePeer(to);
    openStarted(first);
    try {
      await loadAll();
    } catch {
      /* the optimistic row is already on screen */
    }
    // Somebody who has moved on to another conversation in the meantime keeps it.
    if (!context.current() || state.openPeer !== first) return;
    // A conversation started with a namespace or a `/k/` address is filed by the server under
    // whatever peer it resolved to, so open by what came back rather than by what was typed. Opened
    // again even when it is the same peer: its thread list has only just arrived, and that is what
    // selects the conversation's thread rather than the list of them.
    const copy = state.inbound.find(m => m.message_id === sent.sent_copy_id);
    const resolved = copy ? peerKeyOf(copy) : first;
    const started = buildThreads().find((t) => t.peer === resolved || t.peer === to);
    openStarted(started ? started.peer : first);
  }

  // The conversation a first message just started, opened and selected with its composer ready:
  // the next thing anyone does after "hello" is wait for the answer in that conversation, not pick
  // it out of the list. Focus goes to the composer on a phone too — the keyboard is already up from
  // writing the first message, so this keeps it rather than raising it.
  function openStarted(peer) {
    if (!peer) return render();
    openThread(peer);
    $("compose").focus({ preventScroll: true });
  }

  // ---- settings --------------------------------------------------------------------------------

  let settingsPage = "root";
  const settingsTitles = { root: "Settings", account: "Account", handles: "Handles",
    inbox: "Inbox and appearance", contacts: "Contacts and permissions", help: "Help and about",
    profile: "Profile picture", privacy: "Privacy and filtering", blocked: "Blocked" };
  function showSettingsPage(next, focus = true) {
    if (!Object.hasOwn(settingsTitles, next)) return;
    const previous = settingsPage;
    settingsPage = next;
    document.querySelectorAll("[data-settings-page]").forEach(el => { el.hidden = el.dataset.settingsPage !== next; });
    $("settings-title").textContent = settingsTitles[next];
    if (next === "profile") openProfilePage();
    if (next === "privacy") openPrivacyPage();
    if (next === "blocked") openBlockedPage();
    $("settings-back").hidden = next === "root";
    $("settings-sheet").querySelector(".sheet-body").scrollTop = 0;
    if (focus) {
      const target = next === "root" ? $("settings-nav-" + previous) || $("settings-nav-account") : $("settings-title");
      target.focus();
    }
  }

  let handlesRequest = 0;
  async function loadAccountHandles() {
    const list = $("acct-handles"), refresh = $("acct-handles-refresh");
    if (!list) return;
    const owner = state, request = ++handlesRequest;
    list.textContent = "Loading account handles…";
    refresh.disabled = true;
    try {
      const result = await api("/v1/me/handles?include_inactive=true");
      if (owner !== state || request !== handlesRequest) return;
      if (!Array.isArray(result.handles)) throw new Error("Invalid handle response");
      list.textContent = result.handles.length ? "" : "No handles on this Pigeonpost account yet.";
      for (const handle of result.handles) {
        const row = document.createElement("p");
        row.className = "field-note";
        const name = "/" + String(handle.namespace).replace(/^\/+/, "");
        const provider = handle.source === "apple" ? "App Store" : handle.source === "google" ? "Google Play" : "Pigeonpost";
        row.textContent = name + " · " + (handle.active ? "Active" : "Expired") + " · " + provider;
        list.append(row);
      }
    } catch (_) {
      if (owner === state && request === handlesRequest) list.textContent = "Could not refresh your account handles. Your registrations are saved. Try Refresh again.";
    } finally { if (owner === state && request === handlesRequest) refresh.disabled = false; }
  }

  function openSettings() {
    applyMessageScale(messageScale());
    syncThemeChoice();
    // An unnamed mailbox has no handle, and saying "—" is more honest than repeating its address
    // on the line above the one that already shows it.
    $("acct-mailbox").textContent = state.me ? (state.me.handle || "not named") : "—";
    $("acct-address").textContent = state.me ? state.me.address : "—";
    configureAddressCopy($("copy-acct-mailbox"), state.me?.handle);
    configureAddressCopy($("copy-acct-address"), state.me?.address);
    $("acct-postbox").textContent = cfg.postbox;
    $("archive-count").textContent =
      state.archived.size === 1 ? "1 conversation" : state.archived.size + " conversations";
    $("archive-link").textContent = location.origin + "/#archive";
    renderContactList();
    $("acct-handles-refresh").onclick = () => {
      const context = mailboxContext();
      loadAccountHandles();
      loadIdentities().then(() => { if (context.current()) renderMe(); }).catch(() => {
        if (context.current()) toast("Could not refresh your mailboxes. Please try again.");
      });
    };
    loadAccountHandles();
    syncSettingsRows();
    showSettingsPage("root", false);
    openSheet("settings-sheet");
    $("settings-nav-account").focus();
  }

  function renderContactList() {
    const list = $("contact-list");
    list.textContent = "";
    if (!state.contacts.length) {
      const li = document.createElement("li");
      li.className = "cl-empty";
      li.textContent = "Nobody is listed yet. Strangers get whatever the inbox policy allows.";
      list.append(li);
      return;
    }
    for (const c of state.contacts) {
      const li = document.createElement("li");
      const who = document.createElement("div");
      who.className = "cl-who";
      const peer = document.createElement("span");
      peer.className = "cl-peer";
      peer.textContent = c.peer;
      const terms = document.createElement("span");
      terms.className = "cl-terms";
      const verbs = (c.allowed_verbs || []).join(", ");
      terms.textContent = [
        c.alias,
        c.admission === "block" ? "blocked" : "allowed",
        c.autonomy === "auto" ? "auto" : "review",
        verbs || null,
      ]
        .filter(Boolean)
        .join(" · ");
      who.append(peer, terms);

      const edit = document.createElement("button");
      edit.type = "button";
      edit.className = "btn-ghost";
      edit.textContent = "Edit";
      edit.onclick = () => openContact(c);

      li.append(who, edit);
      list.append(li);
    }
  }

  // ---- one trusted sender -----------------------------------------------------------------------

  let editingContact = null;

  // `prefillPeer` is for the sender panel, where the address is already on screen and known. It is
  // still an add rather than an edit — the row does not exist yet — so the field stays editable.
  function openContact(contact, prefillPeer) {
    editingContact = contact || null;
    $("contact-title").textContent = contact ? "Edit sender" : "Add a sender";
    $("contact-peer").value = contact ? contact.peer : (prefillPeer || "");
    // The address is the identity of the row, so changing it would be adding a different sender
    // rather than editing this one. Add and remove is the honest way to do that.
    $("contact-peer").disabled = Boolean(contact);
    $("contact-alias").value = (contact && contact.alias) || "";
    $("contact-admission").value = (contact && contact.admission) || "allow";
    $("contact-autonomy").value = (contact && contact.autonomy) || "review";
    $("contact-remove").hidden = !contact;
    $("contact-error").hidden = true;
    renderVerbs(contact);
    openSheet("contact-sheet");
    if (!contact && !prefillPeer) $("contact-peer").focus();
  }

  function renderVerbs(contact) {
    const box = $("contact-verbs");
    box.textContent = "";
    const legend = document.createElement("legend");
    legend.textContent = "Requests they may have acted on";
    box.append(legend);

    const vocab = state.vocabulary || { grantable: [], never_auto: [] };
    const granted = new Set((contact && contact.allowed_verbs) || []);
    for (const verb of vocab.grantable || []) {
      const label = document.createElement("label");
      const box2 = document.createElement("input");
      box2.type = "checkbox";
      box2.value = verb;
      box2.checked = granted.has(verb);
      label.append(box2, document.createTextNode(verb));
      box.append(label);
    }
    // Shown, not hidden: knowing which requests are never automatic is the reassurance that makes
    // the automatic ones safe to grant.
    for (const verb of vocab.never_auto || []) {
      const label = document.createElement("label");
      label.className = "never";
      const box2 = document.createElement("input");
      box2.type = "checkbox";
      box2.disabled = true;
      label.append(box2, document.createTextNode(verb + " — never automatic"));
      box.append(label);
    }
  }

  async function saveContact() {
    const context = mailboxContext();
    const peer = $("contact-peer").value.trim();
    const fail = (message) => {
      const el = $("contact-error");
      el.textContent = message;
      el.hidden = false;
    };
    if (!peer) return fail("Whose address is this?");

    const verbs = [...$("contact-verbs").querySelectorAll("input:checked:not(:disabled)")]
      .map((i) => i.value);
    $("contact-save").disabled = true;
    try {
      await api("/v1/contacts", {
        method: "PUT",
        body: {
          peer,
          alias: $("contact-alias").value.trim() || null,
          admission: $("contact-admission").value,
          autonomy: $("contact-autonomy").value,
          allowed_verbs: verbs,
          identity: context.address,
        },
      });
      if (!context.current()) return;
      closeSheet("contact-sheet");
      await loadContacts();
      if (!context.current()) return;
      renderContactList();
      render();
    } catch (e) {
      if (context.current()) fail(e && e.message ? e.message : "Could not save.");
    } finally {
      if (context.current()) $("contact-save").disabled = false;
    }
  }

  async function removeContact() {
    if (!editingContact) return;
    const context = mailboxContext();
    try {
      await api("/v1/contacts", {
        method: "DELETE",
        body: { peer: editingContact.peer, identity: context.address },
      });
      if (!context.current()) return;
      closeSheet("contact-sheet");
      await loadContacts();
      if (!context.current()) return;
      renderContactList();
      render();
      toast("Removed. They get whatever strangers get.");
    } catch (e) {
      if (context.current()) toast("Could not remove them.");
    }
  }

  function wireColumns() {
    let saved = {};
    try { saved = JSON.parse(LS.getItem("ppi_columns") || "{}"); } catch (_) { /* use defaults */ }
    const columns = [
      { id: "list-resizer", name: "list", initial: 320, min: 240, max: 480 },
      { id: "subs-resizer", name: "subs", initial: 240, min: 180, max: 360 },
    ];
    const widths = {};
    const apply = (column, value, persist = false) => {
      const other = column.name === "list" ? (widths.subs || 240) : (widths.list || 320);
      const max = Math.max(column.min, Math.min(column.max, window.innerWidth - other - 340));
      const width = Math.min(max, Math.max(column.min, Number(value) || column.initial));
      widths[column.name] = width;
      document.documentElement.style.setProperty(`--${column.name}-w`, width + "px");
      const divider = $(column.id);
      divider.setAttribute("aria-valuemin", column.min);
      divider.setAttribute("aria-valuemax", max);
      divider.setAttribute("aria-valuenow", width);
      if (persist) { saved = { ...widths }; LS.setItem("ppi_columns", JSON.stringify(saved)); }
    };
    for (const column of columns) {
      apply(column, saved?.[column.name]);
      const divider = $(column.id);
      let drag = null;
      divider.addEventListener("pointerdown", e => {
        if (e.button !== 0) return;
        e.preventDefault();
        drag = { x: e.clientX, width: widths[column.name] };
        divider.setPointerCapture(e.pointerId);
      });
      divider.addEventListener("pointermove", e => {
        if (!drag) return;
        apply(column, drag.width + e.clientX - drag.x, true);
      });
      const end = () => { drag = null; };
      divider.addEventListener("pointerup", end);
      divider.addEventListener("pointercancel", end);
      divider.addEventListener("lostpointercapture", end);
      divider.addEventListener("dblclick", () => apply(column, column.initial, true));
      divider.addEventListener("keydown", e => {
        const amount = e.shiftKey ? 40 : 10;
        const value = { ArrowLeft: widths[column.name] - amount, ArrowRight: widths[column.name] + amount,
          Home: column.min, End: column.max }[e.key];
        if (value === undefined) return;
        e.preventDefault();
        apply(column, value, true);
      });
    }
    window.addEventListener("resize", () => { for (const column of columns) apply(column, saved?.[column.name]); });
  }

  function wire() {
    $("viewer-close").addEventListener("click", closeViewer);
    $("viewer-download").addEventListener("click", () => { if (viewerState) downloadAttachment(viewerState.item); });
    // Capture phase, so Escape closes the viewer before anything underneath reacts to it.
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && viewerState) { e.preventDefault(); e.stopPropagation(); closeViewer(); }
    }, true);
    $("signin-btn").onclick = () => login();
    $("signout-btn").onclick = () => signOut();

    $("new-btn").onclick = () => openNewConversation();
    $("new-close").onclick = () => closeSheet("new-sheet");
    $("new-cancel").onclick = () => closeSheet("new-sheet");
    $("new-send").onclick = () => sendNewConversation();
    $("new-peer").addEventListener("input", e => { if (!e.isComposing) normaliseAddressInput(e.target); });
    $("new-peer").addEventListener("compositionend", e => normaliseAddressInput(e.target));
    wireSheet("new-sheet");

    $("settings-btn").onclick = () => openSettings();
    document.querySelectorAll("[data-settings-open]").forEach(el => { el.onclick = () => showSettingsPage(el.dataset.settingsOpen); });
    $("settings-back").onclick = () => showSettingsPage("root");
    $("settings-signout").onclick = () => signOut();
    $("settings-close").onclick = () => closeSheet("settings-sheet");
    $("settings-done").onclick = () => closeSheet("settings-sheet");
    wireSheet("settings-sheet");

    $("retry-inbox").onclick = () => loadAll().catch(e => {
      if (e instanceof ApiError && e.status === 401) signOut();
    });
    $("load-older").onclick = loadOlderMessages;
    $("jump-latest").onclick = jumpToLatest;
    $("find-btn").onclick = openFind;
    $("find-close").onclick = closeFind;
    $("find-prev").onclick = () => findStep(-1);
    $("find-next").onclick = () => findStep(1);
    $("find-input").addEventListener("input", e => {
      clearTimeout(findTimer);
      const view = conversationView, query = e.target.value.trim();
      findTimer = setTimeout(() => {
        if (conversationView !== view || !view) return;
        view.query = query;
        view.hit = 0;
        view.seek = true;
        renderThread();
        ackVisible();
      }, 160);
    });
    $("find-input").addEventListener("keydown", e => {
      if (e.key === "Enter") { e.preventDefault(); findStep(e.shiftKey ? -1 : 1); }
    });
    $("thread-scroll").addEventListener("scroll", () => {
      const view = conversationView, scroll = $("thread-scroll");
      if (!view) return;
      const movedUp = scroll.scrollTop < view.lastTop - 1;
      const moved = Math.abs(scroll.scrollTop - view.lastTop) > 1;
      if (!moved) return;
      view.follow = !view.endId && scroll.scrollHeight - scroll.clientHeight - scroll.scrollTop <= 2;
      view.lastTop = scroll.scrollTop;
      view.anchor = readingAnchor();
      $("jump-latest").hidden = view.follow;
      if (movedUp && scroll.scrollTop < 80) loadOlderMessages();
    }, { passive: true });
    if (typeof ResizeObserver !== "undefined") {
      const observer = new ResizeObserver(scheduleMessageLayout);
      observer.observe($("messages"));
      observer.observe($("thread-scroll"));
    }
    document.fonts?.ready.then(scheduleMessageLayout);
    $("messages").addEventListener("load", scheduleMessageLayout, true);
    $("delete-thread-btn").onclick = askDeleteThread;
    $("delete-thread-confirm").onclick = deleteThread;
    $("delete-thread-cancel").onclick = () => closeSheet("delete-thread-sheet");
    wireSheet("delete-thread-sheet");
    wireColumns();

    for (const input of document.querySelectorAll('#theme-choice input[name="theme"]')) {
      input.addEventListener("change", () => { if (input.checked) applyTheme(input.value, true); });
    }

    $("size-down").onclick = () =>
      applyMessageScale(Math.max(0.8, Math.round((messageScale() - 0.1) * 10) / 10));
    $("size-up").onclick = () =>
      applyMessageScale(Math.min(2, Math.round((messageScale() + 0.1) * 10) / 10));

    $("open-archive").onclick = () => {
      closeSheet("settings-sheet");
      showArchive(true);
    };
    $("archive-exit").onclick = () => showArchive(false);
    $("archive-btn").onclick = () => {
      if (!state.openPeer) return;
      setArchived(state.openPeer, !state.archived.has(state.openPeer));
    };

    $("contact-add").onclick = () => openContact(null);
    $("contact-close").onclick = () => closeSheet("contact-sheet");
    $("contact-cancel").onclick = () => closeSheet("contact-sheet");
    $("contact-save").onclick = () => saveContact();
    $("contact-remove").onclick = () => removeContact();
    wireSheet("contact-sheet");

    document.addEventListener("keydown", (e) => {
      if (e.key === "Tab" && sheetStack.length) {
        const items = [...$(sheetStack.at(-1)).querySelectorAll("button:not(:disabled), a[href], input:not(:disabled), textarea:not(:disabled), select:not(:disabled)")]
          .filter(el => !el.closest("[hidden]"))
          // Document order, which a selector list does not promise in every engine (jsdom groups
          // the matches by selector, which put the theme radios after the footer's Done).
          .sort((a, b) => (a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1));
        const first = items[0], last = items.at(-1);
        if (first && e.shiftKey && (document.activeElement === first || document.activeElement === $("settings-title"))) { e.preventDefault(); last.focus(); }
        else if (last && !e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
      }
      if (e.key !== "Escape") return;
      for (let i = sheetStack.length - 1; i >= 0; i -= 1) {
        const id = sheetStack[i];
        if (!$(id).hidden) {
          if (id === "settings-sheet" && settingsPage !== "root") { e.preventDefault(); showSettingsPage("root"); return; }
          closeSheet(id);
          e.preventDefault();
          return;
        }
        // A sheet hidden without going through closeSheet leaves a stale entry; drop it rather
        // than letting it swallow the keystroke.
        sheetStack.splice(i, 1);
      }
    });

    $("identity-btn").onclick = () => {
      const menu = $("identity-menu");
      const open = menu.hidden;
      menu.hidden = !open;
      $("identity-btn").setAttribute("aria-expanded", String(open));
    };
    document.addEventListener("click", (e) => {
      if (!$("identity-menu").hidden && !e.target.closest(".me") && !e.target.closest(".identity-menu")) {
        $("identity-menu").hidden = true;
        $("identity-btn").setAttribute("aria-expanded", "false");
      }
    });

    // Going back through history is what actually closes the thread on a phone, so the on-screen
    // back button asks history to do it rather than closing the pane behind history's back.
    $("back-btn").onclick = () => {
      if (onPhone() && history.state && history.state.thread) history.back();
      else closeThread();
    };

    // Back out of the threads screen on a phone: one step further out than the message screen's.
    $("subs-back").onclick = () => {
      if (onPhone() && history.state && history.state.thread) history.back();
      else {
        state.openPeer = null;
        state.openThread = null;
        render();
      }
    };

    $("subs-new").onclick = () => { newSubthread(); };
    wireSheet("thread-sheet");
    $("thread-close").onclick = () => closeSheet("thread-sheet");
    $("thread-cancel").onclick = () => closeSheet("thread-sheet");
    $("thread-create").onclick = () => { createSubthread(); };
    // Enter is what people press in a one-field dialog.
    $("thread-title-input").addEventListener("keydown", (e) => {
      if (e.key === "Enter") { e.preventDefault(); createSubthread(); }
    });

    for (const id of ["peer-name-btn", "peer-info-btn"]) $(id).onclick = () => openPeerInfo($(id));
    for (const id of ["peer-info-close", "peer-info-done"]) $(id).onclick = () => closeSheet("peer-info-sheet");
    wireSheet("peer-info-sheet");
    wireSheet("peer-block-sheet");
    $("peer-known").onchange = () => updateSender(state.openPeer, $("peer-known").checked ? "known" : "forget");
    $("peer-full").onchange = () => updateSender(state.openPeer, $("peer-full").checked ? "full" : "review");
    $("peer-retry").onclick = async () => {
      const context = mailboxContext();
      state.contactsRetrying = true;
      renderThread();
      await loadContacts();
      if (context.current()) { state.contactsRetrying = false; render(); }
    };
    $("peer-requests").onclick = () => {
      const exact = state.contacts.find(row => row.peer === state.openPeer);
      if (exact) { openContact(exact); $("contact-alias").focus(); }
    };
    $("peer-archive").onclick = () => setArchived(state.openPeer, !state.archived.has(state.openPeer));
    $("peer-block").onclick = () => {
      if (contactFor(state.openPeer)?.admission === "block") { updateSender(state.openPeer, "known"); return; }
      askBlock({ peer: state.openPeer, name: $("peer-info-title").textContent, origin: "info" });
    };
    $("peer-block-cancel").onclick = () => closeSheet("peer-block-sheet");
    $("peer-block-confirm").onclick = () => confirmBlock();
    wirePhase1b();
    // Phase 1e: reply/edit bar, message info, the message menu.
    $("compose-context-close").addEventListener("click", () => { cancelComposeMode(); $("compose").focus({ preventScroll: true }); });
    wireSheet("msg-info-sheet");
    $("msg-info-done").addEventListener("click", () => closeSheet("msg-info-sheet"));
    document.addEventListener("mousedown", (e) => { if (!$("msg-menu").hidden && !$("msg-menu").contains(e.target)) closeMessageMenu(); });
    document.addEventListener("scroll", () => closeMessageMenu(), true);
    $("msg-menu").addEventListener("keydown", (e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); closeMessageMenu(); } });

    $("search").oninput = (e) => {
      state.filter = e.target.value;
      renderThreadList();
    };

    const compose = $("compose");
    const autosize = () => {
      compose.style.height = "auto";
      const style = getComputedStyle(compose);
      const border = (parseFloat(style.borderTopWidth) || 0) + (parseFloat(style.borderBottomWidth) || 0);
      compose.style.height = Math.min(compose.scrollHeight + border, window.innerHeight * 0.4) + "px";
      updateSendAvailability();
    };
    compose.addEventListener("input", autosize);

    // Enter sends, Shift+Enter breaks the line — but only where there is a keyboard to do it with.
    compose.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && composeMode) {
        e.preventDefault();
        e.stopPropagation();
        cancelComposeMode();
        return;
      }
      if (e.key === "Enter" && !e.shiftKey && !e.isComposing && window.matchMedia("(min-width: 781px)").matches) {
        e.preventDefault();
        $("composer").requestSubmit();
      }
    });

    wireAttach();
    wireDrop();
    $("composer").addEventListener("submit", (e) => {
      e.preventDefault();
      const text = compose.value.trim();
      const key = composerKey();
      if ((!text && !staged.length) || !state.openPeer || sendingDrafts.has(key)) return;
      // Phase 1e: an edit replaces a message rather than sending one; a reply sends one that names
      // its parent. Either mode is dropped if the conversation changed underneath it.
      if (composeMode && composeMode.peer !== state.openPeer) setComposeMode(null);
      const mode = composeMode;
      if (mode?.kind === "edit" && !text) return;
      sendingDrafts.add(key);
      compose.disabled = true;
      autosize();
      let job;
      if (mode?.kind === "edit") job = sendEdit(text);
      else {
        if (mode?.kind === "reply") setComposeMode(null);
        job = sendMessage(text, mode?.kind === "reply" ? mode.m : null);
      }
      job.finally(() => {
        sendingDrafts.delete(key);
        if (composerKey() === key) {
          compose.disabled = false;
          autosize();
          // Disabling the box while it sends drops the focus; give it back so the next message can
          // be typed straight away — unless the person has since moved somewhere else on purpose.
          const away = document.activeElement;
          if (!away || away === document.body || away === compose || away === $("send-btn")) compose.focus({ preventScroll: true });
        }
      });
    });

    // One history entry covers a peer, and closeThread walks out of it a screen at a time. Pushing
    // it back when there is still a level to leave is what keeps the system gesture in step with
    // the on-screen back button.
    window.addEventListener("popstate", () => {
      if (!state.openPeer) return;
      const stillInside = onPhone() && state.openThread !== null && subsVisible();
      closeThread();
      if (stillInside) history.pushState({ thread: state.openPeer }, "");
    });

    document.addEventListener("keydown", (e) => {
      if (e.defaultPrevented) return;
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f" && state.openPeer && !sheetStack.length) {
        e.preventDefault(); openFind(); return;
      }
      if (e.key !== "Escape") return;
      if (!$("identity-menu").hidden) {
        $("identity-menu").hidden = true;
        $("identity-btn").setAttribute("aria-expanded", "false");
        $("identity-btn").focus();
      } else if (!$("find-bar").hidden) closeFind();
      else if (state.openPeer) closeThread();
    });

    // A hidden tab holds nothing open. A stream a phone has backgrounded is a socket the browser
    // will freeze or drop anyway, and reopening from the cursor on the way back loses nothing —
    // that is what `Last-Event-ID` is for.
    document.addEventListener("visibilitychange", () => {
      if (!getToken() || !state.me) return;
      if (document.visibilityState === "visible") {
        // Current mail, not a stale view, before the stream has said anything.
        loadInbox().then(render).catch(() => {});
        startLive();
      } else {
        stopLive();
      }
    });

    // The browser knows before a request has to time out to find it. Reopening the stream is not
    // enough on its own: one that is already open but asleep in its backoff will not notice for up
    // to thirty seconds, and until it does the page still says it is offline. Ask for the mail.
    window.addEventListener("online", () => {
      if (!getToken() || !state.me || document.visibilityState !== "visible") return;
      loadInbox().then(render).catch(() => {});
      startLive();
    });
    window.addEventListener("offline", () => setOffline(true));

    $("send-btn").disabled = true;
  }

  // ---- Phase 1b: capabilities, profile pictures, calls, filtering and blocking -------------------
  //
  // Every feature below asks `GET /v1/capabilities` first and stays invisible when the postbox has
  // not switched it on — a page released ahead of its server must look exactly like the old one.
  // Blocking is the exception: it uses contact routes every postbox already has.

  let features = new Set();
  const has = (name) => features.has(name);
  // Phase 1e: a postbox with any message action on lists the raw view with edits folded in.
  const actionsOn = () => has("message_edit") || has("message_reply") || has("receipts") || has("agent_status");
  const actionsQuery = () => (actionsOn() ? "&actions=1&fold=1" : "");
  const actionFlags = () => ({ edit: has("message_edit"), reply: has("message_reply"), receipts: has("receipts"), agentStatus: has("agent_status") });

  async function loadCapabilities() {
    const version = sessionVersion;
    let next = new Set();
    try {
      const body = await api("/v1/capabilities");
      if (Array.isArray(body.features)) next = new Set(body.features.map(String));
    } catch (_) { /* an older postbox: nothing new is on */ }
    if (version !== sessionVersion) return;
    features = next;
    Avatars.enabled = has("avatars");
    syncSideNav();
    window.PigeonpostGroups?.onMailbox(); // Phase 3a
    window.PigeonpostCalendar?.onMailbox(); // Phase 3B
    window.PigeonpostNotes?.onMailbox(); // Phase 7
  }

  function resetPhase1b() {
    features = new Set();
    Avatars.reset();
    blockTarget = null;
    ownAvatar = null;
    crop = null;
    clearTimeout(callReloadTimer);
    syncSideNav();
  }

  // Typed events on the account stream. Unknown types are ignored, as the stream's contract says.
  let callReloadTimer = null;
  let receiptReloadTimer = null;
  function typedEvent(event, raw) {
    let frame = null;
    try { frame = JSON.parse(raw); } catch (_) { return; }
    if (!frame || typeof frame !== "object") return;
    const data = frame.data && typeof frame.data === "object" ? frame.data : frame;
    if (event === "call.log") {
      const mailbox = frame.mailbox || data.owner;
      if (mailbox && state.me && mailbox !== state.me.address) return;
      const context = mailboxContext();
      clearTimeout(callReloadTimer);
      callReloadTimer = setTimeout(() => {
        if (!context.current()) return;
        loadCallLog().then(() => { if (context.current()) render(); });
      }, 250);
    } else if (event === "profile.avatar") {
      Avatars.forget([data.address, data.handle]);
      if (settingsPage === "profile" && !$("settings-sheet").hidden) loadOwnAvatar();
    } else if (event === "message.expired" && data.thread_id) {
      // Phase 3a: drop expired direct messages now rather than at the next listing.
      const ids = new Set(data.ids || []);
      state.inbound = state.inbound.filter((m) => !ids.has(m.message_id));
      render();
    } else if (event === "message.receipt" || event === "message.edited") {
      // Phase 1e: ticks moved, or a message changed under somebody's eyes. One listing settles both.
      const mailbox = frame.mailbox || data.mailbox;
      if (mailbox && state.me && mailbox !== state.me.address) return;
      const context = mailboxContext();
      clearTimeout(receiptReloadTimer);
      receiptReloadTimer = setTimeout(() => {
        if (context.current()) loadInbox().then(render).catch(() => {});
      }, 200);
    } else if (event === "thread.settings") {
      loadThreads().then(render).catch(() => {});
    }
    if (/^(group\.|mls\.|message\.expired|thread\.settings)/.test(event)) window.PigeonpostGroups?.onEvent(event, frame); // Phase 3a
    if (/^event\./.test(event)) window.PigeonpostCalendar?.onEvent(event, frame); // Phase 3B
    if (/^meeting\./.test(event)) window.PigeonpostNotes?.onEvent(event, frame); // Phase 7
  }

  // ---- profile pictures ---------------------------------------------------------------------

  // Pictures are fetched with the bearer token (a "people I know" picture is not public), turned
  // into object URLs and laid over the initials, which stay underneath as the fallback.
  const Avatars = {
    enabled: false,
    meta: new Map(),     // key → { hash, url } or null (no picture, or not one this account may see)
    urls: new Map(),     // hash → object URL
    loading: new Map(),  // hash → promise
    queue: new Set(),
    timer: null,
    generation: 0,
    controller: new AbortController(),
    reset() {
      for (const url of this.urls.values()) { try { URL.revokeObjectURL(url); } catch (_) { /* jsdom */ } }
      this.meta.clear(); this.urls.clear(); this.loading.clear(); this.queue.clear();
      clearTimeout(this.timer); this.timer = null;
      this.controller.abort(); this.controller = new AbortController();
      this.generation += 1;
      this.enabled = false;
    },
    clear(el) {
      el.classList.remove("has-photo");
      el.style.backgroundImage = "";
    },
    paint(el, url) {
      el.classList.add("has-photo");
      el.style.backgroundImage = 'url("' + url + '")';
    },
    decorate(el, key) {
      if (!el) return;
      this.clear(el);
      if (!this.enabled || !key || key.endsWith("/*") || !key.startsWith("/")) { delete el.dataset.avatarKey; return; }
      el.dataset.avatarKey = key;
      const meta = this.meta.get(key);
      if (meta === undefined) { this.want(key); return; }
      if (meta && this.urls.has(meta.hash)) this.paint(el, this.urls.get(meta.hash));
    },
    want(key) {
      if (this.queue.has(key)) return;
      this.queue.add(key);
      if (!this.timer) this.timer = setTimeout(() => this.flush(), 30);
    },
    async flush() {
      this.timer = null;
      const keys = [...this.queue].slice(0, 100);
      keys.forEach((k) => { this.queue.delete(k); this.meta.set(k, null); });
      if (this.queue.size) this.timer = setTimeout(() => this.flush(), 30);
      if (!keys.length || !getToken()) return;
      const generation = this.generation;
      let body;
      try {
        body = await api("/v1/avatars?addresses=" + keys.map(encodeURIComponent).join(","), { signal: this.controller.signal });
      } catch (_) { return; }
      if (generation !== this.generation) return;
      const found = body && typeof body.avatars === "object" && body.avatars ? body.avatars : {};
      const replaced = [];
      for (const key of keys) {
        const a = found[key];
        const url = a && a.urls && (a.urls["128"] || a.urls["256"] || a.urls.jpeg);
        if (!url || typeof url !== "string" || !url.startsWith("/")) { this.meta.set(key, null); this.repaint(key); continue; }
        const hash = String(a.hash || (a.id + ":" + a.version));
        this.meta.set(key, { hash, url });
        this.blob(hash, url).then((obj) => { if (obj && generation === this.generation) this.repaint(key); });
      }
      this.prune(replaced);
    },
    // The bytes for one picture, once per hash however many mailboxes share it.
    blob(hash, url) {
      if (this.urls.has(hash)) return Promise.resolve(this.urls.get(hash));
      if (!this.loading.has(hash)) {
        const generation = this.generation;
        const job = (async () => {
          const res = await fetch(cfg.postbox + url, {
            headers: { authorization: `Bearer ${getToken()}` }, signal: this.controller.signal,
          });
          if (!res.ok || typeof res.blob !== "function") return null;
          const data = await res.blob();
          if (generation !== this.generation || typeof URL.createObjectURL !== "function") return null;
          const obj = URL.createObjectURL(data);
          this.urls.set(hash, obj);
          return obj;
        })().catch(() => null).finally(() => this.loading.delete(hash));
        this.loading.set(hash, job);
      }
      return this.loading.get(hash);
    },
    repaint(key) {
      const meta = this.meta.get(key);
      for (const el of document.querySelectorAll("[data-avatar-key]")) {
        if (el.dataset.avatarKey !== key) continue;
        if (meta && this.urls.has(meta.hash)) this.paint(el, this.urls.get(meta.hash));
        else this.clear(el);
      }
    },
    // A picture changed: look again, and let go of bytes nothing shows any more.
    forget(keys) {
      const old = [];
      for (const key of keys) {
        if (!key) continue;
        const meta = this.meta.get(key);
        if (meta) old.push(meta.hash);
        this.meta.delete(key);
        if ([...document.querySelectorAll("[data-avatar-key]")].some((el) => el.dataset.avatarKey === key)) this.want(key);
      }
      this.prune(old);
    },
    prune(hashes) {
      const live = new Set([...this.meta.values()].filter(Boolean).map((m) => m.hash));
      for (const hash of hashes) {
        if (live.has(hash) || !this.urls.has(hash)) continue;
        try { URL.revokeObjectURL(this.urls.get(hash)); } catch (_) { /* jsdom */ }
        this.urls.delete(hash);
      }
    },
  };

  let ownAvatar = null;
  let pendingVisibility = "known";
  const meKeys = () => (state.me ? [state.me.handle, state.me.address].filter(Boolean) : []);

  function syncSettingsRows() {
    $("settings-nav-profile").hidden = !has("avatars");
    $("settings-nav-privacy").hidden = !(has("call_admission") || has("message_filtering") || has("call_history") || has("receipts"));
  }

  function openProfilePage() {
    renderProfile();
    loadOwnAvatar();
  }

  async function loadOwnAvatar() {
    const context = mailboxContext();
    if (!context.address || !has("avatars")) return;
    $("profile-note").textContent = "Loading…";
    try {
      const body = await api(withIdentity("/v1/profile/avatar", context.address), { signal: context.signal });
      if (!context.current()) return;
      ownAvatar = body.avatar || null;
      if (ownAvatar?.visibility) pendingVisibility = ownAvatar.visibility;
      $("profile-note").textContent = ownAvatar ? "This picture belongs to the mailbox you are using now."
        : "No picture yet. People see your initials.";
    } catch (_) {
      if (context.current()) $("profile-note").textContent = "Could not load your picture. Try again later.";
    }
    if (context.current()) renderProfile();
  }

  function renderProfile() {
    const el = $("profile-avatar");
    const key = state.me ? state.me.handle || state.me.address : "";
    el.textContent = initials(key || "··");
    el.dataset.tone = String(tone(key || "-"));
    Avatars.clear(el);
    delete el.dataset.avatarKey;
    const url = ownAvatar?.urls && (ownAvatar.urls["256"] || ownAvatar.urls["128"]);
    if (url) {
      const hash = String(ownAvatar.hash || ownAvatar.id + ":" + ownAvatar.version);
      Avatars.blob(hash, url).then((obj) => { if (obj && ownAvatar && String(ownAvatar.hash || ownAvatar.id + ":" + ownAvatar.version) === hash) Avatars.paint(el, obj); });
    }
    $("profile-remove").hidden = !ownAvatar;
    for (const input of document.querySelectorAll('#profile-visibility input')) input.checked = input.value === pendingVisibility;
  }

  async function setVisibility(value) {
    const previous = pendingVisibility;
    pendingVisibility = value;
    if (!ownAvatar) return; // applied with the next upload
    const context = mailboxContext();
    try {
      const body = await api("/v1/profile/avatar", { method: "PATCH", body: { identity: context.address, visibility: value }, signal: context.signal });
      if (!context.current()) return;
      ownAvatar = body.avatar || ownAvatar;
      Avatars.forget(meKeys());
      toast("Picture visibility saved.");
    } catch (_) {
      if (!context.current()) return;
      pendingVisibility = previous;
      toast("Could not change who can see your picture.");
    }
    renderProfile();
  }

  async function removeAvatar() {
    const context = mailboxContext();
    $("profile-remove").disabled = true;
    try {
      await api(withIdentity("/v1/profile/avatar", context.address), { method: "DELETE", signal: context.signal });
      if (!context.current()) return;
      ownAvatar = null;
      Avatars.forget(meKeys());
      $("profile-note").textContent = "Picture removed. People see your initials.";
    } catch (_) {
      if (context.current()) toast("Could not remove your picture.");
    } finally {
      if (context.current()) { $("profile-remove").disabled = false; renderProfile(); }
    }
  }

  // ---- crop -----------------------------------------------------------------------------------

  const AVATAR_MAX_BYTES = 10 * 1024 * 1024;
  const AVATAR_MAX_SIDE = 4096;
  let crop = null;

  async function decodeImage(file) {
    if (typeof createImageBitmap === "function") return createImageBitmap(file);
    return new Promise((resolve, reject) => {
      if (typeof URL.createObjectURL !== "function") { reject(new Error("no decoder")); return; }
      const img = new Image(), url = URL.createObjectURL(file);
      img.onload = () => { resolve(img); };
      img.onerror = () => { URL.revokeObjectURL(url); reject(new Error("decode")); };
      img.src = url;
    });
  }

  async function startCrop(file) {
    if (!file) return;
    const heic = /hei[cf]/i.test(file.type) || /\.hei[cf]$/i.test(file.name || "");
    if (!heic && !/^image\/(jpeg|png|webp)$/i.test(file.type)) { toast("Choose a JPEG, PNG or WebP picture."); return; }
    let image;
    try { image = await decodeImage(file); } catch (_) {
      toast(heic ? "This browser cannot open HEIC photos. Save it as JPEG first, then choose it again."
        : "That picture could not be opened.");
      return;
    }
    const imgW = image.width || image.naturalWidth, imgH = image.height || image.naturalHeight;
    if (!imgW || !imgH) { toast("That picture could not be opened."); return; }
    const C = window.PigeonpostCrop;
    const view = $("crop-canvas").width;
    crop = { file, image, imgW, imgH, view, zoom: 1, heic, ...C.centred({ imgW, imgH, view, zoom: 1 }) };
    $("crop-zoom").value = "1";
    $("crop-error").hidden = true;
    $("crop-save").disabled = false;
    openSheet("crop-sheet");
    drawCrop();
    $("crop-canvas").focus();
  }

  function drawCrop() {
    const canvas = $("crop-canvas");
    const ctx = crop && canvas.getContext ? (() => { try { return canvas.getContext("2d"); } catch (_) { return null; } })() : null;
    if (!ctx) return;
    const s = window.PigeonpostCrop.baseScale(crop.imgW, crop.imgH, crop.view) * crop.zoom;
    ctx.clearRect(0, 0, crop.view, crop.view);
    ctx.drawImage(crop.image, crop.x, crop.y, crop.imgW * s, crop.imgH * s);
    // The round mask people will actually see, drawn as a shade outside the circle.
    ctx.save();
    ctx.fillStyle = "rgba(0,0,0,.45)";
    ctx.beginPath();
    ctx.rect(0, 0, crop.view, crop.view);
    ctx.arc(crop.view / 2, crop.view / 2, crop.view / 2, 0, Math.PI * 2, true);
    ctx.fill("evenodd");
    ctx.restore();
  }

  function moveCrop(dx, dy) {
    if (!crop) return;
    Object.assign(crop, window.PigeonpostCrop.clampOffset({ ...crop, x: crop.x + dx, y: crop.y + dy }));
    drawCrop();
  }

  function zoomCrop(to) {
    if (!crop) return;
    const next = Math.min(4, Math.max(1, to));
    Object.assign(crop, window.PigeonpostCrop.rezoom({ ...crop, from: crop.zoom, to: next }), { zoom: next });
    $("crop-zoom").value = String(next);
    drawCrop();
  }

  // What to upload: the original when the server can take it as is, otherwise a JPEG no larger
  // than 4096 px (HEIC, very large files), with the crop moved onto the smaller picture.
  async function uploadBody() {
    const C = window.PigeonpostCrop;
    let rect = C.cropRect(crop);
    const fits = !crop.heic && crop.file.size <= AVATAR_MAX_BYTES && Math.max(crop.imgW, crop.imgH) <= AVATAR_MAX_SIDE;
    if (fits) return { blob: crop.file, type: crop.file.type, rect };
    const factor = Math.min(1, AVATAR_MAX_SIDE / Math.max(crop.imgW, crop.imgH));
    const w = Math.max(1, Math.round(crop.imgW * factor)), h = Math.max(1, Math.round(crop.imgH * factor));
    const canvas = document.createElement("canvas");
    canvas.width = w; canvas.height = h;
    let ctx = null;
    try { ctx = canvas.getContext("2d"); } catch (_) { ctx = null; }
    if (!ctx || typeof canvas.toBlob !== "function") throw new Error("This browser cannot prepare that picture.");
    ctx.drawImage(crop.image, 0, 0, w, h);
    const blob = await new Promise((resolve) => canvas.toBlob(resolve, "image/jpeg", 0.9));
    if (!blob || blob.size > AVATAR_MAX_BYTES) throw new Error("That picture is too large.");
    rect = C.scaleRect(rect, factor, w, h);
    return { blob, type: "image/jpeg", rect };
  }

  function avatarError(status, code) {
    if (status === 429 || code === "avatar_rate_limited") return "You can change your picture five times a day. Try again tomorrow.";
    if (status === 413) return "That picture is too large. Choose one under 10 MB.";
    if (status === 415 || status === 422) return "That picture could not be used. Try a JPEG or PNG.";
    return "Could not save your picture. Please try again.";
  }

  async function saveCrop() {
    if (!crop || $("crop-save").disabled) return;
    const context = mailboxContext();
    $("crop-save").disabled = true;
    $("crop-error").hidden = true;
    const fail = (message) => { $("crop-error").textContent = message; $("crop-error").hidden = false; $("crop-save").disabled = false; };
    let upload;
    try { upload = await uploadBody(); } catch (e) { fail(e.message); return; }
    const { x, y, w, h } = upload.rect;
    const all = $("profile-all").checked;
    const path = withIdentity("/v1/profile/avatar", context.address)
      + "&crop=" + [x, y, w, h].join(",") + "&visibility=" + encodeURIComponent(pendingVisibility) + (all ? "&all=1" : "");
    const send = () => fetch(cfg.postbox + path, {
      method: "PUT",
      headers: { authorization: `Bearer ${getToken()}`, "content-type": upload.type || "application/octet-stream", accept: "application/json" },
      body: upload.blob, signal: context.signal,
    });
    try {
      let res = await send();
      if (res.status === 401 && (await renewSession()) && context.current()) res = await send();
      const body = await res.json().catch(() => ({}));
      if (!context.current()) return;
      if (!res.ok) { fail(avatarError(res.status, body.error)); return; }
      ownAvatar = body.avatar || (Array.isArray(body.avatars) ? body.avatars.find((a) => !a.address || a.address === context.address) || body.avatars[0] : null) || ownAvatar;
      const keys = all ? state.identities.flatMap((id) => [id.handle, id.address]) : meKeys();
      Avatars.forget(keys);
      crop = null;
      closeSheet("crop-sheet");
      $("profile-note").textContent = all ? "Saved for all your mailboxes." : "Saved.";
      renderProfile();
      renderIdentityMenu();
      renderMe();
    } catch (_) {
      if (context.current()) fail("Could not reach your inbox. Please try again.");
    }
  }

  // ---- privacy and filtering ------------------------------------------------------------------

  const POLICY_ROWS = [
    { field: "unknown_calls", flag: "call_admission", title: "Calls from people you don’t know",
      note: "Silence keeps the call out of your way and lists it under Filtered calls.",
      options: [["ring", "Ring"], ["silence", "Silence"], ["block", "Block"]] },
    { field: "unnamed_messages", flag: "message_filtering", title: "Messages from addresses without a name",
      note: "Addresses like /k/… that have no handle.",
      options: [["show", "Show"], ["filter", "Move to Filtered"], ["block", "Block"]] },
    { field: "unknown_messages", flag: "message_filtering", title: "Messages from people you don’t know",
      note: "People you know are your contacts and anyone you have written to first.",
      options: [["show", "Show"], ["filter", "Move to Filtered"], ["block", "Block"]] },
    { field: "unnamed_calls", flag: "call_admission", title: "Calls from addresses without a name",
      options: [["ring", "Ring"], ["silence", "Silence"], ["block", "Block"]] },
    { field: "low_reputation", flag: "message_filtering", title: "Low-reputation senders",
      note: "Senders others have reported as spam.",
      options: [["filter", "Filter"], ["allow", "Allow"]] },
    { field: "call_history_days", flag: "call_history", title: "Keep call history",
      note: "Off stops keeping new calls. Entries older than this are deleted.",
      options: [["0", "Off"], ["30", "30 days"], ["180", "180 days"], ["365", "1 year"]] },
    // Phase 1e. Delivered receipts are always on; this is only about "seen".
    { field: "read_receipts", flag: "receipts", title: "Send read receipts",
      note: "When off, people don’t see when you have read their messages, and you don’t see when they have read yours. Delivered ticks still show.",
      options: [["true", "On"], ["false", "Off"]] },
  ];
  let policyFull = null;

  function renderPolicy() {
    const list = $("policy-list");
    list.textContent = "";
    for (const row of POLICY_ROWS) {
      if (!has(row.flag)) continue;
      const box = document.createElement("div");
      box.className = "policy-row";
      const title = document.createElement("h4");
      title.id = "policy-title-" + row.field;
      title.textContent = row.title;
      const group = document.createElement("div");
      group.className = "segmented";
      group.setAttribute("role", "radiogroup");
      group.setAttribute("aria-labelledby", title.id);
      group.dataset.policy = row.field;
      for (const [value, label] of row.options) {
        const l = document.createElement("label"), input = document.createElement("input"), span = document.createElement("span");
        input.type = "radio";
        input.name = "policy-" + row.field;
        input.value = value;
        input.disabled = !policyFull;
        input.checked = Boolean(policyFull) && String(row.field === "read_receipts" ? policyFull.read_receipts !== false : policyFull[row.field]) === value;
        input.addEventListener("change", () => { if (input.checked) savePolicy(row.field, value); });
        span.textContent = label;
        l.append(input, span);
        group.append(l);
      }
      box.append(title, group);
      if (row.note) {
        const note = document.createElement("p");
        note.className = "field-note";
        note.textContent = row.note;
        box.append(note);
      }
      list.append(box);
    }
  }

  async function openPrivacyPage() {
    const context = mailboxContext();
    policyFull = null;
    $("policy-error").hidden = true;
    renderPolicy();
    try {
      const body = await api(withIdentity("/v1/policy", context.address), { signal: context.signal });
      if (!context.current()) return;
      policyFull = body;
    } catch (_) {
      if (!context.current()) return;
      $("policy-error").textContent = "Could not load these settings. Close and try again.";
      $("policy-error").hidden = false;
    }
    renderPolicy();
  }

  async function savePolicy(field, raw) {
    if (!policyFull) return;
    const context = mailboxContext();
    const value = field === "call_history_days" ? Number(raw) : field === "read_receipts" ? raw === "true" : raw;
    const previous = policyFull[field];
    policyFull = { ...policyFull, [field]: value };
    $("policy-error").hidden = true;
    try {
      const body = await api("/v1/policy", { method: "PUT", body: { [field]: value, identity: context.address }, signal: context.signal });
      if (!context.current()) return;
      if (body && typeof body === "object" && field in body) policyFull = { ...policyFull, ...body };
      toast("Saved.");
    } catch (_) {
      if (!context.current()) return;
      policyFull = { ...policyFull, [field]: previous };
      $("policy-error").textContent = "Could not save that setting. Please try again.";
      $("policy-error").hidden = false;
    }
    renderPolicy();
  }

  // ---- blocking ---------------------------------------------------------------------------------

  let blockTarget = null;

  // The rule covering a whole namespace, or null where nobody owns the namespace (a provider or an
  // open one) and a wildcard would block strangers who merely share it.
  function wildcardFor(peer) {
    if (!peer || peer.startsWith("/k/") || peer.endsWith("/*")) return null;
    const parts = peer.split("/").filter(Boolean);
    if (!parts.length) return null;
    const parent = parts.length === 1 ? parts[0] : parts.slice(0, -1).join("/");
    if (["github", "pp", "k"].includes(parent)) return null;
    return "/" + parent + "/*";
  }

  function askBlock(target) {
    blockTarget = target;
    blockingPeer = target.peer;
    const name = target.name || displayName(target.peer);
    $("peer-block-title").textContent = "Block " + name + "?";
    $("peer-block-report").checked = Boolean(target.report);
    const wide = wildcardFor(target.peer);
    $("peer-block-wide-row").hidden = !wide;
    $("peer-block-wide").checked = false;
    if (wide) $("peer-block-wide-label").textContent = "Block everyone at " + wide.slice(0, -2);
    $("peer-block-error").hidden = true;
    $("peer-block-confirm").disabled = false;
    $("peer-block-confirm").textContent = "Block";
    openSheet("peer-block-sheet");
    $("peer-block-cancel").focus();
  }

  async function confirmBlock() {
    const target = blockTarget;
    if (!target || $("peer-block-confirm").disabled) return;
    const wide = !$("peer-block-wide-row").hidden && $("peer-block-wide").checked;
    const report = $("peer-block-report").checked;
    const peer = wide ? wildcardFor(target.peer) : target.peer;
    if (target.origin === "info" && !wide) {
      await updateSender(peer, "block");
      if (state.senderAction?.error) return;
    } else {
      const error = await blockAddress(peer);
      if (error) {
        $("peer-block-error").textContent = error;
        $("peer-block-error").hidden = false;
        return;
      }
      if (!$("peer-block-sheet").hidden) closeSheet("peer-block-sheet");
    }
    if (report) await reportFor(target);
  }

  // Write a block row. Returns an error message, or null when it was saved.
  async function blockAddress(peer) {
    const context = mailboxContext();
    const existing = state.contacts.find((c) => c.peer === peer);
    const row = { peer, alias: existing?.alias || null, admission: "block", autonomy: "review", allowed_verbs: [] };
    $("peer-block-confirm").disabled = true;
    $("peer-block-confirm").textContent = "Blocking…";
    try {
      await api("/v1/contacts", { method: "PUT", body: { ...row, identity: context.address }, signal: context.signal });
      if (!context.current()) return null;
      state.sectionRequests.contacts = (state.sectionRequests.contacts || 0) + 1;
      state.contacts = state.contacts.filter((c) => c.peer !== peer).concat(row);
      toast("Blocked. They won’t be able to message or call you.");
      render();
      return null;
    } catch (e) {
      return "Could not block. " + (e?.message || "Please try again.");
    } finally {
      $("peer-block-confirm").disabled = false;
      $("peer-block-confirm").textContent = "Block";
    }
  }

  async function reportFor(target) {
    const context = mailboxContext();
    let body = null;
    if (target.callId) body = { call_id: target.callId };
    else {
      const id = target.messageId || [...state.inbound, ...state.filtered].reverse()
        .find((m) => m.direction !== "out" && peerKeyOf(m) === target.peer)?.message_id;
      if (id) body = { message_id: id };
    }
    if (!body) return;
    try {
      await api("/v1/report-spam", { method: "POST", body: { ...body, identity: context.address }, signal: context.signal });
      if (context.current()) toast("Blocked and reported as spam.");
    } catch (_) {
      if (context.current()) toast("Blocked, but the spam report could not be sent.");
    }
  }

  function blockSenderButton(m) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "msg-block";
    button.title = "Block sender";
    button.setAttribute("aria-label", "Block sender");
    button.textContent = "⊘";
    button.addEventListener("click", () => {
      if (!state.openPeer) return;
      askBlock({ peer: state.openPeer, name: $("peer-name").textContent, origin: "message", messageId: m.id });
    });
    return button;
  }

  function renderBlockedBanner(conversation) {
    const blocked = contactFor(conversation.peer)?.admission === "block";
    $("blocked-banner").hidden = !blocked;
    if (blocked) $("composer").hidden = true;
  }

  let blockedRows = null;
  async function openBlockedPage() {
    const context = mailboxContext();
    blockedRows = null;
    $("blocked-error").hidden = true;
    renderBlockedList();
    try {
      const body = await api(withIdentity("/v1/contacts?admission=block", context.address), { signal: context.signal });
      if (!context.current()) return;
      blockedRows = (body.contacts || []).filter((c) => c.admission === "block");
    } catch (_) {
      if (!context.current()) return;
      blockedRows = state.contacts.filter((c) => c.admission === "block");
      $("blocked-error").textContent = "Could not refresh the list. Showing what was last loaded.";
      $("blocked-error").hidden = false;
    }
    renderBlockedList();
  }

  function renderBlockedList() {
    const list = $("blocked-list");
    list.textContent = "";
    if (!blockedRows) {
      const li = document.createElement("li");
      li.className = "cl-empty";
      li.textContent = "Loading…";
      list.append(li);
      return;
    }
    if (!blockedRows.length) {
      const li = document.createElement("li");
      li.className = "cl-empty";
      li.textContent = "Nobody is blocked.";
      list.append(li);
      return;
    }
    for (const c of blockedRows) {
      const li = document.createElement("li");
      const who = document.createElement("div");
      who.className = "cl-who";
      const peer = document.createElement("span");
      peer.className = "cl-peer";
      peer.textContent = c.peer;
      const terms = document.createElement("span");
      terms.className = "cl-terms";
      terms.textContent = c.peer.endsWith("/*") ? "Everyone at " + c.peer.slice(0, -2) : (c.alias || "Blocked");
      who.append(peer, terms);
      const unblock = document.createElement("button");
      unblock.type = "button";
      unblock.className = "btn-ghost";
      unblock.textContent = "Unblock";
      unblock.setAttribute("aria-label", "Unblock " + c.peer);
      unblock.onclick = () => unblockAddress(c.peer, unblock);
      li.append(who, unblock);
      list.append(li);
    }
  }

  async function unblockAddress(peer, button) {
    const context = mailboxContext();
    if (button) button.disabled = true;
    try {
      await api(withIdentity("/v1/contacts?peer=" + encodeURIComponent(peer), context.address), {
        method: "DELETE", body: { peer, identity: context.address }, signal: context.signal,
      });
      if (!context.current()) return;
      state.sectionRequests.contacts = (state.sectionRequests.contacts || 0) + 1;
      state.contacts = state.contacts.filter((c) => c.peer !== peer);
      if (blockedRows) blockedRows = blockedRows.filter((c) => c.peer !== peer);
      toast("Unblocked " + peer + ".");
      renderBlockedList();
      renderContactList();
      render();
    } catch (_) {
      if (!context.current()) return;
      if (button) button.disabled = false;
      $("blocked-error").textContent = "Could not unblock " + peer + ". Please try again.";
      $("blocked-error").hidden = false;
    }
  }

  async function blockFromSettings() {
    normaliseAddressInput($("block-input"));
    const peer = $("block-input").value.trim();
    $("blocked-error").hidden = true;
    if (!/^\/(?:k\/[a-z0-9]+|[^\s/*]+(?:\/[^\s/*]+)*(?:\/\*)?)$/i.test(peer)) {
      $("blocked-error").textContent = "Enter an address such as /k/…, /spamco/agent or /spamco/*.";
      $("blocked-error").hidden = false;
      return;
    }
    if (peer === state.me?.address || peer === state.me?.handle) {
      $("blocked-error").textContent = "You cannot block this mailbox itself.";
      $("blocked-error").hidden = false;
      return;
    }
    $("block-add").disabled = true;
    const error = await blockAddress(peer);
    $("block-add").disabled = false;
    if (error) {
      $("blocked-error").textContent = error;
      $("blocked-error").hidden = false;
      return;
    }
    $("block-input").value = "";
    if (blockedRows) blockedRows = blockedRows.filter((c) => c.peer !== peer).concat({ peer, admission: "block" });
    renderBlockedList();
  }

  // ---- side views: calls and filtered ---------------------------------------------------------

  function syncSideNav() {
    const on = (has("call_history") || has("calendar") || has("meeting_summaries")) && Boolean(state.me); // Phase 3B: or calendar // Phase 7: or meeting notes
    $("side-nav").hidden = !on;
    $("calls-btn").hidden = !has("call_history"); // Phase 3B
    window.PigeonpostCalendar?.syncNav(); // Phase 3B
    const unseen = on ? state.calls.unseen : 0;
    $("calls-badge").hidden = !unseen;
    $("calls-badge").textContent = unseen > 99 ? "99+" : String(unseen);
    $("calls-btn").setAttribute("aria-pressed", String(state.leftView === "calls"));
    $("calls-btn").setAttribute("aria-label", unseen ? `Calls, ${unseen} missed` : "Calls");
  }

  // Returns true when a side view, not the conversation list, owns the left pane.
  function renderSideViews() {
    if (state.leftView === "calls" && !has("call_history")) state.leftView = "inbox";
    if (state.leftView === "filtered" && !has("message_filtering")) state.leftView = "inbox";
    if (state.leftView === "calendar" && !has("calendar")) state.leftView = "inbox"; // Phase 3B
    syncSideNav();
    const view = state.leftView, inbox = view === "inbox";
    $("threads").hidden = !inbox;
    $("calls-view").hidden = view !== "calls";
    $("filtered-view").hidden = view !== "filtered";
    const search = $("search").closest(".search");
    if (search) search.hidden = !inbox;
    const groups = has("message_filtering") ? filteredGroups() : [];
    $("filtered-entry").hidden = !(inbox && !state.viewingArchive && groups.length);
    $("filtered-count").textContent = groups.length ? " (" + groups.length + ")" : "";
    $("filtered-entry").setAttribute("aria-label", "Filtered, " + groups.length + (groups.length === 1 ? " conversation" : " conversations"));
    if (inbox) return false;
    $("threads-empty").hidden = true;
    $("archive-banner").hidden = true;
    if (view === "calls") renderCallLog();
    if (view === "filtered") renderFilteredList(groups);
    return true;
  }

  function setLeftView(view) {
    state.leftView = view;
    if (view !== "inbox" && state.viewingArchive) showArchive(false);
    render();
  }

  // ---- call history -----------------------------------------------------------------------------

  async function loadCallLog({ more = false } = {}) {
    if (!has("call_history")) return;
    const context = mailboxContext();
    if (!context.address) return;
    const calls = context.owner.calls, filter = calls.filter;
    let path = withIdentity("/v1/calls/log?filter=" + filter + "&limit=50", context.address);
    if (more && calls.next) path += "&before=" + encodeURIComponent(calls.next);
    calls.loading = true;
    try {
      const body = await api(path, { signal: context.signal });
      if (!context.current() || calls.filter !== filter) return;
      const entries = Array.isArray(body.entries) ? body.entries.filter((e) => e && e.id) : [];
      if (more) {
        const seen = new Set(calls.entries.map((e) => e.id));
        calls.entries = calls.entries.concat(entries.filter((e) => !seen.has(e.id)));
      } else calls.entries = entries;
      calls.next = body.next_before ?? null;
      calls.unseen = Math.max(0, Number(body.unseen_missed) || 0);
      calls.loaded = true;
      calls.error = null;
    } catch (_) {
      if (context.current()) calls.error = "Could not load your calls.";
    } finally {
      if (context.current()) {
        calls.loading = false;
        syncSideNav();
        if (state.leftView === "calls") renderCallLog();
      }
    }
  }

  async function loadFilteredCalls() {
    const context = mailboxContext();
    const calls = context.owner.calls;
    try {
      const body = await api(withIdentity("/v1/calls/log?filter=filtered&limit=50", context.address), { signal: context.signal });
      if (!context.current()) return;
      calls.filtered = Array.isArray(body.entries) ? body.entries : [];
    } catch (_) {
      if (context.current()) calls.filtered = [];
    }
    if (context.current() && state.leftView === "calls") renderCallLog();
  }

  async function showCalls() {
    setLeftView("calls");
    const calls = state.calls, context = mailboxContext();
    if (!calls.loaded) await loadCallLog();
    if (context.current() && state.leftView === "calls") markCallsSeen();
  }

  // Looking at the page is seeing the missed calls on it.
  async function markCallsSeen() {
    const calls = state.calls;
    const newest = calls.entries.reduce((max, e) => Math.max(max, Number(e.started_at) || 0), 0);
    if (!calls.unseen || !newest) return;
    const context = mailboxContext();
    calls.unseen = 0;
    syncSideNav();
    try {
      const body = await api("/v1/calls/log/seen", { method: "POST", body: { up_to: newest, identity: context.address }, signal: context.signal });
      if (!context.current()) return;
      calls.unseen = Math.max(0, Number(body.unseen_missed) || 0);
      for (const e of calls.entries) if (e.started_at <= newest) e.seen = true;
      syncSideNav();
    } catch (_) { /* the badge returns with the next load */ }
  }

  const callPeerKey = (e) => e.peer_handle || e.peer || null;
  const kindWord = (kind) => (kind === "video" ? "video" : "voice");

  function callOutcomeLabel(e) {
    const incoming = e.direction === "incoming";
    switch (e.outcome) {
      case "answered": return incoming ? "Incoming" : "Outgoing";
      case "missed": return incoming ? "Missed" : "No answer";
      case "declined": return "Declined";
      case "cancelled": return incoming ? "Missed" : "Cancelled";
      case "busy": return "Busy";
      case "filtered": return "Filtered";
      case "failed": return "Failed";
      default: return "In progress";
    }
  }

  function formatDuration(seconds) {
    const total = Math.max(0, Math.round(Number(seconds) || 0));
    const h = Math.floor(total / 3600), m = Math.floor((total % 3600) / 60), s = total % 60;
    const two = (n) => String(n).padStart(2, "0");
    return h ? `${h}:${two(m)}:${two(s)}` : `${m}:${two(s)}`;
  }

  const CALL_ICON = {
    audio: '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M6.6 2.9 4.5 3.3c-.8.2-1.4.9-1.3 1.7.5 6.1 5.7 11.3 11.8 11.8.8.1 1.5-.5 1.7-1.3l.4-2.1c.1-.6-.2-1.2-.8-1.4l-2.4-1c-.5-.2-1.1-.1-1.5.3l-.9.9a9 9 0 0 1-4-4l.9-.9c.4-.4.5-1 .3-1.5l-1-2.4c-.2-.6-.8-.9-1.4-.8Z" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/></svg>',
    video: '<svg viewBox="0 0 20 20" aria-hidden="true"><rect x="2.5" y="5" width="11" height="10" rx="2" fill="none" stroke="currentColor" stroke-width="1.5"/><path d="m13.5 9 4-2.3v6.6l-4-2.3" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/></svg>',
  };
  const ARROW = { incoming: "↙", outgoing: "↗" };

  function callName(e) {
    const key = callPeerKey(e);
    if (key) return contactFor(key)?.alias || displayName(key);
    const people = (e.participants || []).length;
    return people ? people + " people" : "Call";
  }

  function callRow(e) {
    const li = document.createElement("li");
    li.className = "call-item";
    li.dataset.callId = e.id;
    const key = callPeerKey(e);
    const missed = e.direction === "incoming" && (e.outcome === "missed" || e.outcome === "cancelled");
    if (missed) li.classList.add("call-missed");

    const main = document.createElement("button");
    main.type = "button";
    main.className = "call-main";
    const av = document.createElement("span");
    av.className = "avatar";
    paintAvatar(av, key || "/guest");
    const text = document.createElement("span");
    text.className = "call-text";
    const name = document.createElement("span");
    name.className = "call-name";
    name.textContent = callName(e);
    const sub = document.createElement("span");
    sub.className = "call-sub";
    const label = callOutcomeLabel(e);
    const bits = [label, e.kind === "video" ? "Video" : "Voice"];
    const duration = e.duration ?? (e.answered_at && e.ended_at ? e.ended_at - e.answered_at : null);
    if (e.outcome === "answered" && duration != null) bits.push(formatDuration(duration));
    const people = (e.participants || []).length;
    if (e.group_id || people > 2) bits.push(people + " people");
    sub.textContent = (ARROW[e.direction] || "") + " " + bits.join(" · ");
    text.append(name, sub);
    const time = document.createElement("span");
    time.className = "call-time";
    time.textContent = e.started_at ? listTime(e.started_at) : "";
    main.append(av, text, time);
    main.setAttribute("aria-label", `${callName(e)}, ${label.toLowerCase()} ${kindWord(e.kind)} call` + (e.started_at ? ", " + listTime(e.started_at) : ""));
    main.onclick = () => { if (key) openThread(key); };
    main.disabled = !key;

    const back = document.createElement("button");
    back.type = "button";
    back.className = "icon-btn call-back";
    back.innerHTML = CALL_ICON[e.kind === "video" ? "video" : "audio"];
    back.title = `Call back (${kindWord(e.kind)})`;
    back.setAttribute("aria-label", `Call ${callName(e)} back, ${kindWord(e.kind)}`);
    back.hidden = !key;
    back.onclick = () => callBack(key, e.kind);

    const more = document.createElement("button");
    more.type = "button";
    more.className = "icon-btn call-more";
    more.textContent = "⋯";
    more.setAttribute("aria-label", "More actions for this call");
    more.setAttribute("aria-expanded", "false");
    const menu = document.createElement("div");
    menu.className = "call-menu";
    menu.hidden = true;
    const action = (label, handler, danger) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = danger ? "btn-danger" : "btn-ghost";
      b.textContent = label;
      b.onclick = () => { menu.hidden = true; more.setAttribute("aria-expanded", "false"); handler(); };
      menu.append(b);
    };
    action("Delete from history", () => deleteCall(e));
    if (key) action("Block", () => askBlock({ peer: key, name: callName(e), origin: "call", callId: e.direction === "incoming" ? e.id : null }), true);
    if (e.direction === "incoming" && key) action("Report spam", () => reportCall(e));
    more.onclick = () => {
      const open = menu.hidden;
      for (const other of document.querySelectorAll(".call-menu")) other.hidden = true;
      for (const other of document.querySelectorAll(".call-more")) other.setAttribute("aria-expanded", "false");
      menu.hidden = !open;
      more.setAttribute("aria-expanded", String(open));
    };
    li.append(main, back, more, menu);
    return li;
  }

  function renderCallLog() {
    const calls = state.calls, list = $("call-log");
    for (const radio of document.querySelectorAll('#calls-filter input')) radio.checked = radio.value === calls.filter;
    list.textContent = "";
    const entries = calls.entries.filter((e) => e.outcome !== "filtered");
    for (const e of entries) list.append(callRow(e));
    const status = $("calls-status");
    status.hidden = !(calls.error || (calls.loading && !calls.loaded));
    status.textContent = calls.error || "Loading calls…";
    $("calls-empty").hidden = entries.length > 0 || !calls.loaded || Boolean(calls.error);
    $("calls-empty").textContent = calls.filter === "missed" ? "No missed calls." : "No calls yet.";
    $("calls-more").hidden = !calls.next;
    $("calls-clear").disabled = !calls.entries.length;
    const filteredList = $("call-log-filtered");
    filteredList.textContent = "";
    for (const e of calls.filtered || []) filteredList.append(callRow(e));
    $("calls-filtered-empty").hidden = !calls.filtered || calls.filtered.length > 0;
    $("calls-filtered-count").textContent = calls.filtered && calls.filtered.length ? "(" + calls.filtered.length + ")" : "";
  }

  async function callBack(key, kind) {
    if (!key) return;
    await openThread(key);
    const button = $(kind === "video" ? "call-video-btn" : "call-audio-btn");
    if (button && !button.hidden && !button.disabled) button.click();
    else toast("Calling isn’t available right now.");
  }

  async function deleteCall(e) {
    const context = mailboxContext();
    try {
      await api(withIdentity("/v1/calls/log/" + encodeURIComponent(e.id), context.address), { method: "DELETE", signal: context.signal });
      if (!context.current()) return;
      const calls = state.calls;
      calls.entries = calls.entries.filter((x) => x.id !== e.id);
      if (calls.filtered) calls.filtered = calls.filtered.filter((x) => x.id !== e.id);
      toast("Removed from call history.");
      render();
    } catch (_) {
      if (context.current()) toast("Could not remove that call.");
    }
  }

  async function reportCall(e) {
    const context = mailboxContext();
    try {
      await api("/v1/report-spam", { method: "POST", body: { call_id: e.id, identity: context.address }, signal: context.signal });
      if (context.current()) toast("Reported as spam.");
    } catch (_) {
      if (context.current()) toast("Could not send the report.");
    }
  }

  let clearArmed = null;
  async function clearCalls() {
    const button = $("calls-clear");
    if (!clearArmed) {
      button.textContent = "Clear all calls?";
      clearArmed = setTimeout(() => { clearArmed = null; button.textContent = "Clear all"; }, 4000);
      return;
    }
    clearTimeout(clearArmed);
    clearArmed = null;
    button.textContent = "Clear all";
    const context = mailboxContext();
    try {
      await api(withIdentity("/v1/calls/log", context.address), { method: "DELETE", signal: context.signal });
      if (!context.current()) return;
      Object.assign(state.calls, { entries: [], filtered: state.calls.filtered ? [] : null, next: null, unseen: 0 });
      toast("Call history cleared.");
      render();
    } catch (_) {
      if (context.current()) toast("Could not clear your call history.");
    }
  }

  // A missed call reads as a small line in the conversation it belongs to.
  function withMissedCalls(messages, conversation) {
    if (!has("call_history") || !state.calls.entries.length) return messages;
    const keys = new Set([conversation.peer]);
    for (const m of conversation.messages) if (m.kind === "in" && m.address) keys.add(m.address);
    const lines = state.calls.entries
      .filter((e) => e.direction === "incoming" && e.outcome === "missed" && !e.group_id
        && (keys.has(e.peer_handle) || keys.has(e.peer)))
      .map((e) => ({ kind: "system", id: "call:" + e.id, at: e.started_at,
        body: `Missed ${kindWord(e.kind)} call · ${clockTime(e.started_at)}` }));
    if (!lines.length) return messages;
    return [...messages, ...lines].sort((a, b) => a.at - b.at);
  }

  function systemLine(m) {
    const li = document.createElement("li");
    li.className = "system-line";
    li.dataset.messageId = m.id;
    const span = document.createElement("span");
    span.textContent = m.body;
    li.append(span);
    return li;
  }

  // ---- filtered messages ------------------------------------------------------------------------

  async function loadFiltered() {
    if (!has("message_filtering")) return;
    const context = mailboxContext();
    if (!context.address) return;
    try {
      const body = await api(withIdentity("/v1/inbox", context.address) + "&filtered=1&include_read=true", { signal: context.signal });
      if (!context.current()) return;
      state.filtered = (body.messages || []).filter((m) => m.direction !== "out");
    } catch (e) {
      if (context.current() && answered(e) && [400, 404, 501].includes(e.status)) state.filtered = [];
    }
  }

  function filteredGroups() {
    const groups = new Map();
    for (const m of state.filtered) {
      const key = peerKeyOf(m);
      if (!key || contactFor(key)?.admission === "block") continue;
      if (!groups.has(key)) groups.set(key, { peer: key, messages: [], last: 0 });
      const group = groups.get(key);
      group.messages.push(m);
      group.last = Math.max(group.last, Number(m.received_at) || 0);
    }
    for (const group of groups.values()) group.messages.sort((a, b) => a.received_at - b.received_at);
    return [...groups.values()].sort((a, b) => b.last - a.last);
  }

  function renderFilteredList(groups) {
    const list = $("filtered-list");
    list.textContent = "";
    $("filtered-empty").hidden = groups.length > 0;
    for (const group of groups) {
      const li = document.createElement("li");
      const row = document.createElement("button");
      row.type = "button";
      row.className = "thread-row";
      const av = document.createElement("span");
      av.className = "avatar";
      paintAvatar(av, group.peer);
      const text = document.createElement("div");
      text.className = "tr-text";
      const top = document.createElement("div");
      top.className = "tr-top";
      const name = document.createElement("span");
      name.className = "tr-name";
      name.textContent = displayName(group.peer);
      const time = document.createElement("span");
      time.className = "tr-time";
      time.textContent = group.last ? listTime(group.last) : "";
      top.append(name, time);
      const bottom = document.createElement("div");
      bottom.className = "tr-bottom";
      const preview = document.createElement("span");
      preview.className = "tr-preview";
      preview.textContent = previewOf(group.messages[group.messages.length - 1]);
      const badges = document.createElement("span");
      badges.className = "tr-badges";
      badges.append(pill(String(group.messages.length), "pill-held"));
      bottom.append(preview, badges);
      text.append(top, bottom);
      row.append(av, text);
      row.onclick = () => openFiltered(group.peer);
      li.append(row);
      list.append(li);
    }
  }

  let filteredOpen = null;
  function openFiltered(peer) {
    const group = filteredGroups().find((g) => g.peer === peer);
    if (!group) return;
    filteredOpen = peer;
    const last = group.messages[group.messages.length - 1];
    $("filtered-title").textContent = displayName(peer);
    $("filtered-sub").textContent = "From " + (last.from || peer)
      + (last.sender_standing ? " · " + last.sender_standing : "")
      + ". Accepting adds them as a contact and moves these messages to your inbox.";
    $("filtered-error").hidden = true;
    $("filtered-accept").disabled = false;
    const list = $("filtered-messages");
    list.textContent = "";
    for (const m of group.messages) {
      const li = document.createElement("li");
      li.className = "filtered-message";
      const meta = document.createElement("div");
      meta.className = "filtered-meta";
      const time = document.createElement("span");
      time.textContent = dayLabel(m.received_at) + " · " + clockTime(m.received_at);
      const report = document.createElement("button");
      report.type = "button";
      report.className = "linkish";
      report.textContent = "Report spam";
      report.setAttribute("aria-label", "Report this message as spam");
      report.onclick = () => reportFilteredMessage(m, report);
      meta.append(time, report);
      const body = document.createElement("p");
      body.className = "filtered-body";
      body.textContent = plainText(copyTextOf(m));
      li.append(meta, body);
      list.append(li);
    }
    openSheet("filtered-sheet");
    $("filtered-close").focus();
  }

  async function reportFilteredMessage(m, button) {
    const context = mailboxContext();
    button.disabled = true;
    try {
      await api("/v1/report-spam", { method: "POST", body: { message_id: m.message_id, identity: context.address }, signal: context.signal });
      if (!context.current()) return;
      button.textContent = "Reported";
    } catch (_) {
      if (!context.current()) return;
      button.disabled = false;
      toast("Could not send the report.");
    }
  }

  async function acceptFiltered() {
    const peer = filteredOpen;
    const group = filteredGroups().find((g) => g.peer === peer);
    if (!group || $("filtered-accept").disabled) return;
    const context = mailboxContext();
    const from = group.messages[group.messages.length - 1].from || peer;
    $("filtered-accept").disabled = true;
    try {
      await api("/v1/filtered/accept", { method: "POST", body: { peer: from, identity: context.address }, signal: context.signal });
      if (!context.current()) return;
      state.filtered = state.filtered.filter((m) => peerKeyOf(m) !== peer);
      closeSheet("filtered-sheet");
      await Promise.allSettled([loadInbox(), loadContacts(), loadFiltered()]);
      if (!context.current()) return;
      state.leftView = "inbox";
      render();
      toast("Accepted. Their messages are in your inbox.");
      const opened = buildThreads().find((t) => t.peer === peer);
      if (opened) openThread(opened.peer);
    } catch (e) {
      if (!context.current()) return;
      $("filtered-accept").disabled = false;
      $("filtered-error").textContent = "Could not accept. " + (e?.message || "Please try again.");
      $("filtered-error").hidden = false;
    }
  }

  function blockFiltered() {
    const group = filteredGroups().find((g) => g.peer === filteredOpen);
    if (!group) return;
    closeSheet("filtered-sheet");
    const last = group.messages[group.messages.length - 1];
    askBlock({ peer: group.peer, name: displayName(group.peer), origin: "filtered", messageId: last.message_id, report: true });
  }

  function wirePhase1b() {
    $("calls-btn").onclick = () => (state.leftView === "calls" ? setLeftView("inbox") : showCalls());
    $("calls-exit").onclick = () => setLeftView("inbox");
    $("filtered-exit").onclick = () => setLeftView("inbox");
    $("filtered-entry").onclick = () => setLeftView("filtered");
    for (const input of document.querySelectorAll('#calls-filter input')) {
      input.addEventListener("change", () => {
        if (!input.checked) return;
        Object.assign(state.calls, { filter: input.value, entries: [], next: null, loaded: false });
        renderCallLog();
        loadCallLog();
      });
    }
    $("calls-more").onclick = () => loadCallLog({ more: true });
    $("calls-clear").onclick = () => clearCalls();
    $("calls-filtered-box").addEventListener("toggle", () => {
      if ($("calls-filtered-box").open && state.calls.filtered === null) loadFilteredCalls();
    });
    document.addEventListener("click", (e) => {
      if (e.target.closest(".call-more") || e.target.closest(".call-menu")) return;
      for (const menu of document.querySelectorAll(".call-menu")) menu.hidden = true;
    });

    $("filtered-close").onclick = () => closeSheet("filtered-sheet");
    $("filtered-accept").onclick = () => acceptFiltered();
    $("filtered-block").onclick = () => blockFiltered();
    wireSheet("filtered-sheet");

    $("blocked-unblock").onclick = () => { if (state.openPeer) updateSender(state.openPeer, "known"); };
    $("block-add").onclick = () => blockFromSettings();
    $("block-input").addEventListener("input", (e) => { if (!e.isComposing) normaliseAddressInput(e.target); });
    $("block-input").addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); blockFromSettings(); } });

    $("profile-pick").onclick = () => $("profile-file").click();
    $("profile-file").addEventListener("change", () => {
      const file = $("profile-file").files && $("profile-file").files[0];
      $("profile-file").value = "";
      startCrop(file);
    });
    $("profile-remove").onclick = () => removeAvatar();
    for (const input of document.querySelectorAll('#profile-visibility input')) {
      input.addEventListener("change", () => { if (input.checked) setVisibility(input.value); });
    }
    $("crop-cancel").onclick = () => { crop = null; closeSheet("crop-sheet"); };
    $("crop-save").onclick = () => saveCrop();
    $("crop-zoom").addEventListener("input", () => zoomCrop(Number($("crop-zoom").value)));
    wireSheet("crop-sheet", () => {});
    const canvas = $("crop-canvas");
    let drag = null;
    const ratio = () => (canvas.clientWidth ? canvas.width / canvas.clientWidth : 1);
    canvas.addEventListener("pointerdown", (e) => { drag = { x: e.clientX, y: e.clientY }; canvas.setPointerCapture?.(e.pointerId); });
    canvas.addEventListener("pointermove", (e) => {
      if (!drag) return;
      const r = ratio();
      moveCrop((e.clientX - drag.x) * r, (e.clientY - drag.y) * r);
      drag = { x: e.clientX, y: e.clientY };
    });
    for (const type of ["pointerup", "pointercancel"]) canvas.addEventListener(type, () => { drag = null; });
    canvas.addEventListener("wheel", (e) => { if (!crop) return; e.preventDefault(); zoomCrop(crop.zoom * (e.deltaY < 0 ? 1.08 : 1 / 1.08)); }, { passive: false });
    canvas.addEventListener("keydown", (e) => {
      const step = { ArrowLeft: [10, 0], ArrowRight: [-10, 0], ArrowUp: [0, 10], ArrowDown: [0, -10] }[e.key];
      if (step) { e.preventDefault(); moveCrop(step[0], step[1]); }
      if (e.key === "+" || e.key === "=") { e.preventDefault(); zoomCrop((crop?.zoom || 1) + 0.1); }
      if (e.key === "-") { e.preventDefault(); zoomCrop((crop?.zoom || 1) - 0.1); }
    });
  }

  // ---- boot ---------------------------------------------------------------------------------

  async function openInbox(createIfMissing = false) {
    if (!getToken() || state.openingInbox) return;
    state.openingInbox = true;
    const create = $("create-inbox-btn");
    const note = $("signin-note");
    create.disabled = true;
    note.textContent = createIfMissing ? "Creating your inbox…" : "Loading your inbox…";
    render();

    try {
      // A previous mint may have succeeded even when its response (or the following listing)
      // was lost. Reconcile before another POST so a retry opens that mailbox instead of minting
      // a second one. This also picks up a mailbox created on another device in the meantime.
      await loadIdentities();
      if (!state.me && createIfMissing) {
        await api("/v1/identities", { method: "POST", body: {} });
        await loadIdentities();
        if (!state.me) throw new Error("Your inbox is not available yet. Please try again.");
      }
      if (!state.me) {
        note.textContent = "You are signed in. Create your inbox to get started.";
        create.textContent = "Create my inbox";
        create.hidden = false;
        create.onclick = () => openInbox(true);
        return;
      }
      note.textContent = "";
      create.hidden = true;
      await loadCapabilities();
      render();
      // Resume setup without wiring the page twice. Mail loading has its own recovery loop.
      try { await loadAll(); } finally { startLive(); }
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) {
        signOut();
        toast("Your session has expired. Sign in again.");
      } else if (state.me) {
        toast("Could not load your inbox: " + e.message);
      } else {
        note.textContent = (createIfMissing ? "Could not create your inbox: " : "Could not load your mailboxes: ") + e.message;
        create.textContent = "Try again";
        create.hidden = false;
        create.onclick = () => openInbox(createIfMissing);
      }
    } finally {
      state.openingInbox = false;
      create.disabled = false;
      render();
    }
  }

  // What calls.js may ask of the inbox: who is signed in as which mailbox, who is open, and how to
  // say something. It gets no access to message bodies.
  window.PigeonpostCallsHost = {
    signedIn: () => Boolean(getToken() && state.me),
    token: () => getToken(),
    renew: () => renewSession(),
    mailbox: () => (state.me ? { address: state.me.address, handle: state.me.handle || null } : null),
    peer: () => state.openPeer || null,
    knownPeer: (peer) => Boolean(peer && (contactFor(peer) || state.inbound.some((m) => peerKeyOf(m) === peer))),
    displayName: peerName,
    paintAvatar,
    // Phase 1b: lets call tiles show a profile picture over their initials.
    decorateAvatar: (el, key) => Avatars.decorate(el, key),
    toast,
  };

  // Phase 3a: what groups.js may ask of the inbox. Group content never passes through here; it is
  // decrypted in groups.js and rendered there.
  function pruneExpiredDirect() {
    const now = Date.now() / 1000;
    const before = state.inbound.length;
    state.inbound = state.inbound.filter((m) => !(m.expires_at && m.expires_at <= now));
    if (state.inbound.length !== before) render();
  }
  setInterval(pruneExpiredDirect, 5000);
  window.PigeonpostGroupsHost = {
    api: (path, opts) => api(path, opts),
    postbox: () => cfg.postbox,
    token: () => getToken(),
    renew: () => renewSession(),
    mailbox: () => (state.me ? { address: state.me.address, handle: state.me.handle || null } : null),
    identities: () => state.identities.slice(),
    contacts: () => state.contacts.slice(),
    features: () => new Set(features),
    leftView: () => (state.viewingArchive ? "archive" : state.leftView),
    filter: () => state.filter || "",
    onPhone: () => onPhone(),
    displayName, paintAvatar, toast, listTime, clockTime,
    closeConversation: () => {
      if (!state.openPeer) return;
      stashDraft();
      state.openPeer = null;
      state.openThread = null;
      render();
    },
    openDirectThread: () => {
      if (!state.openPeer) return null;
      const subs = subthreadsFor(state.openPeer);
      const id = state.openThread || (subs[0] && subs[0].id) || null;
      const t = id && state.serverThreads.find((x) => (x.thread_id || x.id) === id);
      return id ? { id, peer: state.openPeer, ephemeral_secs: t ? t.ephemeral_secs || null : null } : null;
    },
    reloadThreads: () => loadThreads().then(render),
    pruneExpired: pruneExpiredDirect,
    rerender: () => render(),
  };

  // Phase 3B: what calendar.js may ask of the inbox. Calendar data is not end-to-end encrypted; it is
  // fetched and rendered in calendar.js and never passes through here.
  window.PigeonpostCalendarHost = {
    postbox: () => cfg.postbox,
    token: () => getToken(),
    renew: () => renewSession(),
    mailbox: () => (state.me ? { address: state.me.address, handle: state.me.handle || null } : null),
    contacts: () => state.contacts.slice(),
    features: () => new Set(features),
    leftView: () => (state.viewingArchive ? "archive" : state.leftView),
    setLeftView: (view) => setLeftView(view),
    onPhone: () => onPhone(),
    toast,
    closeConversation: () => window.PigeonpostGroupsHost.closeConversation(),
    openConversation: (peer) => {
      if (!peer) return;
      setLeftView("inbox");
      openThread(peer);
    },
  };

  // Phase 7: what meeting-notes.js may ask of the inbox. Notes are ciphertext from the postbox and
  // are decrypted in meeting-notes.js with this device's own key; nothing passes through here.
  window.PigeonpostNotesHost = {
    postbox: () => cfg.postbox,
    token: () => getToken(),
    renew: () => renewSession(),
    mailbox: () => (state.me ? { address: state.me.address, handle: state.me.handle || null } : null),
    features: () => new Set(features),
    toast,
  };

  async function boot() {
    wire();
    await completeLoginIfReturning();

    if (!getToken()) {
      render();
      return;
    }
    // Before the first call, not after it fails.
    await resumeSession();
    applyMessageScale(messageScale());
    if (location.hash === "#archive") state.viewingArchive = true;
    // A tab that slept through its renewal timer wakes holding a dead token; catch that on the way
    // back rather than on the next request.
    document.addEventListener("visibilitychange", () => {
      if (!document.hidden) resumeSession();
    });
    await openInbox();
  }

  boot();
})();
