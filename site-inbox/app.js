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
    stopPolling();
    state = freshState();
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
      openPeer: null,    // peer key of the open thread
      filter: "",
      showInfo: false,
    };
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
        });
        continue;
      }
      t.messages.push({
        kind: "in",
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
      });
    }

    // A contact you have never exchanged mail with still deserves a row — that is how you start a
    // conversation with an agent you have only been told about. Wildcards are policy, not people.
    for (const c of state.contacts) {
      if (c.peer.endsWith("/*")) continue;
      touch(c.peer);
    }

    // Your own agents are always present, silent or not. Marking the row rather than building a
    // second list keeps one row per peer: an agent you talk to daily and one you have never written
    // to are the same kind of thing, and should not appear twice.
    for (const id of ownAgents()) {
      const t = touch(identityKey(id));
      t.mine = true;
      t.identity = id;
    }

    for (const t of threads.values()) {
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
    renderThread();
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
    const threads = buildThreads();
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

    // Your own agents sit at the top under their own heading. They are the reason this app exists
    // for a namespace owner, and burying a silent agent below every stranger who ever wrote in
    // would be the wrong way round.
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
    if (envelope) return "asks to " + envelope.verb.replace(/_/g, " ");
    return m.body.replace(/\s+/g, " ").slice(0, 140);
  }

  function renderThread() {
    const open = Boolean(state.openPeer);
    $("pane-thread").dataset.open = String(open);
    $("thread-head").hidden = !open;
    $("composer").hidden = !open;
    $("thread-empty").hidden = open;
    $("peer-info").hidden = !(open && state.showInfo);

    const list = $("messages");
    list.textContent = "";
    if (!open) return;

    const thread = buildThreads().find((t) => t.peer === state.openPeer)
      || { peer: state.openPeer, messages: [], contact: contactFor(state.openPeer) };

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

    const envelope = m.kind === "in" ? requestEnvelope(m.body) : null;

    if (envelope) {
      const req = document.createElement("div");
      req.className = "request";
      const verb = document.createElement("div");
      verb.className = "verb";
      verb.textContent = envelope.verb;
      req.append(verb);
      if (envelope.args && Object.keys(envelope.args).length) {
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
      // Bodies are other agents' text. textContent, always.
      const text = document.createElement("div");
      text.className = "text";
      text.textContent = m.body;
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
    const time = document.createElement("span");
    time.textContent = clockTime(m.at);
    meta.append(time);
    bubble.append(meta);

    li.append(bubble);
    return li;
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

    const note = document.createElement("p");
    note.className = "note";
    if (thread.mine) {
      note.textContent = "You are writing to this agent from " + (state.me.handle || state.me.address)
        + ". Opening the mailbox instead shows the mail it has received.";
    } else {
      note.textContent = c && c.autonomy === "auto"
        ? "Requests naming a granted verb are acted on without you. Everything else is held."
        : "Nothing from this sender is acted on automatically. Grant autonomy with `pigeonpost postbox allow` if you want that.";
    }
    box.append(note);
  }

  // ---- actions ------------------------------------------------------------------------------

  const onPhone = () => window.matchMedia("(max-width: 780px)").matches;

  function closeThread() {
    state.openPeer = null;
    state.showInfo = false;
    render();
  }

  async function openThread(peer) {
    const wasClosed = !state.openPeer;
    state.openPeer = peer;
    state.showInfo = false;
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

    // Opening a conversation is reading it. Acknowledging clears the unread mark server-side, which
    // is also what tells an agent sharing this mailbox that the message has been dealt with — so it
    // is a real decision, not just a UI flourish.
    const unread = state.inbound.filter((m) => peerKeyOf(m) === peer && !m.read);
    if (!unread.length) return;
    for (const m of unread) m.read = true;
    renderThreadList();
    for (const m of unread) {
      try {
        await api(withIdentity("/v1/ack"), { method: "POST", body: { message_id: m.message_id, identity: state.me.address } });
      } catch (_) { /* it will still be there next poll; nothing is lost by a failed ack */ }
    }
  }

  async function sendMessage(text) {
    const to = state.openPeer;
    const record = Pending.add({
      local_id: "local_" + randomString(8),
      mailbox: state.me.address,
      to,
      body: text,
      at: Math.floor(Date.now() / 1000),
      status: "sending",
    });
    render();

    try {
      const sent = await api("/v1/send", {
        method: "POST",
        body: { to, body: text, from: state.me.address },
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
    stopPolling();
    LS.setItem(K.identity, identity.address);
    state.me = { address: identity.address, handle: identity.handle };
    state.openPeer = null;
    state.showInfo = false;
    state.inbound = [];
    Pending.clear();
    renderIdentityMenu();
    render();
    await loadAll();
    startPolling();
  }

  // ---- loading and polling ------------------------------------------------------------------

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
    const preferred = cfg && cfg.primaryNamespace
      ? named.find((i) => i.handle.startsWith(cfg.primaryNamespace + "/"))
      : null;
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
    await Promise.all([loadInbox(), loadContacts()]);
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
    const body = await api(
      withIdentity("/v1/inbox") + "&include_sent=true&include_read=true",
      { signal },
    );
    adopt(body);
  }

  // Take a server listing as the truth, and retire any optimistic row it now accounts for.
  function adopt(body) {
    state.inbound = body.messages || [];
    state.policy = body.policy || null;
    Pending.reconcile(new Set(state.inbound.map((m) => m.message_id)));
  }

  async function loadContacts() {
    try {
      const body = await api(withIdentity("/v1/contacts"));
      state.contacts = body.contacts || [];
    } catch (_) {
      state.contacts = [];
    }
  }

  // Long-poll: the postbox holds the request open until mail lands or the budget runs out, so this
  // is a live inbox without a socket and without hammering the server.
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
        const path = withIdentity("/v1/inbox") + "&include_sent=true"
          + "&wait=" + encodeURIComponent(cfg.waitSeconds || 25);
        const body = await api(path, { signal: pollController.signal });
        if (!polling) break;
        const before = state.inbound.length;
        adopt(body);
        if (state.inbound.length !== before) render();
        else renderThreadList();
        backoff = 1000;
      } catch (e) {
        if (!polling) break;
        if (e instanceof ApiError && e.status === 401) { signOut(); return; }
        // Anything else — offline, proxy hiccup — is temporary. Back off rather than spin.
        await new Promise((r) => setTimeout(r, backoff));
        backoff = Math.min(backoff * 2, 30000);
      }
    }
  }

  // ---- wiring -------------------------------------------------------------------------------

  function wire() {
    $("signin-btn").onclick = () => login();
    $("signout-btn").onclick = () => signOut();

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

    window.addEventListener("popstate", () => {
      if (state.openPeer) closeThread();
    });

    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && state.openPeer) closeThread();
    });

    // Coming back to a backgrounded tab should show current mail, not a stale view.
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible" && getToken() && state.me) {
        loadInbox().then(render).catch(() => {});
      }
    });

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
    render();

    try {
      await loadIdentities();
      if (!state.me) {
        $("signin-note").textContent =
          "This account has no mailboxes yet. Create one with `pigeonpost postbox new`.";
        render();
        return;
      }
      renderMe();
      await loadAll();
      startPolling();
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
