import http from "node:http";
const ACCOUNTS = { "tok-alice": [{ address: "/k/ec55xq18rbwdnwpsvkht07az40", handle: "/alice" }], "tok-bob": [{ address: "/k/rywv63mb6h0hhrpfjewrrttspw", handle: "/bob" }] };
const serviceToken = process.env.PIGEONPOST_TEST_INTERNAL_TOKEN || "";
const alice = ACCOUNTS["tok-alice"][0], bob = ACCOUNTS["tok-bob"][0];
const firstOccurrence = "occ_nativeCalendar_first", secondOccurrence = "occ_nativeCalendar_second";
export const server = http.createServer(async (req, res) => {
  const owned = ACCOUNTS[(req.headers.authorization || "").replace(/^Bearer /, "")];
  const u = new URL(req.url, "http://x");
  const out = (s, b) => { res.writeHead(s, { "content-type": "application/json" }); res.end(JSON.stringify(b)); };
  if (u.pathname.startsWith("/internal/")) {
    if (!serviceToken || req.headers.authorization !== "Bearer " + serviceToken || req.method !== "POST") return out(401, { error: "unauthorized" });
    let body;
    try {
      let text = "";
      for await (const chunk of req) { text += chunk; if (text.length > 65536) return out(413, {}); }
      body = JSON.parse(text);
      if (!body || typeof body !== "object" || Array.isArray(body)) return out(400, {});
    } catch { return out(400, {}); }
    if (u.pathname === "/internal/v1/call-admission") {
      const target = [alice, bob].find(person => person.address === body.to || person.handle === body.to);
      return out(200, { decision: target ? "ring" : "block", targets: target ? [target.address] : [], reason: "synthetic_fixture" });
    }
    if (u.pathname === "/internal/v1/call-events") return out(200, { ok: true });
    if (u.pathname === "/internal/v1/calendar/meeting-access") {
      const now = Math.floor(Date.now() / 1000);
      const occurrences = [firstOccurrence, secondOccurrence].map(occurrence_id => ({ occurrence_id, starts_at: now - 60, ends_at: now + 1800, opens_at: now - 600, closes_at: now + 3600 }));
      const mailboxes = Array.isArray(body.actor?.mailboxes) ? body.actor.mailboxes : [];
      const member = body.actor?.kind === "member" && [alice, bob].find(person => mailboxes.some(mailbox => mailbox?.address === person.address));
      if (!member) return out(200, { allowed: false, reason: "not_invited" });
      if (body.code === "BCDEFGHJKM") return out(200, { allowed: false, reason: "window_closed", occurrences });
      if (body.code !== "ABCDEFGHJK") return out(200, { allowed: false, reason: "unavailable" });
      const occurrence = occurrences.find(item => item.occurrence_id === body.occurrence_id);
      if (!occurrence) return out(200, { allowed: false, reason: "choose_occurrence", occurrences });
      const host = member === alice;
      return out(200, { allowed: true, meeting_id: "mtg_nativeCalendar", event_id: "evt_nativeCalendar", code: body.code, ...occurrence,
        role: host ? "host" : "attendee", admission: host ? "direct" : "knock", can_admit: host,
        participant: { ...member, attendee_id: host ? null : "att_nativeBob", name: host ? "Alice" : "Bob", verified: true },
        organizer: alice, policy: { waiting: "everyone_except_hosts", admit: "hosts", allow_knock: false, participant_limit: 6, transport: "mesh" },
        admitters: [], policy_version: "synthetic_v1" });
    }
    return out(404, {});
  }
  if (!owned) return out(401, { error: "unauthorized" });
  if (u.pathname === "/v1/capabilities") return out(200, { features: ["calendar"] });
  if (u.pathname === "/v1/identities") return out(200, { identities: owned.map(m => ({ address: m.address, label: null })) });
  if (u.pathname === "/v1/whoami") { const m = owned.find(x => x.address === u.searchParams.get("identity")); return m ? out(200, m) : out(403, {}); }
  if (u.pathname === "/v1/calendars" || u.pathname.startsWith("/v1/calendar/")) {
    const identity = u.searchParams.get("identity");
    if (!owned.some(mailbox => mailbox.address === identity) || req.headers["x-pigeonpost-identity"] !== identity) return out(403, { error: "forbidden" });
    if (u.pathname === "/v1/calendars") return out(200, { calendars: [{ id: "cal_native", name: "Synthetic calendar" }] });
    if (u.pathname === "/v1/calendar/settings") return out(200, { timezone: "UTC", week_start: 1, previews: false });
    if (u.pathname === "/v1/calendar/invitations") return out(200, { invitations: [] });
    if (u.pathname === "/v1/calendar/events") return out(200, { occurrences: [] });
    if (u.pathname === "/v1/calendar/events/evt_nativeCalendar") {
      const occurrence_id = u.searchParams.get("occurrence_id");
      if (![firstOccurrence, secondOccurrence].includes(occurrence_id)) return out(404, { error: "not_found" });
      const now = Math.floor(Date.now() / 1000), starts_at = now - 60, ends_at = now + 1800;
      const start = new Date(starts_at * 1000).toISOString().slice(0, 19), end = new Date(ends_at * 1000).toISOString().slice(0, 19);
      return out(200, { event: { id: "evt_nativeCalendar", revision: 1, status: "confirmed", title: "Synthetic calendar meeting", all_day: false, start, end, tzid: "UTC", starts_at, ends_at,
        my: identity === alice.address ? { role: "organizer" } : { role: "attendee", attendee_id: "att_nativeBob", rsvp: "accepted" },
        can_edit: identity === alice.address, meeting: { code: "ABCDEFGHJK", waiting: "everyone_except_hosts", admit: "hosts", transport: "mesh" } },
        occurrence: { occurrence_id, all_day: false, starts_at, ends_at, status: "confirmed" } });
    }
  }
  out(404, {});
}).listen(Number(process.argv[2] || 8792), "127.0.0.1");
