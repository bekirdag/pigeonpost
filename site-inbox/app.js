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

  // A handle reads as a name; a key address does not. Show the last meaningful segment either way.
  function displayName(peer) {
    if (!peer) return "unknown";
    if (peer.startsWith("/k/")) return peer.slice(0, 12) + "…";
    const parts = peer.split("/").filter(Boolean);
    return parts.length > 1 ? parts[parts.length - 1] : peer;
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

  async function api(path, opts) {
    const o = opts || {};
    const call = async () => {
      const res = await fetch(cfg.postbox + path, {
        method: o.method || "GET",
        headers: Object.assign(
          { authorization: `Bearer ${getToken()}`, accept: "application/json" },
          o.body ? { "content-type": "application/json" } : {},
        ),
        body: o.body ? JSON.stringify(o.body) : undefined,
        signal: o.signal,
      });
      const body = await res.json().catch(() => ({}));
      if (!res.ok) throw new ApiError(res.status, body.error, body.detail);
      return body;
    };
    try {
      return await call();
    } catch (e) {
      if (e instanceof ApiError && e.status === 401 && (await renewSession())) return call();
      throw e;
    }
  }

  // `?identity=` picks which mailbox an account acts as. Always sent once one is chosen: an account
  // with several mailboxes is refused rather than guessed at, which is the right call server-side
  // and a confusing error here if we let it happen.
  const withIdentity = (path) => {
    const address = state.me && state.me.address;
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
    $("signin").hidden = signedIn;
    $("app").hidden = !signedIn;
    if (!signedIn) return;
    renderMe();
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
      String(visible && onPhone() && !state.openThread),
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

  function renderMe() {
    if (!state.me) return;
    const name = state.me.handle || state.me.address;
    $("me-name").textContent = displayName(name);
    $("me-sub").textContent = state.me.handle || state.me.address;
    paintAvatar($("me-avatar"), name);
    $("identity-btn").disabled = state.identities.length < 2;
    $("identity-btn").querySelector(".chev").style.visibility =
      state.identities.length < 2 ? "hidden" : "visible";
  }

  function renderIdentityMenu() {
    const menu = $("identity-menu");
    menu.textContent = "";
    for (const id of state.identities) {
      const li = document.createElement("li");
      const btn = document.createElement("button");
      btn.type = "button";
      btn.setAttribute("role", "option");
      btn.setAttribute("aria-selected", String(state.me && id.address === state.me.address));

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
      li.append(btn);
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
    $("threads-empty").hidden = threads.length > 0;

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
    const open = Boolean(state.openPeer) && Boolean(currentSubthread(state.openPeer) || !subsVisible());
    $("pane-thread").dataset.open = String(open && (!onPhone() || Boolean(state.openThread) || !subsVisible()));
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

    const list = $("messages");
    list.textContent = "";
    if (!open) return;

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

    let previous = null;
    for (const m of thread.messages) {
      if (!previous || !sameDay(previous.at, m.at)) {
        const sep = document.createElement("li");
        sep.className = "sep";
        const label = document.createElement("span");
        label.className = "daybreak";
        label.textContent = dayLabel(m.at);
        sep.append(label);
        list.append(sep);
      }
      list.append(messageNode(m));
      previous = m;
    }

    if (state.showInfo) renderPeerInfo(thread);

    // Jump to the newest, the way a messenger does.
    const scroll = $("thread-scroll");
    requestAnimationFrame(() => { scroll.scrollTop = scroll.scrollHeight; });
  }

  function messageNode(m) {
    const li = document.createElement("li");
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
      req.append(decision);
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
  // quotes, rules, and inline emphasis. Anything unrecognised is shown as the literal line it was.
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
    if (onPhone() && state.openThread && subsVisible()) {
      state.openThread = null;
      state.showInfo = false;
      render();
      return;
    }
    state.openPeer = null;
    state.openThread = null;
    state.showInfo = false;
    render();
  }

  function openSubthread(id) {
    state.openThread = id;
    state.showInfo = false;
    if (onPhone()) history.replaceState({ thread: state.openPeer, sub: id }, "");
    render();
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
    const peer = state.openPeer;
    const title = $("thread-title-input").value.trim();
    if (!peer) return;
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
        body: { peer, title, identity: state.me.address },
      });
      await loadThreads();
      // Selected straight away, and the pane it belongs in appears with it: opening a thread and
      // then having to find it in a list that just appeared is two steps for one intention.
      state.openThread = made.thread_id;
      closeSheet("thread-sheet");
      render();
      if (!onPhone()) $("compose").focus({ preventScroll: true });
    } catch (e) {
      $("thread-error").textContent =
        e instanceof ApiError ? e.message : "Could not open the thread.";
      $("thread-error").hidden = false;
    } finally {
      button.disabled = false;
    }
  }

  async function openThread(peer) {
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
    const peer = state.openPeer;
    if (!peer) return;
    const showing = subsVisible() ? currentSubthread(peer) : null;
    const unread = state.inbound.filter(
      (m) =>
        peerKeyOf(m) === peer &&
        !m.read &&
        (!showing || (m.thread_id || "") === showing.id),
    );
    if (!unread.length) return;
    for (const m of unread) m.read = true;
    renderThreadList();
    renderSubs();
    for (const m of unread) {
      try {
        await api(withIdentity("/v1/ack"), { method: "POST", body: { message_id: m.message_id, identity: state.me.address } });
      } catch (_) { /* it will still be there next poll; nothing is lost by a failed ack */ }
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
    const to = state.openPeer;
    // Whichever thread is on screen. Sending into the conversation you are reading is the only
    // behaviour that does not surprise: the alternative is a reply that leaves the thread it
    // answers.
    const showing = currentSubthread(to);
    const threadId = showing && showing.id ? showing.id : null;
    text = composeBody(text);
    const record = Pending.add({
      local_id: "local_" + randomString(8),
      mailbox: state.me.address,
      to,
      body: text,
      at: Math.floor(Date.now() / 1000),
      status: "sending",
      thread_id: threadId,
    });
    render();

    try {
      const sent = await api("/v1/send", {
        method: "POST",
        body: threadId
          ? { to, body: text, from: state.me.address, thread_id: threadId }
          : { to, body: text, from: state.me.address },
      });
      // The id of the server's own copy. Holding it is what lets the optimistic row retire the
      // moment that copy comes back, instead of the message appearing twice for a poll.
      record.sent_copy_id = sent.sent_copy_id || null;
      record.status = "sent";
      // Nothing to reconcile against if the postbox did not keep a copy; drop the optimistic row
      // and let the next poll be the truth.
      if (!record.sent_copy_id) Pending.reconcile(new Set());
      loadInbox().then(render).catch(() => {});
    } catch (e) {
      record.status = "failed";
      toast(sendFailure(e));
    }
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
    stopLive();
    LS.setItem(K.identity, identity.address);
    state.me = { address: identity.address, handle: identity.handle };
    state.openPeer = null;
    state.showInfo = false;
    state.inbound = [];
    Pending.clear();
    renderIdentityMenu();
    render();
    await loadAll();
    startLive();
  }

  // ---- loading -------------------------------------------------------------------------------

  async function loadIdentities() {
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
    state.identities = resolved;

    // Default to the operator's own named mailbox rather than whichever address the server
    // happened to list first. A handle is a mailbox somebody deliberately named — usually the one
    // they think of as "my inbox" — while an anonymous /k/ address is typically an agent's. An
    // explicit earlier choice still wins over both.
    const remembered = LS.getItem(K.identity);
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
      state.me = { address: chosen.address, handle: chosen.handle };
    }
    renderIdentityMenu();
  }

  async function loadAll() {
    await Promise.all([loadInbox(), loadContacts(), loadArchive(), loadThreads()]);
    render();
  }

  async function loadInbox(signal) {
    // include_sent turns the listing into a conversation. Opt-in on the wire, because every other
    // caller of this endpoint reads it as mail addressed to them.
    //
    // include_read matters just as much here and pulls the other way from the agent case. A
    // polling agent wants only what is new, so acknowledged mail leaves its listing. A person
    // reading a thread wants the thread — hiding a message the moment it was acknowledged would
    // make conversations lose their own history as they are read.
    let body;
    try {
      body = await api(
        withIdentity("/v1/inbox") + "&include_sent=true&include_read=true",
        { signal },
      );
    } catch (e) {
      // Still thrown — a caller that wants to sign out on a 401 needs to see it. This only records
      // that the postbox was not reached, which every caller of this would otherwise swallow.
      if (!(e instanceof ApiError)) setOffline(true);
      throw e;
    }
    setOffline(false);
    adopt(body);
  }

  // Take a server listing as the truth, and retire any optimistic row it now accounts for.
  function adopt(body) {
    state.inbound = body.messages || [];
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

  async function loadThreads() {
    try {
      const body = await api(withIdentity("/v1/threads"));
      state.serverThreads = body.threads || [];
      setOffline(false);
    } catch (e) {
      // A postbox that does not know about threads yet answers 404/501 here. Everything still
      // works: threads are then whatever the messages themselves say, and a peer with one
      // conversation — which is all such a postbox can produce — shows no thread list at all.
      if (answered(e)) state.serverThreads = [];
      else setOffline(true);
    }
  }

  async function loadContacts() {
    try {
      const body = await api(withIdentity("/v1/contacts"));
      state.contacts = body.contacts || [];
      state.vocabulary = body.vocabulary || null;
      state.policy = body.policy || state.policy;
      setOffline(false);
    } catch (e) {
      if (answered(e)) state.contacts = [];
      else setOffline(true);
    }
  }

  async function loadArchive() {
    try {
      const body = await api(withIdentity("/v1/archive"));
      state.archived = new Set(body.archived || []);
      setOffline(false);
    } catch (e) {
      // An archive we could not read must not hide anything: failing open shows a conversation
      // that should have been filed, failing closed hides one that should not be. Only one of
      // those loses mail.
      if (answered(e)) state.archived = new Set();
      else setOffline(true);
    }
  }

  // The banner, not a dialog and not an empty screen: what is loaded stays readable and stays
  // scrollable, and the one thing that changes is that the page stops claiming to be current.
  // Sending is still allowed — the composer's own failure path is what says a message did not go,
  // and it says it about that message rather than about the whole app.
  function setOffline(off) {
    const next = Boolean(off);
    if (state.offline === next) return;
    state.offline = next;
    const el = $("offline-banner");
    if (el) el.hidden = !next;
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
    // Move it in the UI first: filing something is a gesture that should feel instant, and the
    // server call is a formality that either confirms it or is undone below.
    if (archived) state.archived.add(peer);
    else state.archived.delete(peer);
    if (state.openPeer === peer) closeThread();
    render();
    try {
      await api("/v1/archive", {
        method: "PUT",
        body: { peer, archived, identity: state.me.address },
      });
      toast(archived ? "Archived." : "Moved back to your inbox.");
    } catch (e) {
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

        setOffline(false);
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

  function stopPolling() {
    polling = false;
    if (pollController) pollController.abort();
    pollController = null;
  }

  async function startPolling() {
    if (polling || !state.me) return;
    polling = true;
    let backoff = 1000;
    while (polling) {
      pollController = new AbortController();
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
        if (!polling) break;
        setOffline(false);
        const before = state.inbound.length;
        adopt(body);
        if (state.inbound.length !== before) render();
        else renderThreadList();
        backoff = 1000;
      } catch (e) {
        if (!polling) break;
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
  function openSheet(id) {
    $(id).hidden = false;
    const at = sheetStack.indexOf(id);
    if (at !== -1) sheetStack.splice(at, 1);
    sheetStack.push(id);
  }
  function closeSheet(id) {
    $(id).hidden = true;
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
    $("new-peer").value = "";
    $("new-body").value = "";
    $("new-error").hidden = true;
    openSheet("new-sheet");
    $("new-peer").focus();
  }

  async function sendNewConversation() {
    const to = $("new-peer").value.trim();
    const body = $("new-body").value.trim();
    const fail = (message) => {
      const el = $("new-error");
      el.textContent = message;
      el.hidden = false;
    };
    if (!to) return fail("Who is it for?");
    if (!body) return fail("Write something to send.");

    $("new-send").disabled = true;
    try {
      await api("/v1/send", { method: "POST", body: { to, body, from: state.me.address } });
      closeSheet("new-sheet");
      await loadAll();
      // A conversation started with a namespace or a `/k/` address is filed by the server under
      // whatever peer it resolved to, so open by what came back rather than by what was typed.
      const started = buildThreads().find((t) => t.peer === normalisePeer(to) || t.peer === to);
      if (started) openThread(started.peer);
      else render();
    } catch (e) {
      fail(sendFailure(e));
    } finally {
      $("new-send").disabled = false;
    }
  }

  // ---- settings --------------------------------------------------------------------------------

  function openSettings() {
    applyMessageScale(messageScale());
    // An unnamed mailbox has no handle, and saying "—" is more honest than repeating its address
    // on the line above the one that already shows it.
    $("acct-mailbox").textContent = state.me ? (state.me.handle || "not named") : "—";
    $("acct-address").textContent = state.me ? state.me.address : "—";
    $("acct-postbox").textContent = cfg.postbox;
    $("archive-count").textContent =
      state.archived.size === 1 ? "1 conversation" : state.archived.size + " conversations";
    $("archive-link").textContent = location.origin + "/#archive";
    renderContactList();
    openSheet("settings-sheet");
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
          identity: state.me.address,
        },
      });
      closeSheet("contact-sheet");
      await loadContacts();
      renderContactList();
      render();
    } catch (e) {
      fail(e && e.message ? e.message : "Could not save.");
    } finally {
      $("contact-save").disabled = false;
    }
  }

  async function removeContact() {
    if (!editingContact) return;
    try {
      await api("/v1/contacts", {
        method: "DELETE",
        body: { peer: editingContact.peer, identity: state.me.address },
      });
      closeSheet("contact-sheet");
      await loadContacts();
      renderContactList();
      render();
      toast("Removed. They get whatever strangers get.");
    } catch (e) {
      toast("Could not remove them.");
    }
  }

  function wire() {
    $("signin-btn").onclick = () => login();
    $("signout-btn").onclick = () => signOut();

    $("new-btn").onclick = () => openNewConversation();
    $("new-close").onclick = () => closeSheet("new-sheet");
    $("new-cancel").onclick = () => closeSheet("new-sheet");
    $("new-send").onclick = () => sendNewConversation();
    wireSheet("new-sheet");

    $("settings-btn").onclick = () => openSettings();
    $("settings-close").onclick = () => closeSheet("settings-sheet");
    $("settings-done").onclick = () => closeSheet("settings-sheet");
    wireSheet("settings-sheet");

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
      if (e.key !== "Escape") return;
      for (let i = sheetStack.length - 1; i >= 0; i -= 1) {
        const id = sheetStack[i];
        if (!$(id).hidden) {
          closeSheet(id);
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
      compose.style.height = Math.min(compose.scrollHeight, window.innerHeight * 0.4) + "px";
      $("send-btn").disabled = !compose.value.trim();
    };
    compose.addEventListener("input", autosize);

    // Enter sends, Shift+Enter breaks the line — but only where there is a keyboard to do it with.
    compose.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.shiftKey && window.matchMedia("(min-width: 781px)").matches) {
        e.preventDefault();
        $("composer").requestSubmit();
      }
    });

    $("composer").addEventListener("submit", (e) => {
      e.preventDefault();
      const text = compose.value.trim();
      if (!text || !state.openPeer) return;
      compose.value = "";
      autosize();
      sendMessage(text);
    });

    // One history entry covers a peer, and closeThread walks out of it a screen at a time. Pushing
    // it back when there is still a level to leave is what keeps the system gesture in step with
    // the on-screen back button.
    window.addEventListener("popstate", () => {
      if (!state.openPeer) return;
      const stillInside = onPhone() && state.openThread && subsVisible();
      closeThread();
      if (stillInside) history.pushState({ thread: state.openPeer }, "");
    });

    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && state.openPeer) closeThread();
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

    // The browser knows before a request has to time out to find it.
    window.addEventListener("online", () => {
      if (getToken() && state.me && document.visibilityState === "visible") startLive();
    });
    window.addEventListener("offline", () => setOffline(true));

    $("send-btn").disabled = true;
  }

  // ---- boot ---------------------------------------------------------------------------------

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
    render();

    try {
      await loadIdentities();
      if (!state.me) {
        // Signing in used to end here, telling someone who had just authenticated in a browser to
        // go and install a command line tool. An account holder needs no proof-of-work to mint —
        // the postbox creates under the account on the strength of this very token — so the whole
        // remaining step is one call the page can already make.
        $("signin-note").textContent = "You are signed in, but have no inbox yet.";
        const create = $("create-inbox-btn");
        create.hidden = false;
        create.disabled = false;
        create.onclick = async () => {
          create.disabled = true;
          $("signin-note").textContent = "Creating your inbox…";
          try {
            await api("/v1/identities", { method: "POST", body: {} });
            $("signin-note").textContent = "";
            create.hidden = true;
            // Resume where boot() left off rather than re-running it: boot wires the event
            // handlers, and running it twice would bind every one of them a second time.
            await loadIdentities();
            if (state.me) {
              renderMe();
              await loadAll();
              startLive();
            }
          } catch (err) {
            create.disabled = false;
            $("signin-note").textContent =
              "Could not create an inbox: " + (err && err.message ? err.message : "unknown error");
          }
        };
        render();
        return;
      }
      renderMe();
      await loadAll();
      startLive();
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) {
        signOut();
        toast("Your session has expired. Sign in again.");
        return;
      }
      toast("Could not load your mailboxes: " + e.message);
    }
  }

  boot();
})();
