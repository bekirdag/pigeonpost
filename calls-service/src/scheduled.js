// Pigeonpost calls — scheduled meeting rooms (Phase 3B, SDS §14B.7, docs/calendar.md §7–§8).
//
// A scheduled meeting has a 10-character code reserved by the postbox for the whole series. The code
// is a locator, not a credential: every decision — may this person join, directly or by knocking,
// with which role, until when — comes from the postbox (`POST /internal/v1/calendar/meeting-access`,
// this service's own token). When the postbox cannot be asked, new admission fails closed.
//
// Rooms are ordinary rooms with `room.scheduled` set, keyed by (meeting_id, occurrence_id): one live
// room per occurrence, never merged across occurrences. They are not registered under any 6-character
// code, so the ad-hoc meeting-link routes (`/v1/meetings/{code}`, `/v1/knocks`) never find them.
//
// Differences from an ad-hoc room:
//   * being first in the room grants nothing; roles (host, cohost, attendee, guest) are the postbox's;
//   * only admitters may answer a knock, only hosts and co-hosts may send `mute`, only the host may
//     end or extend the call;
//   * the room closes at the occurrence's `closes_at` (extensions move it);
//   * every two minutes each member and invited guest in the room is rechecked; a definite "no"
//     removes them. While the postbox cannot be asked, people stay (the window bounds access);
//   * capacity is the meeting's own: a mesh meeting never grows past six or onto the media server.
//
// Logs carry counts and outcomes only: no tokens, tickets, addresses or IP addresses.

import crypto from "node:crypto";

export const SCHEDULED_CODE_ALPHABET = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const SCHEDULED_CODE = /^[ABCDEFGHJKMNPQRSTUVWXYZ23456789]{10}$/;
// The postbox's occurrence ids are `occ_<event suffix>_<recurrence key>` (docs/calendar.md §1).
const OCCURRENCE_ID = /^occ_[A-Za-z0-9_]{1,80}$/;
// A guest join ticket from the postbox: opaque, printable, bounded.
const TICKET = /^[\x21-\x7e]{8,1024}$/;

export const SCHEDULED_LIMITS = {
  recheckMs: 120_000,
  joinsPerAccountMinute: 30,
};

// "abcde-fghjk" → "ABCDEFGHJK"; anything that is not a well-formed scheduled code → null.
export function normalizeScheduledCode(value) {
  const code = String(value || "").replace(/-/g, "").toUpperCase();
  return SCHEDULED_CODE.test(code) ? code : null;
}

export function createScheduledMeetings(ctx) {
  const {
    cfg, internal, HttpError, LIMITS, now, norm, liveSession, mailboxFor, sessionInRoom, limited,
    makeRoom, addParticipant, reportCall, sessions, send, endRoom, roomView, admitted, participantName,
    roomsByCode, removeParticipant, resolveKnock, broadcast, clientIp, cleanName, authenticate,
    participantFor,
  } = ctx;
  const recheckMs = Number(cfg.scheduledRecheckMs) > 0 ? Number(cfg.scheduledRecheckMs) : SCHEDULED_LIMITS.recheckMs;

  const live = new Map(); // meeting_id \n occurrence_id -> room
  const inflight = new Map(); // same key -> promise (serializes room creation per occurrence)

  // ---- helpers ----------------------------------------------------------------------------------

  // Code guessing is bounded per IP address. The check happens before the postbox is asked; a miss
  // (nothing open behind that code, for this caller) is what spends the budget.
  const misses = new Map(); // ip -> number[] (timestamps)
  function missBlocked(req) {
    const ip = clientIp(req), t = now();
    const recent = (misses.get(ip) || []).filter((x) => t - x < 600_000);
    if (recent.length) misses.set(ip, recent); else misses.delete(ip);
    return recent.length >= LIMITS.missedCodesPerIp10Min;
  }
  function recordMiss(req) {
    const ip = clientIp(req);
    const list = misses.get(ip) || [];
    list.push(now());
    misses.set(ip, list);
    if (misses.size > 50_000) misses.delete(misses.keys().next().value);
  }

  async function ask(body) {
    if (!internal.configured || typeof internal.meetingAccess !== "function") return null;
    return internal.meetingAccess(body);
  }

  const memberActor = (mailboxes) => ({
    kind: "member",
    mailboxes: mailboxes.map((m) => ({ address: m.address, ...(m.handle ? { handle: m.handle } : {}) })),
  });

  function liveRoom(key) {
    const room = live.get(key);
    if (!room) return null;
    if (room.state === "ended") { live.delete(key); return null; }
    return room;
  }

  function occurrencesOf(v) {
    if (v.occurrences && v.occurrences.length) return v.occurrences;
    if (!v.occurrence_id) return [];
    return [{ occurrence_id: v.occurrence_id, starts_at: v.starts_at, ends_at: v.ends_at, opens_at: v.opens_at, closes_at: v.closes_at }];
  }

  function denial(v, anonymous) {
    // Somebody holding only the link learns nothing beyond "not available".
    if (anonymous) return new HttpError(404, "unavailable");
    switch (v.reason) {
      case "choose_occurrence": {
        const err = new HttpError(409, "choose_occurrence");
        err.extra = { occurrences: occurrencesOf(v) };
        return err;
      }
      case "window_closed": {
        const err = new HttpError(403, "window_closed");
        err.extra = { occurrences: occurrencesOf(v) };
        return err;
      }
      case "not_invited": return new HttpError(403, "not_invited");
      case "declined": return new HttpError(403, "declined");
      case "cancelled": return new HttpError(403, "event_cancelled");
      case "ticket_invalid": return new HttpError(403, "ticket_invalid");
      default: return new HttpError(404, "unavailable");
    }
  }

  function mayAdmit(room, p) {
    // The postbox computes this permission from the current policy and selected member.
    // A cached cohost role or address list must never override its explicit denial.
    return Boolean(p && p.sched && p.sched.kind === "member" && p.mailbox
      && ["host", "cohost", "attendee"].includes(p.sched.role) && p.sched.can_admit === true);
  }

  function updateFromVerdict(room, v) {
    const s = room.scheduled;
    if (v.admitters) s.admitters = v.admitters;
    if (v.policy) s.policy = { ...v.policy, transport: s.policy.transport }; // transport is fixed per occurrence
    if (v.closes_at && v.closes_at !== s.closes_at) {
      s.closes_at = v.closes_at;
      armClose(room);
      broadcast(room, "scheduled_window", { room: room.id, closes_at: s.closes_at });
    }
  }

  function armClose(room) {
    const s = room.scheduled;
    clearTimeout(s.closeTimer);
    const delay = Math.min(Math.max(0, s.closes_at * 1000 - now()), 2_147_000_000);
    s.closeTimer = setTimeout(() => endRoom(room, "window_closed"), delay);
    s.closeTimer.unref?.();
    room.timers.push(s.closeTimer);
  }

  function createRoom(key, v) {
    const room = makeRoom("video", "mesh");
    // makeRoom registered an ad-hoc 6-character code; a scheduled room must not be reachable by one.
    roomsByCode.delete(room.code);
    room.code = v.code || room.code;
    room.state = "waiting"; // becomes "active" with its first participant
    room.caller = v.organizer ? { ...v.organizer } : null; // call history: from = the organizer
    room.targets = [];
    room.scheduled = {
      key, meeting_id: v.meeting_id, event_id: v.event_id, occurrence_id: v.occurrence_id,
      closes_at: v.closes_at, admitters: v.admitters || [], policy: { ...v.policy },
      closeTimer: null, rechecking: false,
    };
    armClose(room);
    const tick = setInterval(() => { recheckRoom(room).catch(() => {}); }, recheckMs);
    tick.unref?.();
    room.timers.push(tick); // endRoom clears it (clearTimeout clears intervals too)
    live.set(key, room);
    console.log(`calls: scheduled room=${room.id.slice(0, 6)} opened`);
    return room;
  }

  // Serialize per occurrence: concurrent first joins get one room.
  async function withRoom(v, fn) {
    const key = v.meeting_id + "\n" + v.occurrence_id;
    const prev = inflight.get(key) || Promise.resolve();
    const run = prev.catch(() => {}).then(() => {
      const room = liveRoom(key) || createRoom(key, v);
      try {
        return fn(room);
      } catch (err) {
        // Nobody got in and nobody waits: do not leave an empty room open until the window ends.
        if (room.state === "waiting" && !room.participants.size && !room.knocks.size) endRoom(room, "empty");
        throw err;
      }
    });
    const settled = run.catch(() => {});
    inflight.set(key, settled);
    settled.then(() => { if (inflight.get(key) === settled) inflight.delete(key); });
    return run;
  }

  // ---- capacity (called from admissionProblem for scheduled rooms) ------------------------------

  function capacityProblem(room) {
    const s = room.scheduled;
    const limit = s.policy.transport === "sfu"
      ? Math.min(s.policy.participant_limit, LIMITS.participants)
      : Math.min(s.policy.participant_limit, cfg.meshMax);
    return room.participants.size >= limit ? "room_full" : null;
  }

  // ---- GET /v1/meetings/scheduled/{code} --------------------------------------------------------

  async function status(req, codeValue, params) {
    const header = String(req.headers.authorization || "");
    const member = header.startsWith("Bearer ") ? await authenticate(req) : null;
    if (missBlocked(req)) throw new HttpError(429, "rate_limited");
    const code = normalizeScheduledCode(codeValue);
    const occurrenceId = params.get("occurrence_id");
    if (occurrenceId && !OCCURRENCE_ID.test(occurrenceId)) throw new HttpError(400, "bad_occurrence");

    if (!member || member.guest) {
      if (!code) { recordMiss(req); return { state: "unavailable", can_knock: false }; }
      const v = await ask({ code, actor: { kind: "anonymous" }, purpose: "status" });
      if (!v) throw new HttpError(503, "calendar_unavailable");
      const canKnock = v.allowed && v.admission === "knock";
      if (!canKnock) { recordMiss(req); return { state: "unavailable", can_knock: false }; }
      return { state: "open", can_knock: true };
    }

    const unavailable = { state: "unavailable", role: "none", admission: "deny", occurrences: [], room: null };
    if (!code) { recordMiss(req); return unavailable; }
    let mailboxes = member.mailboxes;
    const as = params.get("as");
    if (as) {
      const m = mailboxFor(member, as);
      if (!m) throw new HttpError(403, "not_your_mailbox");
      mailboxes = [m];
    }
    const v = await ask({ code, ...(occurrenceId ? { occurrence_id: occurrenceId } : {}), actor: memberActor(mailboxes), purpose: "status" });
    if (!v) throw new HttpError(503, "calendar_unavailable");
    if (v.allowed) {
      const room = liveRoom(v.meeting_id + "\n" + v.occurrence_id);
      return {
        state: "open", role: v.role, admission: v.admission, occurrences: occurrencesOf(v),
        room: room ? { participants: room.participants.size } : null,
      };
    }
    if (v.reason === "window_closed") return { state: "not_open", role: v.role, admission: v.admission, occurrences: occurrencesOf(v), room: null };
    if (v.reason === "choose_occurrence") return { state: "open", role: v.role, admission: v.admission, occurrences: occurrencesOf(v), room: null, choose_occurrence: true };
    if (v.reason === "unavailable") recordMiss(req);
    return { ...unavailable, reason: v.reason };
  }

  // ---- POST /v1/meetings/scheduled/{code}/join -------------------------------------------------

  async function join(req, auth, codeValue, body) {
    const session = liveSession(auth, body.session);
    if (sessionInRoom(session)) throw new HttpError(409, "already_in_call");
    if (missBlocked(req)) throw new HttpError(429, "rate_limited");
    const code = normalizeScheduledCode(codeValue);
    if (!code) { recordMiss(req); throw new HttpError(404, "unavailable"); }
    const occurrenceId = body.occurrence_id === undefined || body.occurrence_id === null ? null : String(body.occurrence_id);
    if (occurrenceId !== null && !OCCURRENCE_ID.test(occurrenceId)) throw new HttpError(400, "bad_occurrence");

    let actor, kind, chosen = null, typedName = "";
    if (!auth.guest) {
      kind = "member";
      if (body.as) {
        chosen = mailboxFor(auth, body.as);
        if (!chosen) throw new HttpError(403, "not_your_mailbox");
      }
      actor = memberActor(chosen ? [chosen] : auth.mailboxes);
      if (limited("sjoin:" + auth.account, SCHEDULED_LIMITS.joinsPerAccountMinute, 60_000)) throw new HttpError(429, "rate_limited");
    } else if (body.ticket !== undefined && body.ticket !== null && body.ticket !== "") {
      if (typeof body.ticket !== "string" || !TICKET.test(body.ticket)) throw new HttpError(400, "bad_ticket");
      kind = "guest";
      actor = { kind: "guest", ticket: body.ticket };
    } else {
      kind = "anonymous";
      typedName = cleanName(body.name);
      if (!typedName) throw new HttpError(400, "name_required");
      actor = { kind: "anonymous" };
    }
    if (kind !== "member" && limited("knock:" + clientIp(req), LIMITS.knocksPerIpHour, 3600_000)) throw new HttpError(429, "rate_limited");

    const v = await ask({ code, ...(occurrenceId ? { occurrence_id: occurrenceId } : {}), actor, purpose: "join" });
    if (!v) throw new HttpError(503, "calendar_unavailable");
    if (!v.allowed) {
      if (v.reason === "unavailable" || kind === "anonymous") recordMiss(req);
      throw denial(v, kind === "anonymous");
    }
    if (v.admission === "deny" || (kind === "anonymous" && v.admission !== "knock")) {
      if (kind === "anonymous") { recordMiss(req); throw new HttpError(404, "unavailable"); }
      throw new HttpError(403, "not_invited");
    }
    if (session.closed || sessionInRoom(session)) throw new HttpError(409, "already_in_call");
    if (!v.code) v.code = code;

    let mailbox = null, name;
    if (kind === "member") {
      mailbox = (v.participant && v.participant.address && mailboxFor(auth, v.participant.address)) || chosen || auth.mailboxes[0];
      name = mailbox.handle || mailbox.address;
    } else if (kind === "guest") {
      name = cleanName(v.participant && v.participant.name) || "Guest";
    } else {
      name = typedName;
    }
    const sched = {
      kind, role: kind === "anonymous" ? "none" : v.role, can_admit: kind === "member" && v.can_admit,
      attendee_id: (v.participant && v.participant.attendee_id) || null,
      verified: kind === "member" ? true : Boolean(v.participant && v.participant.verified && kind === "guest"),
    };

    return withRoom(v, (room) => {
      updateFromVerdict(room, v);
      if (session.closed || sessionInRoom(session)) throw new HttpError(409, "already_in_call");
      if (v.admission === "direct") {
        const p = addParticipant(room, session, { name, mailbox, guest: kind !== "member", e2ee: body.e2ee, sched });
        if (mailbox) reportCall(room, "join", { by: mailbox.address, participants: [...room.participants.values()].map(participantName) });
        admitted(room, p);
        return { ...roomView(room, p), state: "joined" };
      }
      return { state: "waiting", ...knockIn(room, session, auth, { name, mailbox, guest: kind !== "member", e2ee: body.e2ee, sched }) };
    });
  }

  function knockIn(room, session, auth, { name, mailbox, guest, e2ee, sched }) {
    if (room.knocks.size >= LIMITS.pendingKnocksPerRoom) throw new HttpError(429, "too_many_requests");
    for (const k of room.knocks.values()) if (k.sessionId === session.id) throw new HttpError(409, "already_waiting");
    const k = { id: crypto.randomUUID().replace(/-/g, ""), sessionId: session.id, account: auth.account, name, mailbox, guest, e2ee: e2ee === true, at: now(), timer: null, sched };
    room.knocks.set(k.id, k);
    // Nobody who may admit is here: the knock waits (bounded by knockMs); nothing admits it automatically.
    k.timer = setTimeout(() => resolveKnock(room, k, false, "timeout"), cfg.knockMs);
    k.timer.unref?.();
    broadcast(room, "knock", { room: room.id, knock: k.id, name: k.name, mailbox: k.mailbox, guest: k.guest, verified: sched.verified });
    return { room: room.id, knock: k.id };
  }

  // ---- the live authorization gate --------------------------------------------------------------

  function checkAnswer(room, decider) {
    if (!mayAdmit(room, decider)) throw new HttpError(403, "not_allowed");
  }

  function checkMute(from) {
    const role = from.sched && from.sched.kind === "member" ? from.sched.role : "none";
    if (role !== "host" && role !== "cohost") throw new HttpError(403, "not_allowed");
  }

  function hostOf(auth, room, body) {
    if (!room.scheduled) throw new HttpError(404, "not_found");
    const p = participantFor(room, auth, body.session);
    if (!p.sched || p.sched.kind !== "member" || p.sched.role !== "host" || !p.mailbox) throw new HttpError(403, "not_allowed");
    return p;
  }

  function end(auth, room, body) {
    hostOf(auth, room, body);
    endRoom(room, "ended_by_host");
    return { room: room.id, state: "ended" };
  }

  async function extend(auth, room, body) {
    const p = hostOf(auth, room, body);
    if (body.minutes !== 15 && body.minutes !== 30) throw new HttpError(400, "bad_minutes");
    const s = room.scheduled;
    const v = await ask({
      code: room.code, occurrence_id: s.occurrence_id, actor: memberActor([p.mailbox]),
      purpose: "extend", extend_minutes: body.minutes,
    });
    if (!v) throw new HttpError(503, "calendar_unavailable");
    if (!v.allowed) throw denial(v, false);
    if (room.state === "ended") throw new HttpError(404, "no_call");
    if (v.occurrence_id && v.occurrence_id !== s.occurrence_id) throw new HttpError(409, "occurrence_changed");
    if (v.role !== "host") throw new HttpError(403, "not_allowed");
    p.sched.role = v.role;
    updateFromVerdict(room, v);
    return { room: room.id, closes_at: s.closes_at };
  }

  // ---- revalidation -----------------------------------------------------------------------------

  // One participant: true = keep, false = definite denial (removed). Unavailable postbox → keep.
  async function recheckOne(room, p) {
    if (!p.sched || p.sched.kind === "anonymous") return true;
    let actor;
    if (p.sched.kind === "member" && p.mailbox) actor = memberActor([p.mailbox]);
    else if (p.sched.kind === "guest" && p.sched.attendee_id) actor = { kind: "guest", attendee_id: p.sched.attendee_id };
    else return true;
    const s = room.scheduled;
    const v = await ask({ code: room.code, occurrence_id: s.occurrence_id, actor, purpose: "recheck" });
    if (!v || room.state === "ended" || !room.participants.has(p.id)) return true;
    if (!v.allowed || v.admission === "deny" || (v.occurrence_id && v.occurrence_id !== s.occurrence_id)) {
      send(sessions.get(p.sessionId), "ended", { room: room.id, reason: "revoked" });
      removeParticipant(room, p, "revoked");
      return false;
    }
    p.sched.role = p.sched.kind === "member" ? v.role : p.sched.role;
    p.sched.can_admit = p.sched.kind === "member" && v.can_admit;
    updateFromVerdict(room, v);
    return true;
  }

  async function recheckRoom(room) {
    if (!room.scheduled || room.state === "ended" || room.scheduled.rechecking) return;
    room.scheduled.rechecking = true;
    try {
      const people = [...room.participants.values()];
      const results = await Promise.all(people.map((p) => recheckOne(room, p).then((keep) => keep, () => true)));
      const removed = results.filter((x) => !x).length;
      if (removed) console.log(`calls: scheduled room=${room.id.slice(0, 6)} recheck removed=${removed}`);
    } finally {
      room.scheduled.rechecking = false;
    }
  }

  // A reload taking its place back (resume): the person must still be allowed in.
  async function recheck(room, p) {
    if (!room.scheduled) return;
    if (!(await recheckOne(room, p))) throw new HttpError(403, "revoked");
  }

  // ---- what each viewer sees --------------------------------------------------------------------

  function view(room, you) {
    const s = room.scheduled;
    const canAdmit = mayAdmit(room, you);
    const out = {
      event_id: s.event_id, occurrence_id: s.occurrence_id,
      role: you && you.sched ? you.sched.role : "none", can_admit: canAdmit, closes_at: s.closes_at,
    };
    // Somebody arriving who may admit sees who is already waiting.
    if (canAdmit) out.knocks = [...room.knocks.values()].map((k) => ({ knock: k.id, name: k.name, mailbox: k.mailbox, guest: k.guest, verified: Boolean(k.sched && k.sched.verified) }));
    return out;
  }

  return { status, join, capacityProblem, checkAnswer, checkMute, end, extend, recheck, recheckRoom, view, live };
}
