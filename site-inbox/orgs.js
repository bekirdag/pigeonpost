// Pigeonpost inbox — the organization parts members see inside the inbox (SDS §5.4, §4 org rooms).
//
// The enterprise portal is for admins. A member meets the org here: their org mailbox carries the
// org's name in the mailbox switcher, the org's rooms sit under a Company heading with a Browse
// rooms entry, org addresses autocomplete when composing, and a colleague's profile says which org
// they belong to.
//
// Hidden unless the postbox reports `orgs` for the account. The postbox is the only source of
// truth: what this file keeps (the catalog, directory answers) is a display hint, and every join or
// creation asks the server again. Untrusted text is only ever inserted with textContent.
(function () {
  "use strict";

  const IDENT = /^[A-Za-z0-9_-]{1,128}$/;
  const MAILBOX = /^\/[A-Za-z0-9._~\-/]{1,250}$/;
  // Bidi controls, C0/C1 controls and lone surrogates make a name lie about itself.
  const UNSAFE = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f؜‎‏‪-‮⁦-⁩]|[\ud800-\udfff]/;
  const CREATOR_ROLES = new Set(["owner", "admin", "people_admin", "agent_admin", "billing_admin"]);
  const ROLE_ORDER = ["owner", "admin", "people_admin", "agent_admin", "billing_admin"];
  const ROLE_LABEL = { owner: "Owner", admin: "Admin", people_admin: "People admin", agent_admin: "Agent admin", billing_admin: "Billing admin" };

  const O = { catalog: null, loading: null, gen: 0, listeners: new Set(), directory: new Map() };

  function safeText(value, limit, multiline) {
    if (typeof value !== "string" || value.length > limit) return false;
    if (UNSAFE.test(multiline ? value.replace(/[\n\t]/g, " ") : value)) return false;
    return multiline || !/[\n\t]/.test(value);
  }
  const optText = (value, limit, multiline) => (value == null ? null : safeText(value, limit, multiline) ? value : undefined);

  // One org row of `GET /v1/orgs`. A row that does not match the contract is dropped, not shown.
  function parseOrg(raw) {
    if (!raw || typeof raw !== "object" || !IDENT.test(raw.id || "") || !IDENT.test(raw.my_member_id || "")) return null;
    if (!safeText(raw.name, 256) || !raw.name.trim() || !IDENT.test(raw.status || "")) return null;
    const settings = raw.settings && typeof raw.settings === "object" ? raw.settings : {};
    const creation = ["everyone", "admins"].includes(settings.rooms_creation) ? settings.rooms_creation : null;
    const roles = Array.isArray(raw.my_roles) ? raw.my_roles.filter((r) => typeof r === "string" && IDENT.test(r)).slice(0, 16) : [];
    const mb = raw.my_mailbox;
    let mailbox = null;
    if (mb && typeof mb === "object" && typeof mb.address === "string" && MAILBOX.test(mb.address)) {
      const handle = optText(mb.handle, 256);
      mailbox = { address: mb.address, handle: handle === undefined ? null : handle };
    }
    const namespace = optText(raw.namespace, 128);
    return { id: raw.id, name: raw.name, status: raw.status, memberId: raw.my_member_id, namespace: namespace || null, roomsCreation: creation, roles, mailbox };
  }

  function parseCatalog(raw) {
    const rows = raw && Array.isArray(raw.orgs) ? raw.orgs.slice(0, 1000) : [];
    const seen = new Set();
    const orgs = [];
    for (const row of rows) {
      const org = parseOrg(row);
      if (!org || seen.has(org.id)) continue;
      seen.add(org.id);
      orgs.push(org);
    }
    return { orgs };
  }

  const readable = (org) => ["active", "past_due", "read_only"].includes(org.status);
  const writable = (org) => ["active", "past_due"].includes(org.status);

  // A display hint only; the postbox rechecks membership, status and policy when the room is created.
  function mayCreate(org, acting) {
    if (!org || !writable(org) || !org.mailbox || org.mailbox.address !== acting) return false;
    if (org.roomsCreation === "everyone") return true;
    if (org.roomsCreation === "admins") return org.roles.some((r) => CREATOR_ROLES.has(r));
    return false;
  }

  // "Admin", "Member" … the highest role this person holds in the org.
  function roleLabel(org) {
    for (const r of ROLE_ORDER) if (org.roles.includes(r)) return ROLE_LABEL[r];
    return "Member";
  }

  function parseRooms(raw) {
    const rows = raw && Array.isArray(raw.rooms) ? raw.rooms.slice(0, 50) : [];
    const out = [];
    const seen = new Set();
    for (const r of rows) {
      if (!r || !IDENT.test(r.id || "") || seen.has(r.id) || !IDENT.test(r.join_policy || "")) continue;
      if (!safeText(r.name, 256) || !r.name.trim()) continue;
      if (!Number.isInteger(r.member_count) || r.member_count < 0) continue;
      const description = optText(r.description, 2000, true);
      if (description === undefined) continue;
      if (r.my_state != null && !IDENT.test(r.my_state)) continue;
      seen.add(r.id);
      out.push({ id: r.id, name: r.name, description, memberCount: r.member_count, joinPolicy: r.join_policy, myState: r.my_state || null });
    }
    return out;
  }

  // open | accept | join | unavailable, from the server's own state for this mailbox.
  function roomAction(room, org) {
    const state = room.myState;
    if (state === "active" || state === "keying") return "open";
    if (org && !writable(org)) return "unavailable";
    if (state === "invited") return "accept";
    if (state == null || state === "left" || state === "removed") return room.joinPolicy === "public" ? "join" : "unavailable";
    return "unavailable";
  }

  function parseDirectory(raw) {
    const rows = raw && Array.isArray(raw.entries) ? raw.entries.slice(0, 50) : [];
    const out = [];
    for (const e of rows) {
      if (!e || typeof e.handle !== "string" || !MAILBOX.test(e.handle) || typeof e.address !== "string" || !MAILBOX.test(e.address)) continue;
      if (e.kind !== "member" && e.kind !== "agent") continue;
      const name = optText(e.name, 200);
      if (name === undefined) continue;
      out.push({ handle: e.handle, address: e.address, kind: e.kind, name });
    }
    return out;
  }

  function explain(e) {
    const code = e && e.code;
    const status = e && e.status;
    if (code === "rate_limited" || status === 429) return "Too many requests. Wait a moment and try again.";
    if (code === "not_org_member") return "Act as your organization mailbox to use its rooms.";
    if (code === "forbidden" || code === "posting_restricted") return "Your organization doesn’t allow this.";
    if (code === "org_read_only") return "This organization is read-only right now.";
    if (code === "org_inactive" || code === "not_found") return "This organization or room is not available any more.";
    if (code === "banned") return "You are banned from this room.";
    if (code === "group_full") return "This room is full.";
    if (code === "invite_required") return "This room is invite only.";
    return (e && e.body && typeof e.body.message === "string" && e.body.message) || "Something went wrong.";
  }

  // ---- network --------------------------------------------------------------------------------------

  const host = () => window.PigeonpostGroupsHost;
  class OrgApiError extends Error {
    constructor(status, code, body) { super(code || "http " + status); this.status = status; this.code = code; this.body = body; }
  }
  async function api(path, mailbox, renewed) {
    const h = host();
    if (!h) throw new OrgApiError(0, "no_host");
    const headers = { authorization: "Bearer " + h.token(), accept: "application/json" };
    if (mailbox) headers["x-pigeonpost-identity"] = mailbox;
    const res = await fetch(h.postbox() + path, { headers });
    if (res.status === 401 && !renewed && (await h.renew())) return api(path, mailbox, true);
    const value = await res.json().catch(() => ({}));
    if (!res.ok) throw new OrgApiError(res.status, value && value.error, value);
    return value;
  }

  const enabled = () => { const h = host(); return Boolean(h && h.features().has("orgs")); };

  function notify() { for (const fn of O.listeners) { try { fn(); } catch (_) { /* a listener must not break the others */ } } }

  // Called by app.js when capabilities load and on org.* events. The catalog is the account's.
  async function load() {
    const gen = ++O.gen;
    O.directory.clear();
    if (!enabled()) {
      const had = O.catalog !== null;
      O.catalog = null;
      if (had) notify();
      return null;
    }
    try {
      const body = await api("/v1/orgs");
      if (gen !== O.gen) return O.catalog;
      O.catalog = parseCatalog(body);
    } catch (_) {
      if (gen !== O.gen) return O.catalog;
      O.catalog = null; // a 404 (flag off for this org's owner) or an outage: show nothing
    }
    notify();
    return O.catalog;
  }

  function reset() { O.gen++; O.catalog = null; O.directory.clear(); }

  const orgs = () => (O.catalog ? O.catalog.orgs.filter(readable) : []);
  const orgFor = (address) => (address ? orgs().find((o) => o.mailbox && o.mailbox.address === address) || null : null);

  async function rooms(orgId, query, mailbox) {
    if (orgId != null && !IDENT.test(orgId)) throw new OrgApiError(0, "bad_org");
    const q = String(query || "").trim().slice(0, 80);
    const params = new URLSearchParams();
    // With org_id this is that org's directory; without it, the listed public groups.
    if (orgId) params.set("org_id", orgId);
    if (q) params.set("q", q);
    const qs = params.toString();
    return parseRooms(await api("/v1/rooms" + (qs ? "?" + qs : ""), mailbox));
  }

  // Org directory search for autocomplete. Results are cached per org and query for this load.
  async function directory(orgId, query) {
    if (!IDENT.test(orgId || "")) return [];
    const q = String(query || "").trim().toLowerCase().slice(0, 80);
    const key = orgId + "\u0000" + q;
    if (O.directory.has(key)) return O.directory.get(key);
    const pending = api("/v1/orgs/" + encodeURIComponent(orgId) + "/directory?" + new URLSearchParams({ q, limit: "20" }))
      .then(parseDirectory).catch(() => { O.directory.delete(key); return []; });
    O.directory.set(key, pending);
    return pending;
  }

  // Which of the acting mailbox's orgs a peer belongs to, by exact handle or address.
  async function membershipOf(peer, acting) {
    const org = orgFor(acting);
    if (!org || !peer || !MAILBOX.test(peer)) return null;
    const rows = await directory(org.id, peer.split("/").filter(Boolean).pop() || "");
    const hit = rows.find((r) => r.handle === peer || r.address === peer);
    return hit ? { org, entry: hit } : null;
  }

  window.PigeonpostOrgs = {
    load, reset, enabled, orgs, orgFor, rooms, directory, membershipOf, mayCreate, roleLabel, roomAction, explain, writable,
    onChange: (fn) => { O.listeners.add(fn); return () => O.listeners.delete(fn); },
    // Pure helpers, for tests.
    _parse: { parseCatalog, parseRooms, parseDirectory, safeText },
  };
})();
