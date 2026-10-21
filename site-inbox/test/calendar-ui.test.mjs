// Phase 3B in the web inbox, driven in jsdom against a fake postbox that answers the routes of
// docs/calendar.md: the feature gate, the Calendar entry and its invitation badge, week placement
// across a DST change for two viewer zones, month view with an exclusive all-day end, the create sheet
// (guests, meeting options, repeat rules), recurring edits, ambiguous times, answers, the invitations
// panel, what a non-organizer may see of contact guests, feeds, import, and live updates.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";
import { fixtureServer, response, OWNER } from "./fixtures.mjs";

const read = (name) => readFileSync(new URL("../" + name, import.meta.url), "utf8");
const tick = (ms = 30) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(predicate, what = "UI did not settle") {
  for (let n = 0; n < 300; n++) { if (predicate()) return; await tick(10); }
  assert.fail(what);
}
const U = (y, m, d, h = 0, mi = 0) => Date.UTC(y, m - 1, d, h, mi) / 1000;
// Wednesday 28 October 2026, 12:00 in New York (EDT, UTC-4).
const NOW = U(2026, 10, 28, 16);

function occurrence(over = {}) {
  return {
    occurrence_id: "occ_1", event_id: "evt_1", recurrence_key: "20261029T090000", calendar_id: "cal_a", color: "blue",
    all_day: false, starts_at: U(2026, 10, 29, 13), ends_at: U(2026, 10, 29, 14), tzid: "America/New_York", start_date: null, end_date: null,
    title: "Design review", location: "Room 2", status: "confirmed", recurring: false, overridden: false,
    organizer: { address: OWNER, handle: "/bekir/main" }, role: "organizer", my_rsvp: null, invited: false, meeting: null,
    privacy: "default", transparency: "opaque", revision: 3, ...over,
  };
}
function event(over = {}) {
  return {
    id: "evt_1", uid: "evt_1@pigeonpost.dev", calendar_id: "cal_a", revision: 3, sequence: 1, status: "confirmed",
    organizer: { address: OWNER, handle: "/bekir/main" }, title: "Design review", description: "", location: "Room 2",
    all_day: false, start: "2026-10-29T09:00", end: "2026-10-29T10:00", tzid: "America/New_York",
    starts_at: U(2026, 10, 29, 13), ends_at: U(2026, 10, 29, 14), start_date: null, end_date: null, rrule: null, exdates: [],
    transparency: "opaque", privacy: "default", meeting: null, attendees: [], counts: { accepted: 0, tentative: 0, declined: 0, needs_action: 0 },
    my: { role: "organizer", attendee_id: null, rsvp: null, calendar_id: "cal_a" }, reminders: [{ minutes_before: 10, channel: "push" }],
    linked_group: null, can_edit: true, ...over,
  };
}

// The live stream: `push` feeds frames the way the server writes them.
function eventStream() {
  const s = { queue: [], wake: null, opened: 0 };
  s.push = (frame) => { s.queue.push(frame); if (s.wake) { const w = s.wake; s.wake = null; w(); } };
  s.response = () => {
    s.opened += 1;
    const encoder = new TextEncoder();
    return { ok: true, status: 200, body: { getReader: () => ({ read: async () => {
      if (!s.queue.length) await new Promise((r) => { s.wake = r; });
      return { value: encoder.encode(s.queue.shift()), done: false };
    } }) } };
  };
  return s;
}

async function app(t, { features = ["calendar"], zone = "America/New_York", now = NOW, occurrences = [occurrence()], events = { evt_1: event() },
  calendars = [{ id: "cal_a", name: "Work", color: "blue", is_default: true, revision: 1 }], invitations = [], filtered = [], settings = {}, stream = false, configure = () => {} } = {}) {
  const server = fixtureServer();
  Object.assign(server, { features, occurrences, events, calendars, invitations, filtered, cal: [], fail: [], feeds: {}, stream: stream ? eventStream() : null });
  server.settings = { previews: false, timezone: null, week_start: 1, working_hours: null, ...settings };
  const base = server.fetch;
  server.fetch = async (url, opts = {}) => {
    const parsed = new URL(url, "https://postbox.pigeonpost.dev");
    const path = parsed.pathname, method = opts.method || "GET";
    if (path === "/v1/capabilities") return response({ features: server.features });
    if (path === "/v1/events" && server.stream) { server.calls.push({ path, query: parsed.searchParams, method }); return server.stream.response(); }
    if (!path.startsWith("/v1/calendar")) return base(url, opts);
    let body = null;
    if (typeof opts.body === "string") { try { body = JSON.parse(opts.body); } catch (_) { body = opts.body; } }
    const call = { path, method, body, query: parsed.searchParams, headers: opts.headers || {} };
    server.cal.push(call);
    const failure = server.fail.findIndex((f) => f.path === path && f.method === method);
    if (failure !== -1) { const f = server.fail.splice(failure, 1)[0]; return response(f.body, f.status); }
    if (path === "/v1/calendar/settings") return response(method === "PUT" ? { settings: body } : server.settings);
    if (path === "/v1/calendars" && method === "GET") return response({ calendars: server.calendars });
    if (path === "/v1/calendar/invitations") return response({ invitations: call.query.get("state") === "filtered" ? server.filtered : server.invitations });
    if (path === "/v1/calendar/events" && method === "GET") {
      const from = Number(call.query.get("from")), to = Number(call.query.get("to"));
      return response({ occurrences: server.occurrences.filter((o) => o.all_day || (o.starts_at < to && o.ends_at > from)) });
    }
    if (path === "/v1/calendar/events" && method === "POST") {
      const created = event({ id: "evt_new", title: body.title, rrule: body.rrule || null, attendees: [], meeting: body.meeting && body.meeting.enabled ? { code: "ABCDEFGHJK", url: "https://inbox.pigeonpost.dev/m/ABCDEFGHJK", join_before_minutes: 10 } : null });
      server.events.evt_new = created;
      return response({ event: created, delivery_status: "queued" }, 201);
    }
    let m = /^\/v1\/calendar\/events\/([^/]+?)(\.ics)?(\/.*)?$/.exec(path);
    if (m) {
      const id = decodeURIComponent(m[1]), rest = m[3] || "";
      const ev = server.events[id];
      if (m[2]) return { ok: true, status: 200, text: async () => "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n" };
      if (!ev) return response({ error: "not_found" }, 404);
      if (rest === "" && method === "GET") {
        const occId = call.query.get("occurrence_id");
        return response({ event: ev, occurrence: occId ? { occurrence_id: occId, recurrence_key: "x", starts_at: ev.starts_at, ends_at: ev.ends_at, status: "confirmed", my_rsvp: ev.my.rsvp } : null });
      }
      if (rest === "" && method === "PATCH") return response({ event: { ...ev, ...body, revision: ev.revision + 1 } });
      if (rest === "/rsvp") return response({ ok: true });
      if (rest === "/cancel") return response({ event: { ...ev, status: "cancelled" } });
      if (rest === "/attendees") return response({ event: { ...ev, revision: ev.revision + 1 } });
      if (rest === "/reminders") return response({ ok: true });
      if (/^\/attendees\/[^/]+\/link$/.test(rest)) return response({ url: "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=fresh", expires_at: NOW + 86400 });
    }
    m = /^\/v1\/calendar\/feeds\/([^/]+)$/.exec(path);
    if (m) {
      const id = decodeURIComponent(m[1]);
      if (method === "GET") return response(server.feeds[id] || { enabled: false });
      server.feeds[id] = body.enabled ? { enabled: true, projection: body.projection, created_at: NOW } : { enabled: false };
      return response(body.enabled ? { url: "https://postbox.pigeonpost.dev/v1/calendar/feed/s3cr3tfeed.ics", expires_at: NOW + 365 * 86400 } : { ok: true });
    }
    if (path === "/v1/calendar/import/preview") {
      return response({ draft_id: "drf_1", expires_at: NOW + 3600, warnings: ["Alarms were dropped."], events: [
        { index: 0, title: "Dentist", all_day: false, start: "2026-11-02T10:00", end: "2026-11-02T11:00", tzid: "Europe/Istanbul", rrule: null, warnings: [] },
        { index: 1, title: "Holiday", all_day: true, start_date: "2026-11-10", end_date: "2026-11-12", rrule: null, warnings: ["Attendees were dropped."] },
      ] });
    }
    if (/^\/v1\/calendar\/import\/[^/]+\/confirm$/.test(path)) return response({ created: (body.select || []).map((i) => event({ id: "evt_imp" + i })) });
    return response({ error: "not_found" }, 404);
  };
  configure(server);
  const errors = [], vconsole = new VirtualConsole();
  vconsole.on("jsdomError", (e) => errors.push(e.message));
  const dom = new JSDOM(read("index.html"), { url: "https://inbox.pigeonpost.dev/", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: vconsole });
  const w = dom.window, $ = (id) => w.document.getElementById(id);
  Object.defineProperty(w, "crypto", { value: webcrypto });
  w.matchMedia = (query) => ({ matches: query.includes("min-width"), addEventListener() {} });
  w.fetch = server.fetch;
  w.URL.createObjectURL = () => "blob:fixture";
  w.URL.revokeObjectURL = () => {};
  w.confirm = () => true;
  server.downloads = [];
  w.HTMLAnchorElement.prototype.click = function () { server.downloads.push(this.getAttribute("download")); };
  w.localStorage.setItem("ppi_token", "fixture_account");
  w.eval(read("config.js"));
  w.eval(read("calendar.js"));
  w.PigeonpostCalendar._test.setZone(zone);
  w.PigeonpostCalendar._test.setNow(now);
  w.eval(read("names.js"));
  w.eval(read("app.js"));
  t.after(async () => { await tick(120); w.close(); assert.deepEqual(errors, [], "No uncaught script errors"); });
  await until(() => $("identity-btn").dataset.address === "/bekir/main");
  await tick(80);
  const calls = (pred) => server.cal.filter(pred);
  return { w, $, server, calls, doc: w.document };
}

async function openCalendar(a) {
  await until(() => a.$("calendar-btn") && !a.$("calendar-btn").hidden, "Calendar entry shows");
  a.$("calendar-btn").click();
  await until(() => !a.$("calendar-view").hidden && a.calls((c) => c.path === "/v1/calendar/events" && c.method === "GET").length, "calendar opens and fetches");
  await tick(40);
}
const click = (node) => { assert.ok(node, "control exists"); node.click(); };
function keyEnter(a, input) { input.dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "Enter", bubbles: true })); }

test("calendar stays hidden without the flag", async (t) => {
  const a = await app(t, { features: [] });
  assert.ok(!a.$("calendar-btn") || a.$("calendar-btn").hidden, "no Calendar entry");
  assert.equal(a.$("side-nav").hidden, true, "no side nav when neither calls nor calendar are on");
  assert.equal(a.calls(() => true).length, 0, "no calendar requests at all");
});

test("Calendar sits in the side nav with a badge for pending invitations and opens the week view", async (t) => {
  const inv = { event_id: "evt_inv", occurrence_id: "occ_inv", title: "Offsite", starts_at: U(2026, 10, 30, 13), ends_at: U(2026, 10, 30, 14), all_day: false, organizer: { address: "/k/jane", handle: "/jane/main" }, recurring: false, meeting: true, invited_at: NOW - 60 };
  const a = await app(t, { invitations: [inv, { ...inv, event_id: "evt_inv2" }] });
  await until(() => a.$("calendar-btn") && !a.$("calendar-btn").hidden);
  assert.equal(a.$("side-nav").hidden, false, "side nav shows for calendar alone");
  assert.equal(a.$("calls-btn").hidden, true, "Calls stays hidden without call_history");
  assert.equal(a.$("calendar-btn").previousElementSibling.id, "calls-btn", "beside Calls");
  await until(() => !a.$("calendar-badge").hidden);
  assert.equal(a.$("calendar-badge").textContent, "2");
  assert.equal(a.$("calendar-btn").getAttribute("aria-label"), "Calendar, 2 invitations");
  await openCalendar(a);
  assert.equal(a.$("calendar-btn").getAttribute("aria-pressed"), "true");
  assert.equal(a.$("calendar-side").hidden, false);
  assert.equal(a.$("threads").hidden, true, "the conversation list gives way");
  assert.match(a.$("cal-invites-banner").textContent, /^2 invitations — Review$/);
  const get = a.calls((c) => c.path === "/v1/calendar/events")[0];
  assert.equal(get.query.get("include"), "invitations");
  assert.equal(get.headers["x-pigeonpost-identity"], OWNER);
  assert.ok(Number(get.query.get("to")) - Number(get.query.get("from")) <= 62 * 86400);
  assert.equal(a.$("cal-body").dataset.view, "week", "week is the desktop default");
  assert.equal(a.doc.querySelectorAll(".cal-day-col").length, 7);
  assert.ok(a.doc.querySelector('.cal-day-col[data-date="2026-10-28"] .cal-now'), "a now line on today");
});

test("week view places a New York 09:00 meeting at 09:00 on both sides of the DST change, and an hour apart for Istanbul", async (t) => {
  const before = occurrence({ occurrence_id: "occ_sat", starts_at: U(2026, 10, 31, 13), ends_at: U(2026, 10, 31, 14), recurring: true });
  const after = occurrence({ occurrence_id: "occ_sun", starts_at: U(2026, 11, 1, 14), ends_at: U(2026, 11, 1, 15), recurring: true });
  const ny = await app(t, { occurrences: [before, after] });
  await openCalendar(ny);
  const at = (a, id) => a.doc.querySelector(`.cal-event[data-occurrence="${id}"]`);
  await until(() => at(ny, "occ_sun"));
  assert.equal(at(ny, "occ_sat").closest(".cal-day-col").dataset.date, "2026-10-31");
  assert.equal(at(ny, "occ_sat").dataset.startMinute, "540");
  assert.equal(at(ny, "occ_sun").closest(".cal-day-col").dataset.date, "2026-11-01");
  assert.equal(at(ny, "occ_sun").dataset.startMinute, "540");
  assert.equal(at(ny, "occ_sun").dataset.endMinute, "600");
  const get = ny.calls((c) => c.path === "/v1/calendar/events")[0];
  assert.equal(Number(get.query.get("from")), U(2026, 10, 26, 4), "Monday 26 October, midnight New York");
  assert.equal(Number(get.query.get("to")), U(2026, 11, 2, 5), "Monday 2 November, midnight New York (EST)");

  const ist = await app(t, { occurrences: [before, after], zone: "Europe/Istanbul" });
  await openCalendar(ist);
  await until(() => at(ist, "occ_sun"));
  assert.equal(at(ist, "occ_sat").dataset.startMinute, "960", "16:00 in Istanbul before New York falls back");
  assert.equal(at(ist, "occ_sun").dataset.startMinute, "1020", "17:00 after");
});

test("overlapping events sit side by side", async (t) => {
  const a = await app(t, { occurrences: [occurrence({ occurrence_id: "o1" }), occurrence({ occurrence_id: "o2", starts_at: U(2026, 10, 29, 13, 30), ends_at: U(2026, 10, 29, 15) })] });
  await openCalendar(a);
  await until(() => a.doc.querySelector('.cal-event[data-occurrence="o2"]'));
  const one = a.doc.querySelector('.cal-event[data-occurrence="o1"]'), two = a.doc.querySelector('.cal-event[data-occurrence="o2"]');
  assert.equal(one.dataset.cols, "2");
  assert.notEqual(one.dataset.col, two.dataset.col);
});

test("month view honours the exclusive end of all-day events and moves by arrow keys", async (t) => {
  const allDay = occurrence({ occurrence_id: "occ_all", all_day: true, starts_at: null, ends_at: null, start_date: "2026-10-22", end_date: "2026-10-24", title: "Conference" });
  const a = await app(t, { occurrences: [allDay] });
  await openCalendar(a);
  click(a.doc.querySelector('input[name="cal-view"][value="month"]'));
  await until(() => a.doc.querySelector(".cal-month"));
  await tick(40);
  const cell = (d) => a.doc.querySelector(`.cal-month-cell[data-date="${d}"]`);
  assert.equal(a.doc.querySelector(".cal-month").getAttribute("role"), "grid");
  assert.ok(cell("2026-10-22").querySelector('.cal-chip[data-occurrence="occ_all"]'));
  assert.ok(cell("2026-10-23").querySelector('.cal-chip[data-occurrence="occ_all"]'));
  assert.equal(cell("2026-10-24").querySelector('.cal-chip[data-occurrence="occ_all"]'), null, "the end date is exclusive");
  assert.equal(a.doc.querySelectorAll(".cal-month-cell").length, 42, "a six-week grid");
  const month = a.calls((c) => c.path === "/v1/calendar/events").at(-1);
  assert.ok(Number(month.query.get("to")) - Number(month.query.get("from")) <= 62 * 86400);
  const focused = a.doc.querySelector('.cal-month-cell[tabindex="0"]');
  assert.equal(focused.dataset.date, "2026-10-28");
  focused.focus();
  focused.dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
  assert.equal(a.doc.activeElement.dataset.date, "2026-11-04");
  a.doc.activeElement.dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }));
  assert.equal(a.doc.activeElement.dataset.date, "2026-11-03");
});

test("the create sheet builds the request: guests, co-host, meeting options and a weekly rule", async (t) => {
  const a = await app(t, { occurrences: [] });
  await openCalendar(a);
  click(a.$("cal-new"));
  await until(() => !a.$("cal-edit-sheet").hidden);
  assert.equal(a.doc.activeElement.id, "ce-title");
  assert.equal(a.$("ce-meeting").checked, false, "no guests yet: no meeting");
  a.$("ce-title").value = "Design review";
  a.$("ce-date").value = "2026-10-29";
  a.$("ce-date").dispatchEvent(new a.w.Event("change"));
  a.$("ce-start").value = "09:00";
  a.$("ce-end").value = "10:00";
  a.$("ce-tz").value = "America/New_York";
  a.$("ce-guest").value = "alice/main";
  keyEnter(a, a.$("ce-guest"));
  a.$("ce-guest").value = "Guest Person <Guest@Example.com>";
  keyEnter(a, a.$("ce-guest"));
  assert.equal(a.$("ce-chips").children.length, 2);
  assert.equal(a.$("ce-meeting").checked, true, "guests turn the meeting on");
  const contactChip = a.doc.querySelector('.ce-chip[data-kind="contact"]');
  assert.ok(contactChip.querySelector(".ce-chip-icon svg"), "an envelope icon");
  assert.match(a.$("ce-contact-note").textContent, /Invitation link to share yourself/);
  click([...a.doc.querySelectorAll('.ce-chip[data-kind="handle"] .ce-chip-toggle')].find((b) => b.textContent === "Co-host"));
  a.$("ce-repeat").value = "weekly";
  a.$("ce-repeat").dispatchEvent(new a.w.Event("change"));
  assert.equal(a.$("ce-repeat-weekly").textContent, "Weekly on Thursday");
  a.$("ce-waiting").value = "none";
  a.$("ce-admit").value = "organizer";
  a.$("ce-knock").checked = true;
  click(a.doc.querySelector('input[name="ce-transport"][value="sfu"]'));
  a.$("ce-reminder").value = "30";
  click(a.$("ce-save"));
  await until(() => a.calls((c) => c.path === "/v1/calendar/events" && c.method === "POST").length);
  const post = a.calls((c) => c.path === "/v1/calendar/events" && c.method === "POST")[0].body;
  assert.match(post.request_key, /^[A-Za-z0-9_-]{1,64}$/);
  assert.equal(post.title, "Design review");
  assert.equal(post.start, "2026-10-29T09:00");
  assert.equal(post.end, "2026-10-29T10:00");
  assert.equal(post.tzid, "America/New_York");
  assert.equal(post.all_day, false);
  assert.equal(post.rrule, "FREQ=WEEKLY;BYDAY=TH");
  assert.equal(post.calendar_id, "cal_a");
  assert.deepEqual(post.attendees, [{ handle: "/alice/main", role: "cohost" }, { contact: "guest@example.com", name: "Guest Person" }]);
  assert.deepEqual(post.meeting, { enabled: true, waiting: "none", admit: "organizer", allow_knock: true, transport: "sfu" });
  assert.deepEqual(post.reminders, [{ minutes_before: 30, channel: "push" }]);
  assert.equal(post.locale, "en");
  await until(() => !a.$("cal-detail-sheet").hidden && a.$("cd-saved"));
  assert.equal(a.$("cd-saved").textContent, "Event saved — invitations queued");
  assert.match(a.$("cd-link").textContent, /\/m\/ABCDEFGHJK$/, "the meeting link after save");
  a.$("cal-detail-sheet").dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  assert.equal(a.$("cal-detail-sheet").hidden, true, "Escape closes the sheet");
});

test("a custom repeat produces a COUNT rule", async (t) => {
  const a = await app(t, { occurrences: [], features: ["calendar", "calendar_email"] });
  await openCalendar(a);
  click(a.$("cal-new"));
  a.$("ce-title").value = "Standup";
  a.$("ce-date").value = "2026-11-02";
  a.$("ce-guest").value = "/k/abcdefabcdef";
  keyEnter(a, a.$("ce-guest"));
  a.$("ce-guest").value = "pat@example.org";
  keyEnter(a, a.$("ce-guest"));
  assert.match(a.$("ce-contact-note").textContent, /Will get an invitation at this address/, "with calendar_email on");
  a.$("ce-repeat").value = "custom";
  a.$("ce-repeat").dispatchEvent(new a.w.Event("change"));
  assert.equal(a.$("ce-custom").hidden, false);
  a.$("ce-freq").value = "DAILY";
  a.$("ce-freq").dispatchEvent(new a.w.Event("change"));
  a.$("ce-interval").value = "2";
  click(a.doc.querySelector('input[name="ce-ends"][value="count"]'));
  a.$("ce-count").value = "5";
  click(a.$("ce-save"));
  await until(() => a.calls((c) => c.path === "/v1/calendar/events" && c.method === "POST").length);
  const post = a.calls((c) => c.method === "POST" && c.path === "/v1/calendar/events")[0].body;
  assert.equal(post.rrule, "FREQ=DAILY;INTERVAL=2;COUNT=5");
  assert.deepEqual(post.attendees, [{ handle: "/k/abcdefabcdef" }, { contact: "pat@example.org" }]);
});

test("editing a recurring event asks for the scope and sends scope, occurrence and revision", async (t) => {
  const ev = event({ rrule: "FREQ=WEEKLY;BYDAY=TH;COUNT=6", revision: 7 });
  const a = await app(t, { occurrences: [occurrence({ recurring: true, occurrence_id: "occ_29" })], events: { evt_1: ev } });
  await openCalendar(a);
  click(a.doc.querySelector('.cal-event[data-occurrence="occ_29"] .cal-event-open'));
  await until(() => a.$("cd-edit"));
  assert.match(a.$("cal-detail-sheet-body").textContent, /Weekly on Thursday, 6 times/);
  assert.equal(a.calls((c) => c.method === "GET" && c.path === "/v1/calendar/events/evt_1")[0].query.get("occurrence_id"), "occ_29");
  click(a.$("cd-edit"));
  await until(() => !a.$("cal-edit-sheet").hidden);
  assert.equal(a.$("ce-repeat").value, "custom", "a weekly rule with a count is a custom rule");
  assert.equal(a.$("ce-count").value, "6");
  assert.equal(a.$("ce-byday-TH").checked, true);
  a.$("ce-title").value = "Design review (moved)";
  click(a.$("ce-save"));
  await until(() => !a.$("cal-scope-sheet").hidden);
  const labels = [...a.doc.querySelectorAll('input[name="cs-scope"]')].map((i) => i.parentNode.textContent);
  assert.deepEqual(labels, ["This event", "This and following events", "All events"]);
  click(a.$("cs-scope-ok"));
  await until(() => a.calls((c) => c.method === "PATCH").length);
  const patch = a.calls((c) => c.method === "PATCH")[0];
  assert.equal(patch.path, "/v1/calendar/events/evt_1");
  assert.equal(patch.body.scope, "this");
  assert.equal(patch.body.occurrence_id, "occ_29");
  assert.equal(patch.body.expected_revision, 7);
  assert.equal(patch.body.title, "Design review (moved)");
  assert.equal("rrule" in patch.body, false, "one occurrence keeps the series rule");
  assert.equal("attendees" in patch.body, false);
});

test("an ambiguous time asks which one and resends with fold", async (t) => {
  const a = await app(t, { occurrences: [], configure: (s) => s.fail.push({ path: "/v1/calendar/events", method: "POST", status: 422, body: { error: "time_invalid", kind: "ambiguous", message: "ambiguous" } }) });
  await openCalendar(a);
  click(a.$("cal-new"));
  a.$("ce-title").value = "Early call";
  a.$("ce-date").value = "2026-11-01";
  a.$("ce-start").value = "01:30";
  a.$("ce-end").value = "02:00";
  a.$("ce-tz").value = "America/New_York";
  click(a.$("ce-save"));
  await until(() => !a.$("ce-fold").hidden, "the fold prompt");
  assert.equal(a.$("cal-edit-sheet").hidden, false);
  assert.equal("fold" in a.calls((c) => c.method === "POST")[0].body, false);
  click(a.$("ce-fold-later"));
  await until(() => a.calls((c) => c.method === "POST" && c.path === "/v1/calendar/events").length === 2);
  const second = a.calls((c) => c.method === "POST" && c.path === "/v1/calendar/events")[1].body;
  assert.equal(second.fold, "later");
  assert.equal(second.start, "2026-11-01T01:30");
  await until(() => a.$("cal-edit-sheet").hidden);
});

test("other refusals are explained in the sheet", async (t) => {
  const a = await app(t, { occurrences: [], configure: (s) => s.fail.push({ path: "/v1/calendar/events", method: "POST", status: 422, body: { error: "meeting_capacity" } }) });
  await openCalendar(a);
  click(a.$("cal-new"));
  a.$("ce-title").value = "Big";
  click(a.$("ce-save"));
  await until(() => !a.$("ce-error").hidden);
  assert.match(a.$("ce-error").textContent, /Large call/);
});

test("an invited attendee answers with Yes, Maybe or No and a note", async (t) => {
  const ev = event({ my: { role: "attendee", attendee_id: "att_me", rsvp: "needs_action", calendar_id: "cal_a" }, organizer: { address: "/k/jane", handle: "/jane/main" } });
  const a = await app(t, { occurrences: [occurrence({ role: "attendee", invited: true, my_rsvp: "needs_action" })], events: { evt_1: ev } });
  await openCalendar(a);
  click(a.doc.querySelector('.cal-event[data-occurrence="occ_1"] .cal-event-open'));
  await until(() => a.$("cd-rsvp-tentative"));
  assert.equal(a.$("cd-edit"), null, "no organizer actions");
  assert.ok(a.$("cd-ics") && a.$("cd-message"));
  a.$("cd-note").value = "Might be late";
  click(a.$("cd-rsvp-tentative"));
  await until(() => a.calls((c) => c.path.endsWith("/rsvp")).length);
  assert.deepEqual(a.calls((c) => c.path.endsWith("/rsvp"))[0].body, { status: "tentative", scope: "all", note: "Might be late" });
  click(a.$("cd-ics"));
  await until(() => a.calls((c) => c.path === "/v1/calendar/events/evt_1.ics").length);
  assert.match(a.calls((c) => c.path === "/v1/calendar/events/evt_1.ics")[0].headers.authorization, /^Bearer /);
  await until(() => a.server.downloads.length);
  assert.equal(a.server.downloads[0], "Design review.ics");
});

test("the invitations panel lists pending and filtered invitations and answers them", async (t) => {
  const inv = { event_id: "evt_inv", occurrence_id: "occ_inv", title: "Offsite", starts_at: U(2026, 10, 30, 13), ends_at: U(2026, 10, 30, 14), all_day: false, tzid: "America/New_York", organizer: { address: "/k/jane", handle: "/jane/main" }, recurring: true, meeting: true, invited_at: NOW - 60 };
  const a = await app(t, { invitations: [inv], filtered: [{ ...inv, event_id: "evt_fil", title: "Webinar" }] });
  await openCalendar(a);
  click(a.$("cal-invites-banner"));
  await until(() => !a.$("cal-invites-sheet").hidden && a.$("cal-filtered-list"));
  assert.ok(a.calls((c) => c.path === "/v1/calendar/invitations" && c.query.get("state") === "filtered").length);
  const card = a.doc.querySelector('#cal-invites-list .cal-invite[data-event="evt_inv"]');
  assert.match(card.textContent, /Offsite/);
  assert.match(card.textContent, /From \/jane\/main · Pigeonpost meeting/);
  assert.match(a.doc.querySelector('#cal-filtered-list .cal-invite[data-event="evt_fil"]').textContent, /Webinar/);
  click(card.querySelector('button[data-rsvp="accepted"]'));
  await until(() => a.calls((c) => c.path === "/v1/calendar/events/evt_inv/rsvp").length);
  assert.deepEqual(a.calls((c) => c.path === "/v1/calendar/events/evt_inv/rsvp")[0].body, { status: "accepted", scope: "all" });
});

test("a contact guest's address is shown to the organizer only", async (t) => {
  const attendees = [
    { attendee_id: "att_a", kind: "handle", address: "/k/alice", handle: "/alice/main", name: "/alice/main", role: "attendee", optional: false, rsvp: "accepted" },
    { attendee_id: "att_g", kind: "external", name: "Guest Person", contact: "secret@example.com", role: "attendee", rsvp: "needs_action", proof: null, delivery: "queued" },
  ];
  const counts = { accepted: 3, tentative: 1, declined: 0, needs_action: 1 };
  const meeting = { code: "ABCDEFGHJK", url: "https://inbox.pigeonpost.dev/m/ABCDEFGHJK", join_before_minutes: 10, waiting: "guests", admit: "hosts" };
  const asAttendee = await app(t, { events: { evt_1: event({ attendees, counts, meeting, my: { role: "attendee", attendee_id: "att_a", rsvp: "accepted", calendar_id: "cal_a" } }) } });
  await openCalendar(asAttendee);
  click(asAttendee.doc.querySelector(".cal-event-open"));
  await until(() => asAttendee.$("cd-guests"));
  const text = asAttendee.$("cal-detail-sheet").textContent;
  assert.ok(!text.includes("secret@example.com"), "never the address to a non-organizer");
  assert.match(text, /Guest Person/);
  assert.equal(asAttendee.$("cd-counts").textContent, "3 yes, 1 maybe, 1 awaiting");
  assert.equal(asAttendee.doc.querySelector(".cd-guest-link"), null);
  assert.match(asAttendee.$("cd-at-rest").textContent, /stored encrypted at rest; Pigeonpost can read them/);
  assert.doesNotMatch(text, /end-to-end/i, "calendar data is never labelled end-to-end encrypted");

  const asOrganizer = await app(t, { events: { evt_1: event({ attendees, counts, meeting }) } });
  await openCalendar(asOrganizer);
  click(asOrganizer.doc.querySelector(".cal-event-open"));
  await until(() => asOrganizer.$("cd-guests"));
  assert.match(asOrganizer.$("cd-guests").textContent, /secret@example\.com/);
  assert.match(asOrganizer.$("cd-not-sent").textContent, /Invitations to contact addresses are not sent yet/);
  assert.equal(asOrganizer.doc.querySelector('.cd-delivery').textContent, "Queued");
  click(asOrganizer.doc.querySelector(".cd-guest-link"));
  await until(() => asOrganizer.calls((c) => c.path === "/v1/calendar/events/evt_1/attendees/att_g/link").length);
  await until(() => asOrganizer.doc.querySelector(".cd-guest-url"));
});

test("Join in the detail opens the scheduled meeting through calls.js", async (t) => {
  const meeting = { code: "ABCDEFGHJK", url: "https://inbox.pigeonpost.dev/m/ABCDEFGHJK", join_before_minutes: 10 };
  const live = event({ meeting, starts_at: NOW - 300, ends_at: NOW + 1800 });
  const a = await app(t, { occurrences: [occurrence({ starts_at: NOW - 300, ends_at: NOW + 1800, meeting: { code: "ABCDEFGHJK", opens_at: NOW - 900, closes_at: NOW + 3600 } })], events: { evt_1: live } });
  const joins = [];
  a.w.PigeonpostCalls = { start() {}, stop() {}, renderHead() {}, joinScheduled: async (code, opts) => { joins.push({ code, opts }); return joins.length === 1 ? { state: "choose_occurrence", occurrences: [{ occurrence_id: "occ_a", starts_at: NOW, ends_at: NOW + 60 }] } : { state: "joined" }; } };
  await openCalendar(a);
  const inline = a.doc.querySelector('.cal-event[data-occurrence="occ_1"] .cal-join');
  assert.ok(inline, "an in-progress meeting shows Join inline");
  click(inline);
  await until(() => joins.length === 1);
  assert.deepEqual(JSON.parse(JSON.stringify(joins[0])), { code: "ABCDEFGHJK", opts: { occurrenceId: "occ_1", as: OWNER, title: "Design review" } });
  await until(() => !a.$("cal-choose-sheet").hidden);
  click(a.doc.querySelector("#cal-choose-sheet .cal-choose-list button"));
  await until(() => joins.length === 2);
  assert.equal(joins[1].opts.occurrenceId, "occ_a");
});

test("a feed link is shown once, with reset and off afterwards", async (t) => {
  const a = await app(t);
  await openCalendar(a);
  click(a.$("cal-settings-btn"));
  await until(() => a.$("cs-feed-on-cal_a"));
  a.$("cs-feed-proj-cal_a").value = "personal_redacted";
  click(a.$("cs-feed-on-cal_a"));
  await until(() => a.$("cs-feed-url-cal_a"));
  assert.equal(a.$("cs-feed-url-cal_a").value, "https://postbox.pigeonpost.dev/v1/calendar/feed/s3cr3tfeed.ics");
  assert.deepEqual(a.calls((c) => c.method === "POST" && c.path === "/v1/calendar/feeds/cal_a")[0].body, { enabled: true, projection: "personal_redacted" });
  assert.match(a.$("cal-settings-sheet").textContent, /are not recalled/);
  a.$("cal-settings-sheet").dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  click(a.$("cal-settings-btn"));
  await until(() => a.$("cs-feed-reset-cal_a"));
  assert.equal(a.$("cs-feed-url-cal_a"), null, "the link is not shown again");
  assert.ok(!a.$("cal-settings-sheet").textContent.includes("s3cr3tfeed"));
  click(a.$("cs-feed-off-cal_a"));
  await until(() => a.calls((c) => c.method === "POST" && c.path === "/v1/calendar/feeds/cal_a").length === 2);
  assert.deepEqual(a.calls((c) => c.method === "POST" && c.path === "/v1/calendar/feeds/cal_a")[1].body, { enabled: false });
});

test("import previews an .ics file and confirms the chosen events", async (t) => {
  const a = await app(t);
  await openCalendar(a);
  click(a.$("cal-settings-btn"));
  await until(() => a.$("cs-import-file") && a.$("cs-feed-on-cal_a"), "settings loaded");
  const file = new a.w.File(["BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"], "events.ics", { type: "text/calendar" });
  Object.defineProperty(a.$("cs-import-file"), "files", { value: [file] });
  a.$("cs-import-file").dispatchEvent(new a.w.Event("change"));
  await until(() => a.$("cs-import-list"), "import preview");
  const preview = a.calls((c) => c.path === "/v1/calendar/import/preview")[0];
  assert.equal(preview.method, "POST");
  assert.equal(preview.body, "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n", "the raw file is the body");
  assert.equal(preview.headers["content-type"], "text/calendar");
  assert.equal(preview.query.get("calendar_id"), "cal_a");
  assert.match(a.$("cs-import-preview").textContent, /Alarms were dropped/);
  assert.match(a.$("cs-import-preview").textContent, /Attendees were dropped/);
  const picks = a.doc.querySelectorAll(".cs-import-pick");
  assert.equal(picks.length, 2);
  picks[1].checked = false;
  click(a.$("cs-import-confirm"));
  await until(() => a.calls((c) => c.path === "/v1/calendar/import/drf_1/confirm").length);
  assert.deepEqual(a.calls((c) => c.path === "/v1/calendar/import/drf_1/confirm")[0].body, { select: [0], calendar_id: "cal_a" });
  await until(() => /Imported 1 event/.test(a.$("cs-import-preview").textContent));
});

test("event.* stream events refetch, and a reminder offers Join", async (t) => {
  const meeting = { code: "ABCDEFGHJK", url: "https://inbox.pigeonpost.dev/m/ABCDEFGHJK", join_before_minutes: 10 };
  const a = await app(t, { stream: true, events: { evt_1: event({ meeting }) } });
  await openCalendar(a);
  await until(() => a.server.stream.opened > 0, "stream opened");
  const lists = () => a.calls((c) => c.path === "/v1/calendar/events" && c.method === "GET").length;
  const invites = () => a.calls((c) => c.path === "/v1/calendar/invitations").length;
  for (const type of ["event.updated", "event.cancelled", "event.rsvp", "event.invite"]) {
    const n = lists(), i = invites();
    a.server.stream.push(`event: ${type}\ndata: ${JSON.stringify({ mailbox: OWNER, data: { event_id: "evt_1", revision: 9 } })}\n\n`);
    await until(() => lists() > n, type + " refetches the range");
    if (type === "event.invite") await until(() => invites() > i, "invitations reload");
  }
  a.server.stream.push(`event: event.reminder\ndata: ${JSON.stringify({ event_id: "evt_1", occurrence_id: "occ_1", reminder_id: "r1", starts_at: U(2026, 10, 29, 13) })}\n\n`);
  await until(() => !a.$("cal-toast").hidden);
  assert.match(a.$("cal-toast-text").textContent, /^Design review starts at /);
  assert.equal(a.$("cal-toast-join").hidden, false);
});
