// SDS §5.4 in the web inbox, driven in jsdom: the org mailbox badge in the switcher, the Company
// heading with Browse rooms, the room directory (join, accept, open), member room creation, org
// address autocomplete, the colleague's org line, listed public groups, and the `orgs` gate.
// The MLS client is a stand-in, as in groups-ui.test.mjs.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";
import { fixtureServer, response, OWNER, PEER } from "./fixtures.mjs";

const read = (name) => readFileSync(new URL("../" + name, import.meta.url), "utf8");
const tick = (ms = 30) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(predicate, what = "UI did not settle") {
  for (let n = 0; n < 300; n++) { if (predicate()) return; await tick(10); }
  assert.fail(what);
}
const NOW = Math.floor(Date.now() / 1000);
const ORG = "org_acme0000000000000000000";

function orgRow(over = {}) {
  return {
    id: ORG, my_member_id: "mem_john", name: "Acme Inc.", status: "active", namespace: "acme",
    settings: { rooms_creation: "everyone" }, my_roles: ["member"], my_mailbox: { address: OWNER, handle: "/bekir/main" }, ...over,
  };
}
function group(over = {}) {
  return {
    id: "grp_personalpersonalpersonal", org_id: null, name: "Garden club", description: null, join_policy: "private", post_policy: "everyone",
    visibility: "unlisted", members_can_invite: false, members_can_change_timer: false, ephemeral_secs: null, revision: 1, epoch: 1,
    activation_state: "active", member_count: 2, max_members: 1000, created_by: OWNER, created_at: NOW - 900, last_at: NOW - 60, last_seq: 0,
    deleted_at: null, has_link: false,
    my: { role: "member", state: "active", read_cursor: 0, unread: 0, notify: "all", muted_until: null, invited_by: null, joined_at: NOW - 900, filtered: false, revision: 1 },
    ...over,
  };
}

function fakeClient(server) {
  return {
    async init() { return { deviceId: "web-testdevice00", persistent: true }; },
    async topUpKeyPackages() { return 0; },
    async processWelcomes() { return []; },
    async sync() { return { processed: 0 }; },
    async processPending() { return null; },
    async activate() { return null; },
    async createGroup(settings) {
      server.created.push(settings);
      const g = group({ id: "grp_newroomnewroomnewroomnewr", org_id: settings.org_id || null, name: settings.name, my: { ...group().my, role: "owner" } });
      server.groups.push(g);
      return { group: g, invitations: [] };
    },
    async pruneExpired() { return 0; },
    async dropSeqs() {},
    inGroup: () => true,
    history: () => [],
    safetyNumber: () => "1".repeat(30),
    fingerprint: () => "",
    deviceId: () => "web-testdevice00",
    members: () => [],
    epoch: () => 1,
  };
}

async function app(t, { features = ["groups", "orgs"], orgs = [orgRow()], groups = [], rooms = [], publicRooms = [], directory = [], orgsStatus = 200 } = {}) {
  const server = fixtureServer();
  Object.assign(server, { groups, created: [], ocalls: [] });
  const base = server.fetch;
  server.fetch = async (url, opts = {}) => {
    const parsed = new URL(url, "https://postbox.pigeonpost.dev");
    const path = parsed.pathname, method = opts.method || "GET";
    const body = typeof opts.body === "string" ? JSON.parse(opts.body) : null;
    const headers = opts.headers || {};
    if (path === "/v1/capabilities") return response({ features });
    if (path.startsWith("/v1/orgs") || path === "/v1/rooms" || path.startsWith("/v1/groups") || path.startsWith("/v1/mls") || path.startsWith("/calls/")) {
      server.ocalls.push({ path, method, body, query: parsed.searchParams, identity: headers["x-pigeonpost-identity"] || null });
      if (path === "/v1/orgs") return orgsStatus === 200 ? response({ orgs, operated: [] }) : response({ error: "feature_disabled" }, orgsStatus);
      if (path === `/v1/orgs/${ORG}/directory`) {
        const q = (parsed.searchParams.get("q") || "").toLowerCase();
        return response({ entries: directory.filter((e) => e.handle.toLowerCase().includes(q) || (e.name || "").toLowerCase().includes(q)) });
      }
      if (path === "/v1/rooms") return response({ rooms: parsed.searchParams.get("org_id") ? rooms : publicRooms });
      if (path === "/v1/groups" && method === "GET") return response({ groups: server.groups });
      const m = /^\/v1\/groups\/([^/]+)(\/.*)?$/.exec(path);
      if (m) {
        const id = decodeURIComponent(m[1]);
        if (m[2] === "/join" || m[2] === "/invitation") {
          const room = [...rooms, ...publicRooms].find((r) => r.id === id);
          server.groups.push(group({ id, org_id: room && rooms.includes(room) ? ORG : null, name: room ? room.name : id, my: { ...group().my, state: "keying" } }));
          return response({ ok: true });
        }
        if (m[2] === "/members") return response({ members: [] });
        if (m[2] === "/read") return response({ ok: true });
      }
      if (path.startsWith("/calls/")) return response({ active: false });
      return response({ error: "not_found" }, 404);
    }
    return base(url, opts);
  };
  const errors = [], console = new VirtualConsole();
  console.on("jsdomError", (e) => errors.push(e.message));
  const dom = new JSDOM(read("index.html"), { url: "https://inbox.pigeonpost.dev/", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: console });
  const w = dom.window, $ = (id) => w.document.getElementById(id);
  Object.defineProperty(w, "crypto", { value: webcrypto });
  w.matchMedia = (query) => ({ matches: query.includes("min-width"), addEventListener() {} });
  w.fetch = server.fetch;
  w.localStorage.setItem("ppi_token", "fixture_account");
  for (const f of ["names.js", "config.js", "crop.js", "groups-core.js", "orgs.js"]) w.eval(read(f));
  w.PigeonpostGroupsCore.createClient = () => fakeClient(server);
  w.PigeonpostGroupCryptoLoader = async () => ({ fingerprintDigits: () => "0".repeat(30) });
  w.eval(read("groups.js"));
  w.eval(read("app.js"));
  t.after(async () => { await tick(120); w.close(); assert.deepEqual(errors, [], "No uncaught script errors"); });
  await until(() => $("identity-btn").dataset.address === "/bekir/main");
  await tick(80);
  return { w, $, server };
}

// ---- the pure rules ------------------------------------------------------------------------------

const ctx = { window: {} };
vm.runInNewContext(read("orgs.js"), ctx);
const O = ctx.window.PigeonpostOrgs;
const P = O._parse;
const plain = (v) => JSON.parse(JSON.stringify(v));

test("the catalog drops rows that break the contract and keeps good ones", () => {
  const good = orgRow();
  const { orgs } = P.parseCatalog({ orgs: [good, { ...good, id: "bad id" }, { ...good, id: "org_bidi", name: "Acme‮corp" }, { ...good }] });
  assert.equal(orgs.length, 1, "bad id, bidi name and the duplicate are dropped");
  assert.equal(orgs[0].name, "Acme Inc.");
  assert.deepEqual(plain(orgs[0].roles), ["member"]);
  assert.equal(orgs[0].mailbox.address, OWNER);
  assert.equal(P.parseCatalog(null).orgs.length, 0);
});

test("room creation, role labels and room actions follow the server's rules", () => {
  const [org] = P.parseCatalog({ orgs: [orgRow()] }).orgs;
  assert.equal(O.mayCreate(org, OWNER), true);
  assert.equal(O.mayCreate(org, "/k/someoneelse"), false, "only as the org mailbox");
  assert.equal(O.mayCreate({ ...org, status: "read_only" }, OWNER), false);
  assert.equal(O.mayCreate({ ...org, roomsCreation: "admins" }, OWNER), false);
  assert.equal(O.mayCreate({ ...org, roomsCreation: "admins", roles: ["people_admin"] }, OWNER), true);
  assert.equal(O.mayCreate({ ...org, roomsCreation: null }, OWNER), false);
  assert.equal(O.roleLabel({ roles: ["member", "admin"] }), "Admin");
  assert.equal(O.roleLabel({ roles: [] }), "Member");
  const rooms = P.parseRooms({ rooms: [
    { id: "grp_a", name: "General", description: null, member_count: 3, join_policy: "public", my_state: null },
    { id: "grp_b", name: "Broken", member_count: -1, join_policy: "public" },
  ] });
  assert.equal(rooms.length, 1);
  assert.equal(rooms[0].memberCount, 3);
  const r = (over) => ({ ...rooms[0], ...over });
  assert.equal(O.roomAction(r({ myState: "active" }), org), "open");
  assert.equal(O.roomAction(r({}), org), "join");
  assert.equal(O.roomAction(r({ myState: "invited" }), org), "accept");
  assert.equal(O.roomAction(r({ joinPolicy: "private" }), org), "unavailable");
  assert.equal(O.roomAction(r({}), { ...org, status: "read_only" }), "unavailable");
  assert.equal(P.parseDirectory({ entries: [{ handle: "/acme/jane", address: "/k/jane", kind: "member", name: "Jane" }, { handle: "/acme/x", address: "/k/x", kind: "bot" }] }).length, 1);
});

// ---- in the inbox ------------------------------------------------------------------------------

test("without `orgs` nothing org-shaped appears and /v1/orgs is never asked", async (t) => {
  const a = await app(t, { features: ["groups"] });
  await until(() => a.$("groups-section"));
  assert.equal(a.$("company-heading"), null);
  assert.equal(a.$("browse-rooms-btn"), null);
  assert.equal(a.$("me-org").hidden, true);
  assert.equal(a.server.ocalls.filter((c) => c.path === "/v1/orgs").length, 0);
});

test("an org whose flag is off for its owner (404) shows nothing either", async (t) => {
  const a = await app(t, { orgsStatus: 404 });
  await until(() => a.server.ocalls.some((c) => c.path === "/v1/orgs"));
  await tick(60);
  assert.equal(a.$("company-heading"), null);
  assert.equal(a.$("me-org").hidden, true);
});

test("the org mailbox shows its org and role, and the switcher badges it", async (t) => {
  const a = await app(t, { orgs: [orgRow({ my_roles: ["member", "admin"] })] });
  await until(() => !a.$("me-org").hidden);
  assert.equal(a.$("me-org").textContent, "Acme Inc. · Admin");
  assert.match(a.$("identity-btn").getAttribute("aria-label"), /Acme Inc\./);
  const badges = [...a.w.document.querySelectorAll("#identity-menu .mi-org")];
  assert.equal(badges.length, 1, "only the org mailbox carries a badge");
  assert.equal(badges[0].textContent, "Acme Inc.");
});

test("org rooms sit under Company with Browse rooms; personal groups stay above", async (t) => {
  const a = await app(t, { groups: [group(), group({ id: "grp_generalgeneralgeneralgen", org_id: ORG, name: "General" })] });
  await until(() => a.$("company-heading"));
  const items = [...a.$("groups-list").children];
  const names = items.map((li) => li.querySelector(".tr-name, h3")?.textContent);
  assert.deepEqual(names, ["Garden club", "Company · Acme Inc.", "General", "Browse rooms"]);
});

test("Browse rooms lists the org directory as the org mailbox; Join, Accept and Open act on it", async (t) => {
  const rooms = [
    { id: "grp_designdesigndesigndesign", name: "Design", description: "Reviews on Fridays", member_count: 4, join_policy: "public", my_state: null },
    { id: "grp_boardboardboardboardboar", name: "Board", description: null, member_count: 2, join_policy: "private", my_state: "invited" },
    { id: "grp_secretsecretsecretsecret", name: "Secret", description: null, member_count: 2, join_policy: "private", my_state: null },
  ];
  const a = await app(t, { rooms });
  await until(() => a.$("browse-rooms-btn"));
  a.$("browse-rooms-btn").click();
  await until(() => a.w.document.querySelectorAll(".gb-room").length === 3);
  assert.equal(a.$("group-browse-sheet").hidden, false);
  assert.equal(a.$("group-browse-sheet-title").textContent, "Rooms in Acme Inc.");
  const listing = a.server.ocalls.find((c) => c.path === "/v1/rooms");
  assert.equal(listing.query.get("org_id"), ORG);
  assert.equal(listing.identity, OWNER, "asked as the org mailbox");
  const buttons = [...a.w.document.querySelectorAll(".gb-room button")].map((b) => b.textContent);
  assert.deepEqual(buttons, ["Join", "Accept invitation"], "a private room without an invitation has no button");
  assert.equal(a.$("gb-create").hidden, false, "everyone may create rooms in this org");

  a.w.document.querySelector('.gb-room[data-room="grp_designdesigndesigndesign"] button').click();
  await until(() => a.server.ocalls.some((c) => c.path.endsWith("/join")));
  await until(() => a.$("group-browse-sheet").hidden && !a.$("group-view").hidden);
  assert.equal(a.$("group-head-name").textContent, "Design");

  // Search is passed through, trimmed.
  a.$("browse-rooms-btn").click();
  a.$("gb-q").value = "  boa ";
  a.$("gb-q").dispatchEvent(new a.w.Event("input"));
  await until(() => a.server.ocalls.some((c) => c.path === "/v1/rooms" && c.query.get("q") === "boa"));
  await until(() => a.w.document.querySelector('.gb-room[data-room="grp_boardboardboardboardboar"] button'));
  a.w.document.querySelector('.gb-room[data-room="grp_boardboardboardboardboar"] button').click();
  await until(() => a.server.ocalls.some((c) => c.path.endsWith("/invitation")));
  assert.deepEqual(a.server.ocalls.find((c) => c.path.endsWith("/invitation")).body, { accept: true });
});

test("a member creates an org room from the browser; admins-only orgs hide the form from members", async (t) => {
  const a = await app(t);
  await until(() => a.$("browse-rooms-btn"));
  a.$("browse-rooms-btn").click();
  await until(() => !a.$("gb-create").hidden);
  a.$("gb-create-btn").click();
  await tick();
  assert.equal(a.$("gb-error").hidden, false, "a name is required");
  a.$("gb-name").value = "Launch";
  a.$("gb-desc").value = "Shipping the thing";
  a.w.document.querySelector('input[name="gb-join"][value="private"]').checked = true;
  a.$("gb-listed").checked = false;
  a.$("gb-create-btn").click();
  await until(() => a.server.created.length === 1);
  assert.deepEqual(plain(a.server.created[0]), { org_id: ORG, name: "Launch", description: "Shipping the thing", join_policy: "private", post_policy: "everyone", visibility: "unlisted", invite: [] });
  await until(() => a.$("group-browse-sheet").hidden && !a.$("group-view").hidden);
  assert.equal(a.$("group-head-name").textContent, "Launch");

  const b = await app(t, { orgs: [orgRow({ settings: { rooms_creation: "admins" } })] });
  await until(() => b.$("browse-rooms-btn"));
  b.$("browse-rooms-btn").click();
  await until(() => b.server.ocalls.some((c) => c.path === "/v1/rooms"));
  assert.equal(b.$("gb-create").hidden, true);
});

test("composing as the org mailbox suggests colleagues from the org directory", async (t) => {
  const a = await app(t, { directory: [
    { handle: "/acme/jane", address: "/k/jane", kind: "member", name: "Jane Doe" },
    { handle: "/acme/build", address: "/k/build", kind: "agent", name: "CI" },
    { handle: "/bekir/main", address: OWNER, kind: "member", name: "Me" },
  ] });
  await until(() => !a.$("me-org").hidden);
  a.$("new-btn").click();
  await until(() => a.$("new-peer-org").children.length === 2);
  assert.equal(a.$("new-peer").getAttribute("list"), "new-peer-org");
  const options = [...a.$("new-peer-org").children].map((o) => [o.value, o.label]);
  assert.deepEqual(options, [["/acme/jane", "Jane Doe · Acme Inc."], ["/acme/build", "CI · Agent"]], "yourself is left out");
  a.$("new-peer").value = "/acme/ja";
  a.$("new-peer").dispatchEvent(new a.w.Event("input"));
  await until(() => a.server.ocalls.some((c) => c.path.endsWith("/directory") && c.query.get("q") === "ja"));
  await until(() => a.$("new-peer-org").children.length === 1);
});

test("a colleague's profile names the organization", async (t) => {
  const a = await app(t, { directory: [{ handle: PEER, address: "/k/alp", kind: "member", name: "Alp" }] });
  await until(() => !a.$("me-org").hidden);
  await until(() => a.$("threads").querySelector(".thread-row"));
  const row = [...a.$("threads").querySelectorAll(".thread-row")].find((r) => /alp/i.test(r.textContent));
  assert.ok(row, "the fixture thread is listed");
  row.click();
  await tick(60);
  a.$("peer-info-btn").click();
  await until(() => a.$("peer-facts").querySelector("[data-org]"));
  assert.equal(a.$("peer-facts").querySelector("[data-org]").textContent, "Acme Inc. · Member");
});

test("listed public groups open from New group without `orgs`", async (t) => {
  const a = await app(t, { features: ["groups"], publicRooms: [{ id: "grp_birdsbirdsbirdsbirdsbirds", name: "Birdwatchers", description: "All welcome", member_count: 40, join_policy: "public", my_state: null }] });
  await until(() => a.$("new-group-btn") && !a.$("new-group-btn").hidden);
  a.$("new-group-btn").click();
  a.$("gc-browse").click();
  await until(() => a.w.document.querySelector(".gb-room"));
  assert.equal(a.$("group-browse-sheet-title").textContent, "Public groups");
  const listing = a.server.ocalls.find((c) => c.path === "/v1/rooms");
  assert.equal(listing.query.has("org_id"), false, "the personal directory");
  assert.equal(a.$("gb-create").hidden, true, "groups are created from New group");
  assert.match(a.w.document.querySelector(".gb-room").textContent, /40 members · Public/);
  a.w.document.querySelector(".gb-room button").click();
  await until(() => a.server.ocalls.some((c) => c.path.endsWith("/join")));
});
