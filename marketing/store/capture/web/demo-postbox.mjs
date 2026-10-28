// A stand-in postbox for the store screenshots: the invented people of seed/demo-world.json, each a
// mailbox with a /handle, and one scheduled meeting ("Launch sync") they may all join. Only the
// routes the call page and the call service ask for exist. Tokens are "tok-<name>".
import http from "node:http";

export const PEOPLE = ["maya", "leo", "ana", "priya", "sam", "noor", "kai", "tomas"];
export const NAMES = { maya: "Maya", leo: "Leo", ana: "Ana", priya: "Priya", sam: "Sam", noor: "Noor", kai: "Kai", tomas: "Tomás" };
export const mailbox = (n) => ({ address: "/k/demo" + n, handle: "/" + n });
export const MEETING = { code: "PNXSYNCMAY", event: "evt_launchsync", occurrence: "occ_launchsync_1", meeting: "mtg_launchsync" };

export function createDemoPostbox({ serviceToken, transport = "mesh" }) {
  const byToken = Object.fromEntries(PEOPLE.map((n) => ["tok-" + n, mailbox(n)]));
  const byAddress = Object.fromEntries(PEOPLE.map((n) => [mailbox(n).address, n]));
  const cors = { "access-control-allow-origin": "*", "access-control-allow-headers": "authorization, content-type, x-pigeonpost-identity, accept", "access-control-allow-methods": "GET, POST, DELETE, OPTIONS" };
  const read = (req) => new Promise((r) => { let d = ""; req.on("data", (c) => (d += c)); req.on("end", () => { try { r(d ? JSON.parse(d) : {}); } catch { r({}); } }); });
  const server = http.createServer(async (req, res) => {
    const out = (s, b) => { res.writeHead(s, { "content-type": "application/json", ...cors }); res.end(JSON.stringify(b)); };
    if (req.method === "OPTIONS") { res.writeHead(204, cors); return res.end(); }
    const u = new URL(req.url, "http://x");
    if (u.pathname.startsWith("/internal/")) {
      if (req.headers.authorization !== "Bearer " + serviceToken) return out(401, {});
      const body = await read(req);
      if (u.pathname === "/internal/v1/call-admission") {
        const n = byAddress[body.to] || PEOPLE.find((p) => mailbox(p).handle === body.to);
        return out(200, { decision: n ? "ring" : "block", targets: n ? [mailbox(n).address] : [], reason: "synthetic_fixture" });
      }
      if (u.pathname === "/internal/v1/call-events") return out(200, { ok: true });
      if (u.pathname === "/internal/v1/calendar/meeting-access") {
        const now = Math.floor(Date.now() / 1000);
        const occ = { occurrence_id: MEETING.occurrence, starts_at: now - 120, ends_at: now + 2700, opens_at: now - 900, closes_at: now + 3600 };
        const boxes = Array.isArray(body.actor?.mailboxes) ? body.actor.mailboxes : [];
        const n = boxes.map((m) => byAddress[m?.address]).find(Boolean);
        if (!n) return out(200, { allowed: false, reason: "not_invited" });
        if (body.code !== MEETING.code) return out(200, { allowed: false, reason: "unavailable" });
        if (body.occurrence_id && body.occurrence_id !== MEETING.occurrence) return out(200, { allowed: false, reason: "choose_occurrence", occurrences: [occ] });
        const host = n === "priya";
        return out(200, { allowed: true, meeting_id: MEETING.meeting, event_id: MEETING.event, code: MEETING.code, ...occ,
          role: host ? "host" : "attendee", admission: "direct", can_admit: host,
          participant: { ...mailbox(n), attendee_id: host ? null : "att" + n, name: NAMES[n], verified: true },
          organizer: mailbox("priya"), policy: { waiting: "nobody", admit: "hosts", allow_knock: false, participant_limit: 30, transport },
          admitters: [mailbox("priya").address], policy_version: "demo_v1" });
      }
      return out(404, {});
    }
    const me = byToken[(req.headers.authorization || "").replace(/^Bearer /, "")];
    if (!me) return out(401, { error: "unauthorized" });
    if (u.pathname === "/v1/capabilities") return out(200, { features: ["calendar"] });
    if (u.pathname === "/v1/identities") return out(200, { identities: [{ address: me.address, label: null }] });
    if (u.pathname === "/v1/whoami") return out(200, me);
    return out(404, { error: "not_found" });
  });
  return { server };
}
