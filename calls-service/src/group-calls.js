// Pigeonpost calls — calls from persistent groups (Phase 3a, SDS §4.7, docs/groups.md §6).
//
// A group call is an ordinary room with `room.group_id` set. Who may start, be rung and join is
// decided by the postbox (`POST /internal/v1/group-call-admission`, this service's own token), and
// rechecked at create, join and resume. When the postbox cannot be asked, group calls are refused:
// a group's member list is not something this service may guess.
//
// Differences from a one-to-one ring:
//   * every active member is rung (each according to their own call settings and blocks);
//   * answering does not stop the ring for the others — anyone may still join until the ring
//     times out, and any active member can join a running call without knocking;
//   * the room lives while at least one person is in it;
//   * one live room per group.
//
// The mesh cap (six) still applies. Meeting-link guests stay possible through the room's code,
// but admitting a guest never grants group membership.

const GROUP_ID = /^grp_[A-Za-z0-9]{1,60}$/;

export function validGroupId(value) {
  return typeof value === "string" && GROUP_ID.test(value);
}

export function createGroupCalls(ctx) {
  const {
    cfg, internal, HttpError, KINDS, LIMITS, now, norm,
    liveSession, mailboxFor, sessionInRoom, limited, makeRoom, addParticipant,
    reportCall, sessionsByName, sessions, send, stopRinging, endRoom, roomView, admitted,
    participantName,
  } = ctx;

  const active = new Map(); // group_id -> room

  function liveRoom(groupId) {
    const room = active.get(groupId);
    if (!room) return null;
    if (room.state === "ended") {
      active.delete(groupId);
      return null;
    }
    return room;
  }

  async function admission(groupId, from, kind, join) {
    if (!internal.configured || typeof internal.groupAdmit !== "function") {
      throw new HttpError(503, "group_calls_unavailable");
    }
    const verdict = await internal.groupAdmit({ group_id: groupId, from, kind, join });
    if (!verdict) throw new HttpError(503, "group_calls_unavailable");
    if (!verdict.allowed) throw new HttpError(403, "not_member");
    return verdict;
  }

  async function start(auth, body) {
    if (auth.guest) throw new HttpError(403, "members_only");
    const session = liveSession(auth, body.session);
    const kind = KINDS.has(body.kind) ? body.kind : null;
    if (!kind) throw new HttpError(400, "bad_kind");
    const groupId = body.group_id;
    if (!validGroupId(groupId)) throw new HttpError(400, "bad_group");
    const from = mailboxFor(auth, body.from || "");
    if (!from) throw new HttpError(403, "not_your_mailbox");
    if (sessionInRoom(session)) throw new HttpError(409, "already_in_call");
    if (limited("calls-min:" + auth.account, LIMITS.callsPerMinute, 60_000) || limited("calls-hour:" + auth.account, LIMITS.callsPerHour, 3600_000)) {
      throw new HttpError(429, "rate_limited");
    }
    const existing = liveRoom(groupId);
    if (existing) {
      // Only somebody the postbox lets in learns that a call is running.
      await admission(groupId, from, kind, true);
      const err = new HttpError(409, "call_in_progress");
      err.extra = { room: existing.id, kind: existing.kind, participants: existing.participants.size };
      throw err;
    }
    const verdict = await admission(groupId, from, kind, false);
    if (session.closed || sessionInRoom(session)) throw new HttpError(409, "already_in_call");
    const raced = liveRoom(groupId);
    if (raced) {
      const err = new HttpError(409, "call_in_progress");
      err.extra = { room: raced.id, kind: raced.kind, participants: raced.participants.size };
      throw err;
    }

    const room = makeRoom(kind);
    room.group_id = groupId;
    room.group_name = verdict.group_name;
    room.caller = { address: from.address, ...(from.handle ? { handle: from.handle } : {}) };
    const ringing = verdict.members.filter((m) => m.decision === "ring" && norm(m.address) !== norm(from.address));
    const silenced = verdict.members.filter((m) => m.decision === "silence");
    room.targets = ringing.map((m) => m.address);
    active.set(groupId, room);
    const caller = addParticipant(room, session, { name: from.handle || from.address, mailbox: from, e2ee: body.e2ee });
    ring(room, caller, ringing, silenced);
    return roomView(room, caller);
  }

  function ring(room, caller, members, silenced) {
    room.ring = { to: null, toName: null, group: true, from: caller.mailbox, ringing: new Set(), ringingFor: new Map(), decision: "ring" };
    reportCall(room, "ring");
    if (silenced.length) reportCall(room, "filtered", { to: silenced.map((m) => m.address) });
    for (const member of members) {
      const names = [norm(member.address)];
      if (member.handle) names.push(norm(member.handle));
      for (const name of names) {
        for (const target of sessionsByName.get(name) || []) {
          if (target.id === caller.sessionId || room.ring.ringing.has(target.id) || sessionInRoom(target)) continue;
          room.ring.ringing.add(target.id);
          room.ring.ringingFor.set(target.id, member.address);
          send(target, "incoming", {
            room: room.id, code: room.code, kind: room.kind, from: caller.mailbox,
            to: member.address, group_id: room.group_id, group_name: room.group_name,
          });
        }
      }
    }
    const t = setTimeout(() => {
      if (!room.ring || room.state === "ended") return;
      if (room.state === "ringing") {
        reportCall(room, "missed");
        stopRinging(room, "no_answer");
        if (room.participants.size < 2) endRoom(room, "no_answer");
      } else {
        stopRinging(room, "timeout");
      }
    }, cfg.ringMs);
    t.unref?.();
    room.ring.timer = t;
  }

  // Stop ringing every session that rang for `address` (one person answered or declined on one of
  // their devices), except `keep`.
  function quietMember(room, address, reason, keep) {
    if (!room.ring) return;
    for (const [sid, addr] of [...room.ring.ringingFor]) {
      if (norm(addr) !== norm(address)) continue;
      room.ring.ringing.delete(sid);
      room.ring.ringingFor.delete(sid);
      if (sid !== keep) send(sessions.get(sid), "ring_ended", { room: room.id, reason });
    }
  }

  function join(room, session, mailbox, e2ee) {
    if (sessionInRoom(session)) throw new HttpError(409, "already_in_call");
    // Capacity (mesh cap, or the media server with frame E2EE when enabled) is the host's
    // admission rule, applied inside addParticipant (Phase 6a).
    const p = addParticipant(room, session, { name: mailbox.handle || mailbox.address, mailbox, e2ee });
    quietMember(room, mailbox.address, "answered_elsewhere", session.id);
    room.state = "active";
    reportCall(room, "answer", { by: mailbox.address, participants: [...room.participants.values()].map(participantName) });
    admitted(room, p);
    return roomView(room, p);
  }

  async function accept(auth, room, body) {
    if (auth.guest) throw new HttpError(403, "members_only");
    const session = liveSession(auth, body.session);
    if (!room.ring || !room.ring.ringing.has(session.id)) throw new HttpError(409, "not_ringing");
    const mailbox = mailboxFor(auth, room.ring.ringingFor.get(session.id) || "");
    if (!mailbox) throw new HttpError(403, "not_your_call");
    await admission(room.group_id, mailbox, room.kind, true);
    if (room.state === "ended") throw new HttpError(404, "no_call");
    return join(room, session, mailbox, body.e2ee);
  }

  function decline(auth, room, body) {
    const session = liveSession(auth, body.session);
    if (!room.ring || !room.ring.ringing.has(session.id)) throw new HttpError(409, "not_ringing");
    const address = room.ring.ringingFor.get(session.id);
    const reason = body.reason === "busy" ? "busy" : "declined";
    quietMember(room, address, reason, session.id);
    reportCall(room, reason === "busy" ? "busy" : "decline", { by: address });
    if (!room.ring.ringing.size && room.state === "ringing" && room.participants.size < 2) {
      stopRinging(room, reason, session.id);
      endRoom(room, reason, null);
    }
    return { room: room.id, state: room.state };
  }

  async function status(auth, groupId, fromValue) {
    if (auth.guest) throw new HttpError(403, "members_only");
    if (!validGroupId(groupId)) throw new HttpError(400, "bad_group");
    const from = mailboxFor(auth, fromValue || "");
    if (!from) throw new HttpError(403, "not_your_mailbox");
    await admission(groupId, from, "audio", true);
    const room = liveRoom(groupId);
    if (!room) return { active: false };
    return { active: true, room: room.id, kind: room.kind, participants: room.participants.size, full: room.participants.size >= LIMITS.participants };
  }

  async function joinRoute(auth, groupId, body) {
    if (auth.guest) throw new HttpError(403, "members_only");
    if (!validGroupId(groupId)) throw new HttpError(400, "bad_group");
    const session = liveSession(auth, body.session);
    const from = mailboxFor(auth, body.from || "");
    if (!from) throw new HttpError(403, "not_your_mailbox");
    const room = liveRoom(groupId);
    if (!room) throw new HttpError(404, "no_call");
    await admission(groupId, from, room.kind, true);
    if (room.state === "ended") throw new HttpError(404, "no_call");
    return join(room, session, from, body.e2ee);
  }

  // A reload taking its place back: the person must still be a member.
  async function recheck(room, p) {
    if (!room.group_id || !p.mailbox) return;
    await admission(room.group_id, p.mailbox, room.kind, true);
  }

  return { start, accept, decline, status, join: joinRoute, recheck, active };
}
