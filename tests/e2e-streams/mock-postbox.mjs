// A stand-in for the postbox's stream routes (the real ones are tested in Rust). Tokens are
// "tok-<name>", mailboxes "/k/<name>". One group; everyone listed is an active member.
import http from "node:http";
import crypto from "node:crypto";
const GID = "grp_" + "g".repeat(26);
export function createMockPostbox({ members = {}, maxViewers = 200 } = {}) {
  const m = new Map(Object.entries(members)); // mailbox -> role
  const streams = new Map();
  const b32 = () => [...crypto.randomBytes(26)].map((x) => "abcdefghijklmnopqrstuvwxyz234567"[x % 32]).join("");
  const who = (req) => { const t = (req.headers.authorization || "").replace(/^Bearer /, ""); return t.startsWith("tok-") ? "/k/" + t.slice(4) : null; };
  const active = (mb) => m.has(mb);
  const role = (s, mb) => (s.host === mb ? "host" : s.stage.get(mb) || "viewer");
  const view = (s) => ({ id: s.id, group_id: GID, title: s.title, sources: s.sources, by: s.host, state: s.state, started_at: s.started_at, ends_by: s.started_at + 14400, viewers: s.viewers, stage: [...s.stage].map(([address, state]) => ({ address, state, by: s.host })), max_viewers: maxViewers, revision: s.revision });
  const read = (req) => new Promise((r) => { let d = ""; req.on("data", (c) => (d += c)); req.on("end", () => { try { r(d ? JSON.parse(d) : {}); } catch { r({}); } }); });
  const log = [];
  const server = http.createServer(async (req, res) => {
    const cors = { "access-control-allow-origin": "*", "access-control-allow-headers": "authorization, content-type, x-pigeonpost-identity, accept", "access-control-allow-methods": "GET, POST, DELETE, OPTIONS" };
    const send = (status, body) => { res.writeHead(status, { "content-type": "application/json", ...cors }); res.end(JSON.stringify(body)); };
    if (req.method === "OPTIONS") { res.writeHead(204, cors); return res.end(); }
    const u = new URL(req.url, "http://x"); const p = u.pathname; const body = req.method === "POST" ? await read(req) : {};
    log.push(req.method + " " + p);
    if (p.startsWith("/internal/")) {
      if (req.headers.authorization !== "Bearer svc_calls_test") return send(401, {});
      const s = streams.get(body.stream_id);
      if (p === "/internal/v1/stream-admission") {
        if (!s) return send(200, { allowed: false, reason: "no_stream" });
        const mb = body.from.mailbox;
        if (!active(mb)) return send(200, { allowed: false, reason: "not_member" });
        if (s.state !== "live") return send(200, { allowed: false, reason: "stream_ended" });
        const attested = s.host === mb ? s.hostKey === body.public_key : (s.keys.get(mb) || new Set()).has(body.public_key);
        return send(200, { allowed: true, role: role(s, mb), group_id: GID, host: s.host, host_key: s.hostKey, sources: s.sources, state: "live", ends_by: s.started_at + 14400, max_viewers: maxViewers, key_attested: attested });
      }
      if (p === "/internal/v1/stream-check") {
        if (!s) return send(200, { state: "ended", roles: {} });
        s.viewers = body.viewers; s.peak = Math.max(s.peak || 0, body.viewers);
        const roles = {};
        for (const mb of body.members) roles[mb] = s.state === "live" && active(mb) ? role(s, mb) : null;
        return send(200, { state: s.state, reason: s.reason || null, roles, host_key: s.hostKey, revision: s.revision });
      }
      if (p === "/internal/v1/stream-end") { if (s) { s.state = "ended"; s.reason = body.reason; } return send(200, { ok: true, state: s && s.state }); }
      return send(404, {});
    }
    if (p.startsWith("/test/")) {
      if (p === "/test/remove") { m.delete(body.mailbox); for (const s of streams.values()) { s.keys.delete(body.mailbox); s.stage.delete(body.mailbox); } return send(200, { ok: true }); }
      if (p === "/test/state") return send(200, { streams: [...streams.values()].map(view), log: log.slice(-50) });
      return send(404, {});
    }
    const me = who(req);
    if (!me) return send(401, { error: "unauthorized" });
    if (p === "/v1/identities") return send(200, { identities: [{ address: me }] });
    if (p === "/v1/whoami") return send(200, { address: me, handle: "/" + me.slice(3) + "/main" });
    if (!active(me)) return send(403, { error: "not_member" });
    if (p === `/v1/groups/${GID}/streams`) {
      const live = [...streams.values()].find((s) => s.state === "live");
      if (req.method === "GET") return send(200, { streams: live ? [view(live)] : [] });
      if (live) return send(409, { error: "stream_in_progress", stream_id: live.id });
      const s = { id: "str_" + b32(), title: body.title, sources: body.sources, host: me, hostKey: body.public_key, state: "live", started_at: Math.floor(Date.now() / 1000), viewers: 0, stage: new Map(), keys: new Map(), revision: 1 };
      streams.set(s.id, s);
      return send(201, { stream: view(s), role: "host", host_key: s.hostKey, media: { join: `/calls/v1/streams/${s.id}/join` } });
    }
    const sm = /^\/v1\/streams\/(str_[a-z2-7]{26})(\/\w+)?$/.exec(p);
    if (sm) {
      const s = streams.get(sm[1]);
      if (!s) return send(404, { error: "no_stream" });
      const rest = sm[2] || "";
      if (rest === "" && req.method === "GET") return send(200, { stream: view(s), role: role(s, me) });
      if (rest === "" && req.method === "DELETE") { s.state = "ended"; s.reason = "stopped"; return send(200, { stream: view(s) }); }
      if (rest === "/watch") {
        if (s.state !== "live") return send(410, { error: "stream_ended" });
        if (s.host === me) { s.hostKey = body.public_key; s.revision++; } else { if (!s.keys.has(me)) s.keys.set(me, new Set()); s.keys.get(me).add(body.public_key); }
        return send(200, { stream: view(s), role: role(s, me), host_key: s.hostKey, media: { join: `/calls/v1/streams/${s.id}/join` } });
      }
      if (rest === "/audience") {
        if (s.host !== me) return send(403, { error: "not_host" });
        return send(200, { stream_id: s.id, revision: s.revision, audience: [...s.keys].filter(([mb]) => active(mb)).map(([address, keys]) => ({ address, role: role(s, address), keys: [...keys] })) });
      }
      if (rest === "/stage") {
        const target = body.address;
        if (target === me && s.host !== me) { if (body.on) { if (!s.stage.has(me)) return send(403, { error: "not_invited" }); s.stage.set(me, "speaker"); } else s.stage.delete(me); }
        else { if (s.host !== me) return send(403, { error: "not_host" }); if (body.on) s.stage.set(target, s.stage.get(target) || "invited"); else s.stage.delete(target); }
        s.revision++;
        return send(200, { stream: view(s), address: target, role: role(s, target) });
      }
    }
    return send(404, { error: "not_found" });
  });
  return { server, streams, members: m, GID, log };
}
