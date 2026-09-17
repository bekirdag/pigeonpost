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

  // Default inboxes must identify their owner: /alp and /bekir are different people.
  function displayName(peer) {
    if (!peer) return "unknown";
    if (peer.startsWith("/k/")) return peer.slice(0, 12) + "…";
    const parts = peer.split("/").filter(Boolean);
    if (parts.length === 2 && parts[1] === "main") return "/" + parts[0];
    return parts.length > 1 ? parts[parts.length - 1] : peer;
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

  function initials(peer) {
    const name = displayName(peer).replace(/[^a-z0-9]/gi, "");
    return (name.slice(0, 2) || "··").toUpperCase();
  }

  // Stable per-peer colour so a thread keeps its face between sessions.
  function tone(peer) {
    let h = 0;
    for (let i = 0; i < peer.length; i++) h = (h * 31 + peer.charCodeAt(i)) >>> 0;
    return (h % 6) + 1;
  }

  function paintAvatar(el, peer) {
    el.textContent = initials(peer);
    el.dataset.tone = String(tone(peer));
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
    $("threads").textContent = "";
    state = freshState();
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
      li.append(name, size, drop);
      list.append(li);
    });
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
    reconcile(serverIds) {
      this.rows = this.rows.filter(
        (r) => r.status === "failed" || !(r.sent_copy_id && serverIds.has(r.sent_copy_id)),
      );
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
      paintAvatar(av, id.handle || id.address);
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
    paintAvatar(av, t.peer);

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
    $("pane-thread").dataset.open = String(open && (!onPhone() || state.openThread !== null || !subthreadsFor(state.openPeer).length));
    $("thread-head").hidden = !open;
    $("composer").hidden = !open;
    $("thread-empty").hidden = open;
    $("peer-info").hidden = !(open && state.showInfo);

    // The same control both ways: in the archive it is the way back out, which is where somebody
    // looking at a filed conversation would go looking for it.
    const filed = open && state.archived.has(state.openPeer);
    const archiveBtn = $("archive-btn");
    archiveBtn.hidden = !open;
    archiveBtn.title = filed ? "Move back to your inbox" : "Archive this conversation";
    archiveBtn.setAttribute("aria-label", archiveBtn.title);

    if (!open) {
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
    const thread = showing
      ? { ...conversation, messages: conversation.messages.filter((m) => (m.thread_id || "") === showing.id) }
      : conversation;

    $("peer-name").textContent = threadName(thread);
    $("peer-sub").textContent = thread.mine ? thread.peer + " · your mailbox" : thread.peer;
    paintAvatar($("peer-avatar"), thread.peer);
    $("delete-thread-btn").hidden = !showing?.id;
    renderMessages(thread.messages, showing?.id || "");

    if (state.showInfo) renderPeerInfo(thread);

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
    if (!query) return;
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
      let previous = null;
      for (const m of visible) {
        if (!previous || !sameDay(previous.at, m.at)) {
          const sep = document.createElement("li"), label = document.createElement("span");
          sep.className = "sep";
          label.className = "daybreak";
          label.textContent = dayLabel(m.at);
          sep.append(label);
          rows.push(sep);
        }
        const fingerprint = JSON.stringify([m, view.query, m.id === found]);
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
      rows.forEach((row, index) => { if (list.children[index] !== row) list.insertBefore(row, list.children[index] || null); });
      while (list.children.length > rows.length) list.lastElementChild.remove();
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
    const li = document.createElement("li");
    li.dataset.messageId = m.id;
    if (m.kind === "out") li.className = "mine";

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

    // Files that came with the message. Names and sizes, and a link that downloads — never an
    // inline preview: the bytes are another agent's and the postbox serves them as attachments
    // precisely so a browser does not open them in this origin.
    if (Array.isArray(m.attachments) && m.attachments.length) {
      const files = document.createElement("ul");
      files.className = "files";
      for (const file of m.attachments) {
        const li = document.createElement("li");
        const link = document.createElement("a");
        link.className = "file";
        link.href = cfg.postbox + "/v1/attachments/" + encodeURIComponent(file.id);
        link.textContent = file.filename || "attachment";
        // The token cannot ride in a plain link, so this fetches with it and hands the browser a
        // blob. Same reason the download endpoint is authenticated at all: a file is not public
        // because its id is known.
          link.addEventListener("click", async (event) => {
          event.preventDefault();
          const context = mailboxContext();
          try {
            const res = await fetch(link.href, {
              headers: {
                authorization: "Bearer " + getToken(),
                "x-pigeonpost-identity": context.address,
              },
              signal: context.signal,
            });
            if (!res.ok) throw new Error(String(res.status));
            const blob = await res.blob();
            if (!context.current()) return;
            const url = URL.createObjectURL(blob);
            const save = document.createElement("a");
            save.href = url;
            save.download = file.filename || "attachment";
            save.click();
            URL.revokeObjectURL(url);
          } catch (_) {
            if (context.current()) toast("Could not download that file.");
          }
        });
        const size = document.createElement("span");
        size.className = "file-size";
        size.textContent = readableBytes(file.bytes);
        li.append(link, size);
        files.append(li);
      }
      bubble.append(files);
    }

    const meta = document.createElement("div");
    meta.className = "meta";
    if (m.kind === "out" && m.status === "failed") {
      const failed = document.createElement("span");
      failed.className = "failed";
      failed.textContent = "not sent";
      meta.append(failed);
    }
    // Visible rather than hidden behind a selection: an agent's reply is the thing people most
    // often want out of this app and into somewhere else.
    const copy = document.createElement("button");
    copy.type = "button";
    copy.className = "copy";
    copy.title = "Copy message";
    copy.setAttribute("aria-label", "Copy message");
    copy.textContent = "⧉";
    copy.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(copyTextOf(m));
        copy.textContent = "✓";
        copy.classList.add("copied");
        setTimeout(() => {
          copy.textContent = "⧉";
          copy.classList.remove("copied");
        }, 1400);
      } catch (e) {
        // Clipboard access can be refused outright; say nothing rather than throwing a dialog at
        // somebody for pressing a small button.
      }
    });
    meta.append(copy);

    const time = document.createElement("span");
    time.textContent = clockTime(m.at);
    meta.append(time);
    bubble.append(meta);

    li.append(bubble);
    return li;
  }

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

  function renderPeerInfo(thread) {
    const box = $("peer-info");
    box.textContent = "";
    const c = thread.contact;
    const lastIn = [...thread.messages].reverse().find((m) => m.kind === "in");

    const dl = document.createElement("dl");
    const add = (term, value, mono) => {
      const dt = document.createElement("dt");
      dt.textContent = term;
      const dd = document.createElement("dd");
      dd.textContent = value;
      if (mono) dd.className = "mono";
      dl.append(dt, dd);
    };

    add("Address", (thread.identity && thread.identity.address) || (lastIn && lastIn.address) || thread.peer, true);
    if (thread.peer.startsWith("/") && !thread.peer.startsWith("/k/")) add("Handle", thread.peer, true);
    if (thread.mine) add("Mailbox", "yours — on this account");
    add("Contact", c ? (c.peer === thread.peer ? "yes" : "via " + c.peer) : "not a contact");
    add("Admission", c ? c.admission : "default policy");
    add("Autonomy", c ? c.autonomy : "review");
    add("Granted verbs", c && c.allowed_verbs && c.allowed_verbs.length ? c.allowed_verbs.join(", ") : "none");
    if (lastIn && lastIn.standing) add("Standing", lastIn.standing + (lastIn.tier ? " · " + lastIn.tier : ""));

    box.append(dl);

    // Writing *to* your agent and reading *as* your agent are different things, and the difference
    // is not obvious from a row that looks like every other conversation. Say which one you are in
    // and offer the other.
    if (thread.mine && thread.identity) {
      const open = document.createElement("button");
      open.type = "button";
      open.className = "btn btn-primary open-mailbox";
      open.textContent = "Open this mailbox";
      open.onclick = () => switchIdentity(thread.identity);
      box.append(open);
    }

    // The panel decides rather than describes. It used to list this sender's admission, autonomy
    // and granted verbs as read-only text and then tell you to go and run `pigeonpost postbox
    // allow` — sending someone who is looking straight at the sender, in a browser, to install a
    // command line tool to change the thing on screen. The editor already existed one sheet away
    // in Settings; this is the same editor, opened on this sender.
    if (!thread.mine) {
      const edit = document.createElement("button");
      edit.type = "button";
      edit.className = "btn btn-primary open-mailbox";
      edit.textContent = c && c.peer === thread.peer ? "Edit this sender" : "Trust this sender";
      edit.onclick = () => openContact(c && c.peer === thread.peer ? c : null, thread.peer);
      box.append(edit);
    }

    const note = document.createElement("p");
    note.className = "note";
    if (thread.mine) {
      note.textContent = "You are writing to this agent from " + (state.me.handle || state.me.address)
        + ". Opening the mailbox instead shows the mail it has received.";
    } else if (c && c.autonomy === "auto") {
      note.textContent = "Requests naming a granted verb are acted on without you. Everything else is held.";
    } else if (c && c.peer !== thread.peer) {
      // A wildcard row is a rule about a fleet. Editing it here would quietly change what every
      // other member of that fleet may do, which is not what "this sender" means.
      note.textContent = "Nothing from this sender is acted on automatically. They are covered by "
        + c.peer + "; trusting them on their own gives them settings of their own.";
    } else {
      note.textContent = "Nothing from this sender is acted on automatically.";
    }
    box.append(note);
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
        await api(withIdentity("/v1/ack", context.address), { method: "POST", body: { message_id: m.message_id, identity: context.address } });
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

  async function sendMessage(text) {
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
      if (composerKey() === draftKey) {
      staged.length = 0;
      renderStaged();
      }
    }
    text = composeBody(text);
    const record = Pending.add({
      local_id: "local_" + randomString(8),
      mailbox: context.address,
      to,
      body: text,
      at: Math.floor(Date.now() / 1000),
      status: "sending",
      thread_id: threadId,
      attachments: files.map((file, i) => ({ id: attachments[i], filename: file.name, bytes: file.size })),
    });
    render();

    try {
      const sent = await api("/v1/send", {
        method: "POST",
        body: threadId
          ? { to, body: text, from: context.address, thread_id: threadId, attachments }
          : { to, body: text, from: context.address, attachments },
      });
      // The id of the server's own copy. Holding it is what lets the optimistic row retire the
      // moment that copy comes back, instead of the message appearing twice for a poll.
      record.sent_copy_id = sent.sent_copy_id || null;
      record.status = "sent";
      drafts.delete(draftKey);
      if (context.current() && composerKey() === draftKey) {
        if ($("compose").value.trim() === originalText) $("compose").value = "";
        jumpToLatest();
      }
      // Nothing to reconcile against if the postbox did not keep a copy; drop the optimistic row
      // and let the next poll be the truth.
      if (!record.sent_copy_id) Pending.reconcile(new Set());
      if (context.current()) loadInbox().then(render).catch(() => {});
    } catch (e) {
      record.status = "failed";
      if (context.current()) toast(sendFailure(e));
    }
    if (context.current()) render();
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
    const results = await Promise.allSettled([loadInbox(), loadContacts(), loadArchive(), loadThreads()]);
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
        withIdentity("/v1/inbox", context.address) + "&include_sent=true&include_read=true",
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
    Pending.reconcile(new Set(state.inbound.map((m) => m.message_id)));
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
        const path = "/v1/events" + (liveCursor === null ? "" : "?last_event_id=" + encodeURIComponent(liveCursor));
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
            if (event !== "mail") return;
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
        const path = withIdentity("/v1/inbox") + "&include_sent=true&include_read=true"
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
  }
  function closeSheet(id) {
    $(id).hidden = true;
    if (id === "settings-sheet") $("settings-btn").focus();
    const at = sheetStack.indexOf(id);
    if (at !== -1) sheetStack.splice(at, 1);
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
    try {
      // Through `composeBody` for the same reason the composer is: the first message to a peer is
      // still a message from this app, and sending it as prose is what left it classified
      // `not_a_request` and parked for a human. That the *opening* message was the one going out
      // bare is what made this look like the recipient was broken rather than the sender — every
      // reply after it was already an envelope, so nothing further in the thread showed it.
      const sent = await api("/v1/send", { method: "POST", body: { to, body: composeBody(body), from: context.address } });
      if (!context.current()) return;
      closeSheet("new-sheet");
      await loadAll();
      if (!context.current()) return;
      // A conversation started with a namespace or a `/k/` address is filed by the server under
      // whatever peer it resolved to, so open by what came back rather than by what was typed.
      const copy = state.inbound.find(m => m.message_id === sent.sent_copy_id);
      const resolved = copy ? peerKeyOf(copy) : normalisePeer(to);
      const started = buildThreads().find((t) => t.peer === resolved || t.peer === to);
      if (started) openThread(started.peer);
      else render();
    } catch (e) {
      if (context.current()) fail(sendFailure(e));
    } finally {
      if (context.current()) $("new-send").disabled = false;
    }
  }

  // ---- settings --------------------------------------------------------------------------------

  let settingsPage = "root";
  const settingsTitles = { root: "Settings", account: "Account", handles: "Handles",
    inbox: "Inbox and appearance", contacts: "Contacts and permissions", help: "Help and about" };
  function showSettingsPage(next, focus = true) {
    if (!Object.hasOwn(settingsTitles, next)) return;
    const previous = settingsPage;
    settingsPage = next;
    document.querySelectorAll("[data-settings-page]").forEach(el => { el.hidden = el.dataset.settingsPage !== next; });
    $("settings-title").textContent = settingsTitles[next];
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
          .filter(el => !el.closest("[hidden]"));
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

    $("peer-info-btn").onclick = () => {
      state.showInfo = !state.showInfo;
      renderThread();
    };

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
      $("send-btn").disabled = !compose.value.trim() || sendingDrafts.has(composerKey());
    };
    compose.addEventListener("input", autosize);

    // Enter sends, Shift+Enter breaks the line — but only where there is a keyboard to do it with.
    compose.addEventListener("keydown", (e) => {
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
      if (!text || !state.openPeer || sendingDrafts.has(key)) return;
      sendingDrafts.add(key);
      compose.disabled = true;
      autosize();
      sendMessage(text).finally(() => {
        sendingDrafts.delete(key);
        if (composerKey() === key) { compose.disabled = false; autosize(); }
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
