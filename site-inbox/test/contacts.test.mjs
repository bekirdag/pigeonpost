// Phase 10 in the web inbox (docs/contacts-and-phone.md): the pure rules in contacts.js — E.164, the
// discovery hash, vCard/CSV parsing, name precedence, the local alias store — and the screens built
// on them, driven in jsdom against wire shapes from the postbox contract. Every number and name here
// is fictional (+1 555 01xx, +44 7700 900xxx, +61 491 570 xxx).
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";
import { fixtureServer, response, OWNER, PEER } from "./fixtures.mjs";

const read = name => readFileSync(new URL("../" + name, import.meta.url), "utf8");
const tick = (ms = 30) => new Promise(resolve => setTimeout(resolve, ms));
async function until(predicate, what = "UI did not settle") {
  for (let n = 0; n < 500; n++) { if (predicate()) return; await tick(10); }
  assert.fail(what);
}

function load(window = {}) {
  new Function("window", read("contacts.js"))(window);
  return window.PigeonpostContacts;
}
const C = load();

// ---- pure rules ------------------------------------------------------------------------------

test("hash test vectors from the contract (§3.1)", async () => {
  assert.equal(await C.hashPhone("+15555550100"), "7c0dc19d3eae376a630cee358d0f8912");
  assert.equal(await C.hashPhone("+447700900123"), "5c4fd5bb0f19345b15863f865cb76cee");
  assert.equal(await C.hashPhone("+61491570156"), "511ebc743c8a1d1ea8be5541f2fee95d");
});

test("E.164 normalisation (§2)", () => {
  const table = [
    ["0532 123 45 67", "90", "+905321234567"],
    ["(555) 555-0100", "1", "+15555550100"],
    ["0044 7700 900123", "44", "+447700900123"],
    ["0044 7700 900123", "90", "+447700900123"],
    ["+1 555 555 0100 x12", "90", "+15555550100"],
    ["+1 (555) 555-0100 ext. 7", "1", "+15555550100"],
    ["tel:+1-555-555-0100;ext=5", "", "+15555550100"],
    ["1-555-555-0100", "1", "+15555550100"],
    ["555.555.0100", "+1", "+15555550100"],
    ["011 44 7700 900123", "1", "+447700900123"],
    ["07700 900123", "44", "+447700900123"],
    ["0491 570 156", "61", "+61491570156"],
    ["+44 (0)7700 900123", "1", "+447700900123"], // the UK habit of writing the trunk 0 in brackets
    ["555 0100", "", null],            // no country to put it in
    ["112", "44", null],               // short code
    ["0800-FLOWERS", "1", null],       // letters
    ["garbage", "1", null],
    ["", "1", null],
    [null, "1", null],
    ["+0123456789", "1", null],        // country codes do not start with 0
    ["+1234567890123456", "1", null],  // longer than 15 digits
  ];
  for (const [raw, cc, want] of table) assert.equal(C.normalizeE164(raw, cc), want, `${raw} with ${cc}`);
});

test("vCard: folded lines, several TELs, 2.1 quoted-printable, item1.TEL and tel: URIs", () => {
  const vcf = [
    "BEGIN:VCARD", "VERSION:3.0", "FN:Avery Ex", " ample", "N:Example;Avery;;;",
    "TEL;TYPE=cell:+1 555 555 0100", "TEL;TYPE=work,voice:(555) 555-0101", "END:VCARD",
    "BEGIN:VCARD", "VERSION:2.1", "N;ENCODING=QUOTED-PRINTABLE;CHARSET=UTF-8:M=C3=BCller;J=C3=B6rg;;;",
    "FN;ENCODING=QUOTED-PRINTABLE;CHARSET=UTF-8:J=C3=B6rg =", "M=C3=BCller",
    "TEL;CELL:+44 7700 900123", "END:VCARD",
    "BEGIN:VCARD", "VERSION:4.0", "N:Tester;Casey;;;", "item1.TEL;type=pref:+61 491 570 156",
    "item1.X-ABLabel:mobile", "TEL;VALUE=uri;TYPE=home:tel:+1-555-555-0102", "END:VCARD",
    "BEGIN:VCARD", "VERSION:3.0", "FN:Nobody\\, No Phone", "ORG:Example Org", "END:VCARD",
  ].join("\r\n");
  const cards = C.parseVcard(vcf);
  assert.deepEqual(JSON.parse(JSON.stringify(cards)), [
    { name: "Avery Example", phones: ["+1 555 555 0100", "(555) 555-0101"] },
    { name: "Jörg Müller", phones: ["+44 7700 900123"] },
    { name: "Casey Tester", phones: ["+61 491 570 156", "tel:+1-555-555-0102"] },
    { name: "Nobody, No Phone", phones: [] },
  ]);
  assert.equal(C.normalizeE164(cards[2].phones[1], "90"), "+15555550102");
  // LF-only files and tab folding work too.
  assert.equal(C.parseVcard("BEGIN:VCARD\nFN:Dana\n\tSample\nTEL:+15555550103\nEND:VCARD\n")[0].name, "DanaSample");
});

test("CSV: Google Contacts with ' ::: ', the newer Google columns, Outlook and a generic file", () => {
  const google = [
    "Name,Given Name,Family Name,Phone 1 - Type,Phone 1 - Value,Phone 2 - Type,Phone 2 - Value",
    '"Example, Avery",Avery,Example,Mobile,+1 555 555 0100 ::: +1 555 555 0101,Work,(555) 555-0102',
    'Blake Sample,Blake,Sample,Home,"+44 7700 900123",,',
  ].join("\r\n");
  assert.deepEqual(JSON.parse(JSON.stringify(C.parseCsv(google))), [
    { name: "Example, Avery", phones: ["+1 555 555 0100", "+1 555 555 0101", "(555) 555-0102"] },
    { name: "Blake Sample", phones: ["+44 7700 900123"] },
  ]);
  const google2024 = "First Name,Middle Name,Last Name,Phone 1 - Label,Phone 1 - Value\nCasey,J,Tester,Mobile,+61 491 570 156\n";
  assert.deepEqual(JSON.parse(JSON.stringify(C.parseCsv(google2024))), [{ name: "Casey J Tester", phones: ["+61 491 570 156"] }]);
  const outlook = [
    "First Name,Last Name,Business Phone,Home Phone,Mobile Phone,Primary Phone,E-mail Address",
    'Dana,Sample,(555) 555-0104,,"555-555-0105",,dana@example.invalid',
    '"Multi ""Line""",Person,,"",+44 7700 900124,,',
  ].join("\n");
  assert.deepEqual(JSON.parse(JSON.stringify(C.parseCsv(outlook))), [
    { name: "Dana Sample", phones: ["(555) 555-0104", "555-555-0105"] },
    { name: 'Multi "Line" Person', phones: ["+44 7700 900124"] },
  ]);
  const generic = "\ufeffcontact,Cell,Tel (work),Phone Type\n\"Eli\nSample\",+1 555 555 0106,+1 555 555 0107,mobile\n";
  assert.deepEqual(JSON.parse(JSON.stringify(C.parseCsv(generic))), [{ name: "", phones: ["+1 555 555 0106", "+1 555 555 0107"] }]);
});

test("name precedence (§6.3): alias > contact > ~display > derived", () => {
  const table = [
    [{ alias: "Mum", contactAlias: "Ops", displayName: "Alp", derived: "Alp Main" }, { text: "Mum", kind: "alias" }],
    [{ alias: null, contactAlias: "Ops", displayName: "Alp", derived: "Alp" }, { text: "Ops", kind: "contact" }],
    [{ alias: "  ", contactAlias: "", displayName: "Alp K", derived: "Alp" }, { text: "~Alp K", kind: "display" }],
    [{ alias: null, contactAlias: null, displayName: null, derived: "Bekir" }, { text: "Bekir", kind: "derived" }],
    [{ derived: "/k/abcdefgh…" }, { text: "/k/abcdefgh…", kind: "derived" }],
  ];
  for (const [input, want] of table) assert.deepEqual({ ...C.pickName(input) }, want, JSON.stringify(input));
});

test("name validation mirrors the server", () => {
  assert.deepEqual({ ...C.validateDisplayName("  Bekir   Dağ ") }, { ok: true, value: "Bekir Dağ" });
  for (const bad of ["", "   ", "a/b", "me@home", "Bob\u200b", "Bob\u202e", "x".repeat(41), "...", "Admin", "Pigeon Post Support", "pigeonpost fan"]) {
    assert.equal(C.validateDisplayName(bad).ok, false, JSON.stringify(bad));
  }
  assert.equal(C.validateAlias("Admin").ok, true, "aliases have no reserved words");
  assert.equal(C.validateAlias("x".repeat(64)).ok, true);
  assert.equal(C.validateAlias("x".repeat(65)).ok, false);
  assert.equal(C.validateAlias("a\u0000b").ok, false);
  assert.equal(C.inviteName("Bekir 🚀 <Dağ>"), "Bekir  Dağ".replace(/\s+/g, " "));
  assert.equal(C.inviteName("🚀"), "A friend");
  assert.equal(C.inviteName(null), "A friend");
  assert.equal(C.inviteText("Bekir"), "Pigeonpost: Bekir invited you to chat on Pigeonpost. Join: https://pigeonpost.dev/invite?c=… Reply STOP to opt out.");
});

test("alias store persists per account and survives storage that throws", () => {
  const data = new Map();
  const ls = { getItem: k => (data.has(k) ? data.get(k) : null), setItem: (k, v) => data.set(k, String(v)), removeItem: k => data.delete(k),
    get length() { return data.size; }, key: i => [...data.keys()][i] ?? null };
  const A = load({ localStorage: ls });
  const store = A.aliases("acct_1");
  assert.equal(store.set("/alp/main", "  Alp   from work "), "Alp from work");
  assert.equal(store.get("/alp/main"), "Alp from work");
  assert.deepEqual(JSON.parse(data.get("ppi_aliases_v1:acct_1")), { "/alp/main": "Alp from work" });
  assert.equal(A.aliases("acct_1").get("/alp/main"), "Alp from work", "a fresh store reads it back");
  assert.equal(A.aliases("acct_2").get("/alp/main"), null, "aliases are per account");
  assert.throws(() => store.set("/alp/main", "a/b"), /\//);
  store.set("/k/xyz", "Key person");
  store.remove("/alp/main");
  assert.deepEqual(store.all(), { "/k/xyz": "Key person" });
  A.clearAllAliases();
  assert.equal(data.size, 0);

  const throwing = {};
  Object.defineProperty(throwing, "localStorage", { get() { throw new Error("SecurityError"); } });
  const B = load(throwing).aliases("acct_1");
  assert.equal(B.get("/alp/main"), null);
  assert.equal(B.set("/alp/main", "Alp"), "Alp");
  assert.equal(B.get("/alp/main"), "Alp", "kept in memory when storage is off");
  const broken = { localStorage: { getItem() { throw new Error("x"); }, setItem() { throw new Error("quota"); }, removeItem() { throw new Error("x"); } } };
  const D = load(broken).aliases("acct_1");
  D.set("/alp/main", "Alp");
  assert.equal(D.get("/alp/main"), "Alp");
  D.remove("/alp/main");
  load(broken).clearAllAliases();
});

// ---- the page --------------------------------------------------------------------------------

const VCF = [
  "BEGIN:VCARD", "VERSION:3.0", "FN:Avery Example", "TEL;TYPE=cell:+1 555 555 0100", "END:VCARD",
  "BEGIN:VCARD", "VERSION:3.0", "FN:Blake Sample", "TEL:(555) 555-0123", "TEL:555.555.0123", "END:VCARD",
  "BEGIN:VCARD", "VERSION:3.0", "N:Tester;Casey;;;", "item1.TEL:+44 7700 900123", "END:VCARD",
  "BEGIN:VCARD", "VERSION:3.0", "FN:Shorty", "TEL:112", "END:VCARD",
].join("\r\n");
const FILE_NAMES = ["Avery", "Example", "Blake", "Sample", "Casey", "Tester", "Shorty"];
const FILE_NUMBERS = ["+1 555 555 0100", "15555550100", "5555550100", "(555) 555-0123", "5555550123", "555.555.0123",
  "+44 7700 900123", "447700900123", "7700900123"];

async function app(t, { features = [], configure = () => {}, storage = {} } = {}) {
  const server = fixtureServer();
  server.features = features;
  server.raw = [];                 // every URL and body the page sent, verbatim
  server.phones = [];
  server.ownName = null;
  server.profiles = {};
  server.avatars = {};
  server.directory = {};           // hash → match
  server.inviteAnswer = () => response({ status: "sent", invite_id: "inv_1" });
  const base = server.fetch;
  server.fetch = async (url, opts = {}) => {
    server.raw.push(String(url), typeof opts.body === "string" ? opts.body : "");
    const parsed = new URL(url, "https://postbox.pigeonpost.dev");
    const path = parsed.pathname, method = opts.method || "GET";
    const body = typeof opts.body === "string" ? JSON.parse(opts.body) : null;
    const record = () => server.calls.push({ path, method, body, query: parsed.searchParams });
    const own = path === "/v1/capabilities" || path.startsWith("/v1/phone") || path.startsWith("/v1/contacts/") || path.startsWith("/v1/profile");
    const intercepted = own ? await server.intercept?.({ path, method, body, query: parsed.searchParams }) : null;
    if (intercepted) { record(); return intercepted; }
    if (path === "/v1/capabilities") return response({ features: server.features });
    if (path === "/v1/phone") { record(); return response({ numbers: server.phones, max_numbers: 3 }); }
    if (path === "/v1/phone/verify") {
      record();
      const now = Math.floor(Date.now() / 1000);
      return response({ challenge_id: "pvc_" + "a".repeat(32), expires_at: now + 600, resend_after: now + 60, code_length: 6 }, 202);
    }
    if (path.startsWith("/v1/phone/verify/")) {
      record();
      if (body.code === "000000") return response({ error: "invalid_code", detail: "that code is not right", attempts_left: 4 }, 400);
      const number = { id: "pn_1", identity: OWNER, country_code: "44", masked: "+44 ••• ••• ••23", discoverable: body.discoverable, verified_at: 1 };
      server.phones = [number];
      return response({ number });
    }
    if (path.startsWith("/v1/phone/")) {
      record();
      const id = decodeURIComponent(path.split("/").pop());
      if (method === "DELETE") { server.phones = server.phones.filter(n => n.id !== id); return response({ deleted: true }); }
      const n = server.phones.find(x => x.id === id);
      Object.assign(n, body);
      return response({ number: n });
    }
    if (path === "/v1/contacts/discover") {
      record();
      const matches = body.hashes.filter(h => server.directory[h]).map(h => ({ hash: h, ...server.directory[h] }));
      return response({ matches, checked: body.hashes.length, new: body.hashes.length, remaining_today: 1000 - body.hashes.length });
    }
    if (path === "/v1/contacts/invites") { record(); return server.inviteAnswer(body); }
    if (path === "/v1/profile/name") {
      record();
      if (method === "PUT") { server.ownName = { display_name: body.display_name, visibility: body.visibility || "known", updated_at: 1 }; return response({ name: server.ownName }); }
      if (method === "DELETE") { server.ownName = null; return response({ deleted: true }); }
      return response({ name: server.ownName });
    }
    if (path === "/v1/profiles") {
      record();
      const keys = parsed.searchParams.get("addresses").split(",");
      return response({ profiles: Object.fromEntries(keys.map(k => [k, server.profiles[k] || null])) });
    }
    if (path === "/v1/me/entitlements") { record(); return response({ entitlements: [], plan: "free" }); }
    if (path === "/v1/avatars") { record(); return response({ avatars: {} }); }
    if (path === "/v1/profile/avatar") { record(); return response({ avatar: null }); }
    return base(url, opts);
  };
  configure(server);
  const errors = [], console = new VirtualConsole();
  console.on("jsdomError", e => errors.push(e.message));
  const dom = new JSDOM(read("index.html"), { url: "https://inbox.pigeonpost.dev/", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: console });
  const w = dom.window, $ = id => w.document.getElementById(id);
  Object.defineProperty(w, "crypto", { value: webcrypto });
  w.matchMedia = query => ({ matches: query.includes("min-width"), addEventListener() {} });
  w.fetch = server.fetch;
  w.URL.createObjectURL = () => "blob:fixture";
  w.URL.revokeObjectURL = () => {};
  w.localStorage.setItem("ppi_token", "fixture_account");
  for (const [k, v] of Object.entries(storage)) w.localStorage.setItem(k, v);
  for (const file of ["names.js", "config.js", "crop.js", "contacts.js", "app.js"]) w.eval(read(file));
  t.after(async () => { await tick(120); w.close(); assert.deepEqual(errors, [], "No uncaught script errors"); });
  await until(() => $("identity-btn").dataset.address === "/bekir/main");
  await tick(60);
  const called = (path, method = "GET") => server.calls.filter(c => c.path === path && c.method === method);
  const visible = id => { for (let el = $(id); el; el = el.parentElement) if (el.hidden) return false; return true; };
  const input = (id, value) => { $(id).value = value; $(id).dispatchEvent(new w.Event("input", { bubbles: true })); };
  const pick = (id, value) => { $(id).value = value; $(id).dispatchEvent(new w.Event("change", { bubbles: true })); };
  const chooseFile = (id, text, name) => {
    const file = new w.File([text], name, { type: name.endsWith(".vcf") ? "text/vcard" : "text/csv" });
    Object.defineProperty($(id), "files", { value: [file], configurable: true });
    $(id).dispatchEvent(new w.Event("change"));
  };
  const openSettingsPage = page => { $("settings-btn").click(); $("settings-nav-" + page).click(); };
  return { w, $, server, called, visible, input, pick, chooseFile, openSettingsPage };
}

test("every Phase 10 surface stays hidden while its flag is off", async t => {
  const a = await app(t, { features: [] });
  a.$("settings-btn").click();
  assert.equal(a.$("settings-nav-phone").hidden, true);
  assert.equal(a.$("settings-nav-profile").hidden, true);
  a.$("settings-nav-contacts").click();
  assert.equal(a.visible("discover-open"), false);
  assert.equal(a.$("name-prompt-sheet").hidden, true);
  await tick(60);
  for (const path of ["/v1/phone", "/v1/profile/name", "/v1/profiles", "/v1/contacts/discover"]) {
    assert.equal(a.server.calls.filter(c => c.path === path).length, 0, path);
  }
  // Local aliases need no flag.
  a.$("settings-close").click();
  a.$("threads").querySelector(".thread-row").click();
  await tick();
  a.$("peer-name-btn").click();
  assert.equal(a.visible("peer-alias-section"), true);
});

test("adding a phone number: country, code, verify — discoverable stays off unless ticked", async t => {
  const a = await app(t, { features: ["phone_numbers"] });
  a.openSettingsPage("phone");
  await until(() => /No phone number yet/.test(a.$("phone-list").textContent));
  assert.match(a.$("settings-sheet").textContent, /We keep your number encrypted and never show it to anyone\./);
  a.$("phone-add").click();
  assert.equal(a.visible("phone-sheet"), true);
  const countries = [...a.$("phone-country").options].map(o => o.value);
  for (const code of ["90", "1", "44", "49", "33", "31", "34", "39", "61", "91", "other"]) assert.ok(countries.includes(code), code);

  a.pick("phone-country", "other");
  assert.equal(a.visible("phone-country-other"), true);
  a.input("phone-country-other", "");
  a.input("phone-number", "07700 900123");
  a.$("phone-send").click();
  assert.match(a.$("phone-error").textContent, /country calling code/);
  a.pick("phone-country", "44");
  a.input("phone-number", "not a number");
  a.$("phone-send").click();
  assert.equal(a.called("/v1/phone/verify", "POST").length, 0, "nothing is sent for an unreadable number");
  a.input("phone-number", "07700 900123");
  a.$("phone-send").click();
  await until(() => a.called("/v1/phone/verify", "POST").length === 1);
  assert.deepEqual(a.called("/v1/phone/verify", "POST")[0].body, { phone: "+447700900123", identity: OWNER });
  await until(() => a.visible("phone-code"));
  assert.equal(a.$("phone-code").getAttribute("autocomplete"), "one-time-code");
  assert.equal(a.$("phone-code").getAttribute("inputmode"), "numeric");
  assert.equal(a.$("phone-discoverable").checked, false, "unchecked by default");
  assert.match(a.$("phone-discoverable").closest("label").textContent, /Let people who have my number find me on Pigeonpost/);
  assert.equal(a.$("phone-resend").disabled, true);
  assert.match(a.$("phone-resend").textContent, /Resend code in 0:(59|60|1:00)|Resend code in 1:00/);

  a.input("phone-code", "12");
  a.$("phone-verify").click();
  assert.match(a.$("phone-error").textContent, /6-digit/);
  a.input("phone-code", "000000");
  a.$("phone-verify").click();
  await until(() => !a.$("phone-error").hidden && /4 tries left/.test(a.$("phone-error").textContent));
  a.input("phone-code", "123456");
  a.$("phone-verify").click();
  await until(() => a.called("/v1/phone/verify/pvc_" + "a".repeat(32), "POST").length === 2);
  assert.deepEqual(a.called("/v1/phone/verify/pvc_" + "a".repeat(32), "POST")[1].body, { code: "123456", discoverable: false });
  await until(() => a.$("phone-sheet").hidden && /\+44 ••• ••• ••23/.test(a.$("phone-list").textContent));
  assert.equal(a.$("phone-add").hidden, true, "one number per mailbox");

  // Discoverable switch and unlink, each its own request.
  const toggle = a.$("phone-list").querySelector('input[type="checkbox"]');
  assert.equal(toggle.checked, false);
  toggle.checked = true;
  toggle.dispatchEvent(new a.w.Event("change"));
  await until(() => a.called("/v1/phone/pn_1", "PATCH").length === 1);
  assert.deepEqual(a.called("/v1/phone/pn_1", "PATCH")[0].body, { discoverable: true });
  [...a.$("phone-list").querySelectorAll("button")].find(b => b.textContent === "Unlink").click();
  assert.equal(a.visible("phone-unlink-sheet"), true);
  assert.equal(a.called("/v1/phone/pn_1", "DELETE").length, 0, "unlinking asks first");
  a.$("phone-unlink-confirm").click();
  await until(() => a.called("/v1/phone/pn_1", "DELETE").length === 1);
  await until(() => /No phone number yet/.test(a.$("phone-list").textContent));
});

test("verification errors read as sentences, and WebOTP is aborted when the sheet closes", async t => {
  let otp = null;
  const a = await app(t, { features: ["phone_numbers"], configure(server) {
    server.intercept = call => {
      if (call.path === "/v1/phone/verify" && call.body.phone === "+15555550199") return response({ error: "phone_not_supported" }, 422);
    };
  } });
  a.w.OTPCredential = function OTPCredential() {};
  Object.defineProperty(a.w.navigator, "credentials", { configurable: true, value: { get(options) {
    otp = options;
    return new Promise((_, reject) => options.signal.addEventListener("abort", () => reject(new Error("aborted"))));
  } } });
  a.openSettingsPage("phone");
  a.$("phone-add").click();
  a.pick("phone-country", "1");
  a.input("phone-number", "555 555 0199");
  a.$("phone-send").click();
  await until(() => !a.$("phone-error").hidden);
  assert.match(a.$("phone-error").textContent, /can’t be used with Pigeonpost/);
  a.input("phone-number", "555 555 0100");
  a.$("phone-send").click();
  await until(() => otp !== null);
  assert.deepEqual(JSON.parse(JSON.stringify(otp.otp)), { transport: ["sms"] });
  a.$("phone-cancel").click();
  assert.equal(otp.signal.aborted, true);
});

test("finding people: only 32-hex hashes leave the page, never a number or a name from the file", async t => {
  const a = await app(t, { features: ["contact_discovery", "sms_invites"], configure(server) {
    server.directory["7c0dc19d3eae376a630cee358d0f8912"] = { address: "/k/avery", handle: "/avery/main", display_name: "Avery" };
  } });
  a.openSettingsPage("contacts");
  assert.equal(a.visible("discover-open"), true);
  a.$("discover-open").click();
  assert.equal(a.visible("discover-sheet"), true);
  assert.match(a.$("discover-sheet").textContent, /Your contacts file never leaves this browser\. Only scrambled numbers are checked, and nothing is kept\./);
  assert.equal(a.$("discover-file").getAttribute("accept"), ".vcf,.csv,text/vcard,text/csv");
  a.pick("discover-country", "1");
  const before = a.server.raw.length;
  const keysBefore = Object.keys(a.w.localStorage).sort();
  a.chooseFile("discover-file", VCF, "contacts.vcf");
  await until(() => a.called("/v1/contacts/discover", "POST").length === 1 && !a.$("discover-found-section").hidden);

  const sent = a.called("/v1/contacts/discover", "POST")[0].body;
  assert.deepEqual(Object.keys(sent), ["hashes"]);
  assert.equal(sent.hashes.length, 3, "deduplicated: Blake's two spellings are one number; 112 is skipped");
  for (const h of sent.hashes) assert.match(h, /^[0-9a-f]{32}$/);
  assert.ok(sent.hashes.includes("7c0dc19d3eae376a630cee358d0f8912"));
  assert.ok(sent.hashes.includes("5c4fd5bb0f19345b15863f865cb76cee"));
  const outgoing = a.server.raw.slice(before).join("\n");
  for (const secret of [...FILE_NAMES, ...FILE_NUMBERS]) assert.equal(outgoing.includes(secret), false, `${secret} was sent`);
  for (const call of a.server.calls) {
    const text = JSON.stringify(call.body || {});
    for (const secret of [...FILE_NAMES, ...FILE_NUMBERS]) assert.equal(text.includes(secret), false, `${secret} in ${call.path}`);
  }
  assert.deepEqual(Object.keys(a.w.localStorage).sort(), keysBefore, "nothing from the file is stored");

  const found = a.$("discover-found").querySelectorAll("li");
  assert.equal(found.length, 1);
  assert.match(found[0].textContent, /Avery Example/);
  assert.match(found[0].textContent, /~Avery/);
  assert.match(found[0].textContent, /\/avery\/main/);
  const missing = [...a.$("discover-missing").querySelectorAll("li")].map(li => li.textContent);
  assert.equal(missing.length, 2);
  assert.ok(missing.some(t => t.includes("Blake Sample")) && missing.some(t => t.includes("Casey Tester")));
  assert.match(a.$("discover-status").textContent, /Checked 3 of 3 numbers\. 1 person is on Pigeonpost\. 1 couldn’t be read\. 997 new checks left today\./);
  assert.equal(a.$("discover-status").getAttribute("aria-live"), "polite");
  assert.equal([...a.$("discover-sheet").querySelectorAll("button")].some(b => /invite all/i.test(b.textContent)), false);

  // Start chat opens a conversation addressed by handle.
  [...found[0].querySelectorAll("button")].find(b => b.textContent === "Start chat").click();
  await until(() => a.$("discover-sheet").hidden && a.$("settings-sheet").hidden);
  assert.equal(a.$("peer-sub").textContent, "/avery/main");
  assert.equal(a.$("discover-found").children.length, 0, "the list is gone once the sheet closes");
  assert.equal(a.$("discover-file").value, "");
});

test("discovery sends at most 500 hashes per request and stops on a rate limit", async t => {
  let batches = 0;
  const a = await app(t, { features: ["contact_discovery"], configure(server) {
    server.intercept = call => {
      if (call.path !== "/v1/contacts/discover") return null;
      batches += 1;
      if (batches === 2) return { ok: false, status: 429, headers: { get: k => (k === "retry-after" ? "7200" : null) },
        json: async () => ({ error: "rate_limited", limit: "daily" }) };
      return null;
    };
  } });
  const rows = ["Name,Mobile Phone"];
  for (let i = 0; i < 1200; i++) rows.push(`Person ${i},+1 555 ${String(1000000 + i).slice(1, 4)} ${String(1000 + i).slice(-4)}`);
  a.openSettingsPage("contacts");
  a.$("discover-open").click();
  a.chooseFile("discover-file", rows.join("\n"), "big.csv");
  await until(() => !a.$("discover-error").hidden, "rate limit shown");
  const posts = a.server.calls.filter(c => c.path === "/v1/contacts/discover");
  assert.equal(posts.length, 2, "stopped after the refusal");
  assert.equal(posts[0].body.hashes.length, 500);
  assert.match(a.$("discover-error").textContent, /today\. Try again in 2 hours\./);
  assert.equal(a.$("discover-missing-section").hidden, true, "no invite list without sms_invites");
});

test("inviting needs a confirmation that shows the text, then sends confirm:true", async t => {
  const a = await app(t, { features: ["contact_discovery", "sms_invites", "phone_numbers"] });
  a.openSettingsPage("contacts");
  a.$("discover-open").click();
  a.pick("discover-country", "1");
  a.chooseFile("discover-file", VCF, "contacts.vcf");
  await until(() => a.$("discover-missing").querySelectorAll("li").length === 3);
  const row = [...a.$("discover-missing").querySelectorAll("li")].find(li => li.textContent.includes("Blake Sample"));
  row.querySelector("button").click();
  await until(() => a.visible("invite-sheet"));
  assert.equal(a.$("invite-text").textContent,
    "Pigeonpost: A friend invited you to chat on Pigeonpost. Join: https://pigeonpost.dev/invite?c=… Reply STOP to opt out.");
  await tick(60);
  assert.equal(a.called("/v1/contacts/invites", "POST").length, 0, "nothing is sent before Send invite");
  a.$("invite-cancel").click();
  assert.equal(a.called("/v1/contacts/invites", "POST").length, 0);

  row.querySelector("button").click();
  await until(() => a.visible("invite-sheet"));
  a.$("invite-send").click();
  await until(() => a.called("/v1/contacts/invites", "POST").length === 1);
  assert.deepEqual(a.called("/v1/contacts/invites", "POST")[0].body, { phone: "+15555550123", confirm: true, identity: OWNER });
  await until(() => a.$("invite-sheet").hidden && /Invited/.test(a.$("discover-missing").textContent));

  // Without a verified number of your own, the answer points at the phone page.
  a.server.inviteAnswer = () => response({ error: "phone_required", detail: "verify a number first" }, 403);
  const casey = [...a.$("discover-missing").querySelectorAll("li")].find(li => li.textContent.includes("Casey Tester"));
  casey.querySelector("button").click();
  await until(() => a.visible("invite-sheet"));
  a.$("invite-send").click();
  await until(() => !a.$("invite-error").hidden);
  assert.equal(a.$("invite-error").textContent, "Verify your own number first.");
  assert.equal(a.visible("invite-verify"), true);
  a.$("invite-verify").click();
  await until(() => a.visible("phone-add"));
  assert.equal(a.$("settings-title").textContent, "Phone number");
});

test("display name: a one-time prompt, Skip remembered per mailbox, and Settings → Profile", async t => {
  const a = await app(t, { features: ["display_names"] });
  await until(() => a.visible("name-prompt-sheet"));
  assert.match(a.$("name-prompt-title").textContent, /What should people call you\?/);
  a.$("name-prompt-skip").click();
  assert.equal(a.$("name-prompt-sheet").hidden, true);
  assert.equal(a.w.localStorage.getItem("ppi_name_prompt_skipped:" + OWNER), "1");
  assert.equal(a.called("/v1/profile/name", "PUT").length, 0);

  a.$("settings-btn").click();
  assert.equal(a.$("settings-nav-profile").hidden, false, "names open Profile without avatars");
  a.$("settings-nav-profile").click();
  await until(() => a.called("/v1/profile/name").length >= 2);
  assert.equal(a.visible("profile-name-input"), true);
  assert.equal(a.visible("profile-pick"), false, "no picture controls without the avatars flag");
  a.input("profile-name-input", "Bekir/Admin");
  a.$("profile-name-save").click();
  await until(() => !a.$("profile-name-error").hidden);
  assert.match(a.$("profile-name-error").textContent, /\//);
  assert.equal(a.called("/v1/profile/name", "PUT").length, 0, "validated before sending");
  const everyone = a.$("profile-name-visibility").querySelector('input[value="everyone"]');
  everyone.checked = true; everyone.dispatchEvent(new a.w.Event("change"));
  a.input("profile-name-input", "  Bekir   D ");
  a.$("profile-name-save").click();
  await until(() => a.called("/v1/profile/name", "PUT").length === 1);
  const put = a.called("/v1/profile/name", "PUT")[0];
  assert.deepEqual(put.body, { display_name: "Bekir D", visibility: "everyone" });
  assert.equal(put.query.get("identity"), OWNER);
  await until(() => a.$("toast").textContent === "Saved.");
  a.$("profile-name-remove").click();
  await until(() => a.called("/v1/profile/name", "DELETE").length === 1);
});

test("the name prompt saves, and does not come back once skipped", async t => {
  const a = await app(t, { features: ["display_names"] });
  await until(() => a.visible("name-prompt-sheet"));
  a.input("name-prompt-input", "Bekir");
  a.$("name-prompt-save").click();
  await until(() => a.called("/v1/profile/name", "PUT").length === 1);
  assert.deepEqual(a.called("/v1/profile/name", "PUT")[0].body, { display_name: "Bekir", visibility: "known" });
  await until(() => a.$("name-prompt-sheet").hidden);

  const b = await app(t, { features: ["display_names"], storage: { ["ppi_name_prompt_skipped:" + OWNER]: "1" } });
  await tick(80);
  assert.equal(b.$("name-prompt-sheet").hidden, true);
  assert.equal(b.called("/v1/profile/name").length, 0);
});

test("peer display names show as ~Name, muted, and a local alias wins without any request", async t => {
  const a = await app(t, { features: ["display_names"], storage: { ["ppi_name_prompt_skipped:" + OWNER]: "1" }, configure(server) {
    server.profiles[PEER] = { address: "/k/alp", handle: PEER, display_name: "Alp K" };
  } });
  await until(() => a.$("threads").querySelector(".tr-name")?.textContent === "~Alp K");
  assert.ok(a.called("/v1/profiles")[0].query.get("addresses").split(",").includes(PEER));
  assert.equal(a.called("/v1/profiles")[0].query.has("identity"), false);
  const rowName = a.$("threads").querySelector(".tr-name");
  assert.equal(rowName.classList.contains("is-display-name"), true);
  a.$("threads").querySelector(".thread-row").click();
  await tick();
  assert.equal(a.$("peer-name").textContent, "~Alp K");
  assert.equal(a.$("peer-name").classList.contains("is-display-name"), true);
  assert.equal(a.$("peer-sub").textContent, PEER, "the subtitle is always the address or handle");

  a.$("peer-name-btn").click();
  assert.equal(a.visible("peer-alias-section"), true);
  const callsBefore = a.server.calls.length, rawBefore = a.server.raw.length;
  a.input("peer-alias-input", "Alp from work");
  a.$("peer-alias-save").click();
  await tick(60);
  const after = a.server.calls.slice(callsBefore);
  assert.equal(after.filter(c => c.method !== "GET").length, 0, "no write of any kind");
  assert.equal(after.filter(c => c.path === "/v1/contacts").length, 0);
  assert.equal(a.server.raw.slice(rawBefore).join("\n").includes("Alp from work"), false);
  assert.deepEqual(JSON.parse(a.w.localStorage.getItem("ppi_aliases_v1:local")), { [PEER]: "Alp from work" });
  assert.equal(a.$("peer-name").textContent, "Alp from work");
  assert.equal(a.$("peer-name").classList.contains("is-display-name"), false);
  assert.equal(a.$("threads").querySelector(".tr-name").textContent, "Alp from work");

  a.input("peer-alias-input", "bad@name");
  a.$("peer-alias-save").click();
  assert.equal(a.$("peer-alias-error").hidden, false);
  a.$("peer-alias-remove").click();
  assert.equal(a.$("peer-name").textContent, "~Alp K");
  assert.equal(a.w.localStorage.getItem("ppi_aliases_v1:local"), null);

  // A profile.name event is not wired in jsdom (no stream), but signing out clears aliases.
  a.input("peer-alias-input", "Alp again");
  a.$("peer-alias-save").click();
  a.$("peer-info-close").click();
  a.$("settings-btn").click();
  a.$("settings-nav-account").click();
  a.$("settings-signout").click();
  assert.equal(a.w.localStorage.getItem("ppi_aliases_v1:local"), null);
});

test("profile pictures depend only on the avatars flag, never on a plan", async t => {
  assert.equal(read("app.js").includes("/v1/me/entitlements"), false, "the inbox never asks for entitlements");
  const a = await app(t, { features: ["avatars"] });
  a.$("settings-btn").click();
  assert.equal(a.$("settings-nav-profile").hidden, false);
  a.$("settings-nav-profile").click();
  await until(() => a.called("/v1/profile/avatar").length === 1);
  assert.equal(a.visible("profile-pick"), true);
  assert.equal(a.$("profile-pick").disabled, false);
  assert.equal(a.visible("profile-name-input"), false, "no name field without display_names");
  assert.equal(a.called("/v1/me/entitlements").length, 0);
});

test("index.html loads contacts.js before app.js with this round's cache key", () => {
  const html = read("index.html");
  const contacts = html.indexOf('src="contacts.js?v=20261029-contacts1"');
  assert.ok(contacts > 0 && contacts < html.indexOf('src="app.js?v=20261029-contacts1"'));
  assert.ok(html.includes('href="app.css?v=20261029-contacts1"'));
});
