// A two-person stand-in for the postbox, for the call layout check: `?as=alice` or `?as=bob`, one
// conversation with the other. Only /calls/* reaches a server — the real call service.
(function () {
  "use strict";
  const q = new URLSearchParams(location.search);
  const as = q.get("as") === "bob" ? "bob" : "alice";
  const P = {
    alice: { token: "tok-alice", address: "/k/alice0000000000000000000a", handle: "/alice", peer: "/bob" },
    bob: { token: "tok-bob", address: "/k/bob00000000000000000000000b", handle: "/bob", peer: "/alice" },
  }[as];
  localStorage.setItem("ppi_token", P.token);
  const now = Math.floor(Date.now() / 1000);
  const messages = [{ message_id: "m1", from: "/k/x", sender_handle: P.peer, peer_handle: P.peer, direction: "in", autonomy: "review", read: true, untrusted: true, sender_known: true, sender_tier: "handle", thread_id: "t1", received_at: now - 60, body: "Shall we talk it through on a call?" }];
  const threads = [{ thread_id: "t1", peer: P.peer, title: null, is_default: true, created_at: now - 900, last_at: now - 60, archived: false }];
  const res = (body, status = 200) => Promise.resolve({ ok: status < 300, status, json: async () => JSON.parse(JSON.stringify(body)), blob: async () => new Blob([""]) });
  const real = window.fetch.bind(window);
  window.fetch = function (url, opts = {}) {
    const u = new URL(String(url), location.href);
    if (u.origin === location.origin && (u.pathname.startsWith("/calls/") || u.pathname.startsWith("/vendor/"))) return real(url, opts);
    const p = u.pathname;
    if (p === "/v1/identities") return res({ identities: [{ address: P.address, label: as }] });
    if (p === "/v1/whoami") return res({ address: P.address, handle: P.handle });
    if (p === "/v1/inbox") return u.searchParams.has("wait") ? new Promise(() => {}) : res({ messages });
    if (p === "/v1/contacts") return res({ contacts: [], vocabulary: { grantable: [], never_auto: [] } });
    if (p === "/v1/archive") return res({ archived: [] });
    if (p === "/v1/threads") return res({ threads });
    if (p === "/v1/ack") return res({ ok: true });
    if (p === "/v1/me/handles") return res({ handles: [] });
    return res({ error: "not_found" }, 404);
  };
})();
