// The web inbox signed in as the invented /maya, for the store screenshots. Every postbox request is
// answered here from seed/demo-world.json (passed in as window.__PP_WORLD); nothing reaches a real
// postbox, and capture pages also abort any request that is not to 127.0.0.1.
//
// Meeting notes: the page registers its own notes key (PUT /v1/meetings/keys/…); the harness seals
// the "Launch sync" brief to that key with meeting-intel's own sealBrief (window.__ppSeal, exposed by
// the harness) and this stand-in hands the ciphertext back, so the real reader opens it with this
// device's key exactly as it would a real meeting's.
(function () {
  const W = window.__PP_WORLD;
  const ME = { address: "/k/demomaya", handle: "/maya" };
  localStorage.setItem("ppi_token", "tok-maya");
  try { localStorage.setItem("ppi_theme", "dark"); } catch (_) {}
  const now = Math.floor(Date.now() / 1000);
  const keyOf = (h) => "/k/demo" + h.replace(/[^a-z]/g, "");
  const messages = [];
  let i = 0;
  for (const c of W.conversations) {
    if (c.kind === "group") continue;
    const agent = c.kind === "agent";
    const unread = c.unread || 0;
    const base = { from: keyOf(c.with), sender_handle: c.with, peer: c.with, peer_handle: c.with, direction: "in", sender_known: true, sender_tier: "handle",
      sender_standing: "good", autonomy: agent ? "auto" : "review", thread_id: "t-" + i, untrusted: false };
    for (let u = 0; u < Math.max(1, unread); u++) {
      messages.push({ ...base, message_id: `m${i}_${u}`, body: c.last, received_at: now + c.min * 60 - (unread - u) * 30, read: u >= unread });
    }
    i++;
  }
  const threads = messages.filter((m, k, all) => all.findIndex((x) => x.thread_id === m.thread_id) === k)
    .map((m) => ({ thread_id: m.thread_id, peer: m.peer, title: null, is_default: true, created_at: m.received_at - 600, last_at: m.received_at, archived: false }));
  const contacts = W.conversations.filter((c) => c.kind !== "group").map((c) => ({ peer: c.with, alias: null, admission: "allow", autonomy: c.kind === "agent" ? "auto" : "review", allowed_verbs: [] }));
  const meetingId = "mtg_launchsync0000000000";
  let notesKey = null;
  const res = (body, status = 200) => Promise.resolve(new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } }));
  const real = window.fetch.bind(window);
  window.fetch = function (url, opts = {}) {
    const u = new URL(String(url), location.href);
    if (u.origin === location.origin && !u.pathname.startsWith("/v1/")) return real(url, opts);
    const p = u.pathname, method = (opts.method || "GET").toUpperCase();
    if (p === "/v1/identities") return res({ identities: [{ address: ME.address, label: "Personal" }] });
    if (p === "/v1/whoami") return res(ME);
    if (p === "/v1/capabilities") return res({ features: ["meeting_summaries", "avatars"] });
    if (p === "/v1/events") return new Promise(() => {});
    if (p === "/v1/inbox") { if (u.searchParams.has("wait")) return new Promise(() => {}); return res({ messages, policy: { accept_all: true, auto_accept_known: true } }); }
    if (p === "/v1/contacts") return res({ contacts, policy: { accept_all: true, auto_accept_known: true }, vocabulary: { grantable: ["report_status", "answer_question", "read_file"], never_auto: ["spend", "delete_files"] } });
    if (p === "/v1/archive") return res({ archived: [] });
    if (p === "/v1/threads") return res({ threads });
    if (p === "/v1/ack") return res({ ok: true });
    if (p === "/v1/me/handles") return res({ handles: [] });
    if (p === "/v1/avatars") return res({ avatars: {} });
    if (p.startsWith("/v1/meetings/keys/") && method === "PUT") {
      notesKey = { key_id: decodeURIComponent(p.slice("/v1/meetings/keys/".length)), public_key: JSON.parse(opts.body).public_key };
      return res({ registered: true });
    }
    if (p === "/v1/meetings") return res({ meetings: [{ id: meetingId, kind: "scheduled", state: "ready", created_at: now - 3 * 3600, expires_at: now + 90 * 86400, host: "/priya", as: ME.address }] });
    if (p === "/v1/meetings/" + meetingId && method === "GET") {
      if (!notesKey) return res({ error: "not_found" }, 404);
      return window.__ppSeal(meetingId, notesKey).then((sealed) => res({ id: meetingId, state: "ready", alg: sealed.alg, iv: sealed.iv, ciphertext: sealed.ciphertext, keys: sealed.wraps }));
    }
    return res({ error: "not_found" }, 404);
  };
})();
