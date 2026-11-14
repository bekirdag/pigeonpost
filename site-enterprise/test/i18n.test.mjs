// Interface language (SDS D-05): English first, then Turkish. The Turkish table covers the pages'
// text, the language is chosen and remembered as documented, and names people typed are left alone.
import assert from "node:assert/strict";
import test from "node:test";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { load as loadPage, read, reply, tick, jwt } from "./helpers.mjs";

// jsdom keeps localStorage per origin across pages in one process; start each page without a
// remembered language unless the test sets one.
const load = (t, page, opts = {}) => loadPage(t, page, { ...opts, before: (w) => { w.localStorage.removeItem("ppe_lang"); if (opts.before) opts.before(w); } });

const root = new URL("../", import.meta.url).pathname;
const files = (dir) => readdirSync(dir).flatMap((f) => {
  const p = join(dir, f);
  if (["node_modules", "test", "fonts"].includes(f)) return [];
  return statSync(p).isDirectory() ? files(p) : [p];
});
// The shared platform shell (site-shared/, served at /shared/) draws part of the admin app, so its
// interface text is translated here too.
const shared = join(root, "../site-shared");
const sources = [...files(root), ...files(shared).filter((f) => /\/(core|shell|ui)\.js$/.test(f))]
  .filter((f) => (f.endsWith(".js") || f.endsWith(".html")) && !/i18n(-tr)?\.js$/.test(f));
const TR = (() => { const w = {}; new Function("window", read("i18n-tr.js"))(w); return w.PPE_STRINGS_TR; })();
const tr = (t, page, opts = {}) => load(t, page, { ...opts, url: (opts.url || `https://enterprise.pigeonpost.dev/${page === "index.html" ? "" : page.replace(/\.html$/, "")}`) + "?lang=tr" });

test("the language is ?lang=, then the remembered choice, then the browser's, then English", (t) => {
  const { window } = load(t, "index.html");
  const pick = window.PPE_I18N.pick;
  assert.equal(pick("tr", "en", ["en-US"]), "tr");
  assert.equal(pick("TR", null, []), "tr");
  assert.equal(pick("de", "tr", ["en-US"]), "tr");
  assert.equal(pick(null, null, ["de-DE", "tr-TR", "en"]), "tr");
  assert.equal(pick(null, null, ["en-GB", "tr"]), "en");
  assert.equal(pick(null, "fr", ["de"]), "en");
  assert.equal(pick(null, null, []), "en");
});

test("English stays the default and is untouched", async (t) => {
  const { window, $ } = load(t, "index.html", { fetch: () => reply(503, {}) });
  await tick();
  assert.equal(window.document.documentElement.lang, "en");
  assert.equal($("h1").textContent, "Pigeonpost for teams");
  assert.equal($("#summary").textContent, "30 people · up to 300 agent addresses · 150 GB pooled storage");
  assert.equal($("#ppe-lang").textContent, "Türkçe");
  assert.equal($("#ppe-lang").getAttribute("lang"), "tr");
});

test("?lang=tr shows the packages page in Turkish, remembers it and tidies the address", async (t) => {
  const { window, $ } = tr(t, "index.html", { fetch: () => reply(503, {}) });
  await tick();
  assert.equal(window.document.documentElement.lang, "tr");
  assert.equal(window.location.search, "");
  assert.equal(window.localStorage.getItem("ppe_lang"), "tr");
  assert.equal($("h1").textContent, "Ekipler için Pigeonpost");
  assert.equal(window.document.title, "Ekipler için Pigeonpost — kişiler ve ajanları tek gelen kutusunda");
  assert.equal($('[data-interval="monthly"]').textContent, "Aylık");
  assert.equal($(".seg").getAttribute("aria-label"), "Fatura dönemi");
  assert.match($('meta[name="description"]').getAttribute("content"), /^Şirket adresleri/);
  // Text the script writes later is translated as it appears, numbers and all.
  assert.equal($("#summary").textContent, "30 kişi · en fazla 300 ajan adresi · 150 GB ortak depolama");
  assert.equal($("#start").textContent, "30 kullanıcıyla başla");
  assert.equal($("#seats").getAttribute("aria-valuetext"), "30 kişi");
  assert.equal($("#ppe-lang").textContent, "English");
  // Money and dates follow the language.
  assert.equal(window.PPE.locale(), "tr-TR");
});

test("the remembered choice and a Turkish browser both pick Turkish", async (t) => {
  const remembered = load(t, "index.html", { before: (w) => w.localStorage.setItem("ppe_lang", "tr") });
  assert.equal(remembered.window.document.documentElement.lang, "tr");
  const browser = load(t, "index.html", { before: (w) => Object.defineProperty(w.navigator, "languages", { value: ["tr-TR", "en"], configurable: true }) });
  assert.equal(browser.window.document.documentElement.lang, "tr");
  assert.equal(browser.$("h1").textContent, "Ekipler için Pigeonpost");
});

test("the switch remembers the other language", async (t) => {
  const { window, $ } = load(t, "index.html");
  await tick();
  // jsdom does not navigate, so the reload that follows is a no-op here.
  $("#ppe-lang").click();
  assert.equal(window.localStorage.getItem("ppe_lang"), "tr");
});

test("no switch on the sign-in callback or the payment pages, and the payment form is never translated", async (t) => {
  const pay = tr(t, "checkout.html", { url: "https://enterprise.pigeonpost.dev/checkout" });
  const cb = tr(t, "auth/callback.html", { url: "https://enterprise.pigeonpost.dev/auth/callback" });
  const home = tr(t, "index.html", { fetch: () => reply(503, {}) });
  await tick();
  assert.ok(home.$("#ppe-lang"));
  assert.equal(pay.$("#ppe-lang"), null);
  assert.equal(cb.$("#ppe-lang"), null);
  assert.equal(pay.$("#pay-title").textContent, "Güvenli ödeme");
  const form = pay.$("#pay-form");
  form.replaceChildren(pay.document.createTextNode("Cancel"));
  await tick();
  assert.equal(form.textContent, "Cancel");
});

test("templates translate by pattern; number holes only match numbers; names stay as typed", async (t) => {
  const { window } = tr(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app" });
  const T = window.PPE_I18N.translate;
  assert.equal(T("12 of 30 seats used"), "Kullanılan kullanıcı: 12 / 30");
  assert.equal(T("24 of 300"), "24 / 300");
  assert.equal(T("Bank of America"), "Bank of America");
  assert.equal(T("Role: Admin"), "Rol: Yönetici");
  assert.equal(T("Payment of $30 failed on 4 Nov 2026."), "$30 tutarındaki ödeme başarısız oldu (tarih: 4 Nov 2026).");
  assert.equal(T("  Copy  "), "  Kopyala  ");
  assert.equal(T("$1,800 per year"), "Yıllık $1,800");
  assert.equal(T("30 seats: $1,800 per year, renewing until you cancel."), "30 kullanıcı: $1,800 / yıl, siz iptal edene kadar yenilenir.");
  // A template asked for by its key, as the change-plan picker does.
  assert.equal(window.PPE.t("{1} seats", { 1: 30 }), "30 kullanıcı");
  assert.equal(T("Something nobody wrote"), "Something nobody wrote");
  const named = window.document.createElement("div");
  named.setAttribute("translate", "no");
  named.textContent = "People";
  window.document.body.append(named);
  const plainDiv = window.document.createElement("div");
  plainDiv.textContent = "People";
  window.document.body.append(plainDiv);
  await tick();
  assert.equal(named.textContent, "People");
  assert.equal(plainDiv.textContent, "Kişiler");
});

test("confirm questions are asked in Turkish", (t) => {
  let asked = null;
  const { window } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app?lang=tr", before: (w) => { w.confirm = (m) => { asked = m; return true; }; } });
  window.confirm("Delete Acme Web? Members see \"This group was deleted\" and its messages are purged within 30 days.");
  assert.equal(asked, "Silinsin mi: Acme Web? Üyeler \"Bu grup silindi\" uyarısını görür ve mesajlar 30 gün içinde kalıcı olarak silinir.");
});

test("rule sentences are built from Turkish pieces", (t) => {
  const { window } = tr(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app" });
  const R = window.PPE.rules;
  assert.equal(R.sentence({ owners: "agents", peers: "*", admission: "block", locked: true }, "Acme", "acme"),
    "Ajanlar (Acme), şu göndericilerin mesajlarını engeller: herhangi biri. Ajanlar için kilitli.");
  assert.equal(R.sentence({ owners: "/acme/*", peers: "/acme/*", admission: "allow", known: true }, "Acme", "acme"),
    "Herkes (Acme), şu göndericilerin mesajlarını kabul eder: herkes (Acme) tanıdık kişi olarak.");
  const en = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app" });
  assert.equal(en.window.PPE.rules.sentence({ owners: "/acme/*", peers: "/acme/*", admission: "allow", known: true }, "Acme", "acme"),
    "Everyone at Acme accept messages from everyone at Acme as a known contact.");
});

test("the signed-out admin screen is in Turkish", async (t) => {
  const { $ } = tr(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app" });
  await tick();
  assert.match($(".pp-signin-card").textContent, /Kuruluşunuzu yönetmek için giriş yapın/);
  assert.match($(".pp-signin-actions").textContent, /Giriş yap/);
});

test("the signed-in admin app is in Turkish, with the language switch in the shell's top bar", async (t) => {
  const ORG = { id: "org_abc123", name: "Acme", namespace: "acme", status: "active", seats: 30, seats_used: 3, agent_limit: 300, agents_used: 1, plan_code: "ent-30-yearly-usd", settings: {}, created_at: 1800000000 };
  const fetch = (c) => {
    const p = new URL(c.url, "https://enterprise.pigeonpost.dev").pathname;
    if (p === "/v1/capabilities") return reply(200, { features: ["orgs", "enterprise_portal"] });
    if (p === "/v1/orgs") return reply(200, { orgs: [{ ...ORG, my_roles: ["owner"] }] });
    if (p === `/v1/orgs/${ORG.id}`) return reply(200, { org: ORG, me: { member_id: "m1", roles: ["owner"], mailbox: { handle: "/acme/ada" } } });
    if (p.endsWith("/members")) return reply(200, { members: [] });
    return reply(404, { error: "not_found" });
  };
  const { $, window } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app?lang=tr#/people", session: jwt({ name: "Ada Lovelace" }), fetch });
  await tick(80);
  assert.equal($("#pp-main h1").textContent, "Kişiler");
  assert.match($("#pp-main .pp-page-head p").textContent, /Kullanılan kullanıcı: 3 \/ 30/);
  assert.ok([...window.document.querySelectorAll(".pp-nav-item")].some((a) => a.textContent === "Genel bakış"));
  assert.equal($(".pp-top-actions #ppe-lang").textContent, "English");
  assert.equal($(".pp-user-name").textContent, "Ada Lovelace", "names are never translated");
});

test("the suspended banner with its deletion date is fully in Turkish", (t) => {
  const { window } = tr(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app" });
  const out = window.PPE_I18N.t("The organization is suspended: company addresses don't receive mail. Renew in Billing to restore it. If it isn't renewed, it will be deleted on 3 Aralık 2026, with its rooms and company addresses.");
  assert.match(out, /Yenilenmezse 3 Aralık 2026 tarihinde/);
  assert.doesNotMatch(out, /organization is suspended|If it isn't renewed/);
});

test("every Turkish entry still matches text in the pages, with the same placeholders", () => {
  const all = sources.map((f) => readFileSync(f, "utf8")).join("\n")
    .replace(/&amp;/g, "&").replace(/\\"/g, "\"").replace(/\\n/g, "\n");
  for (const [en, value] of Object.entries(TR)) {
    for (const piece of en.split(/\{#?\d+\}/)) {
      if (piece.trim().length >= 3) assert.ok(all.includes(piece), `no longer in the source: ${JSON.stringify(piece)} (key ${JSON.stringify(en)})`);
    }
    const holes = (s) => [...s.matchAll(/\{#?(\d+)\}/g)].map((m) => m[1]).sort().join(",");
    if (!/network\{2\}/.test(en)) assert.equal(holes(value), holes(en), `placeholders differ for ${JSON.stringify(en)}`);
    assert.doesNotMatch(value, /\{\d+\}'/, `suffix on a placeholder: ${value}`);
    assert.ok(value.trim(), `empty translation for ${JSON.stringify(en)}`);
  }
});

test("every interface sentence in the scripts has a Turkish entry", () => {
  // Double-quoted literals that read as interface text: a capital letter, then words. Templates
  // are covered by the source check above plus the page tests.
  const notInterface = new Set(["SHA-256", "Escape", "Enter", "Pigeonpost", "Türkiye", "Jane, Doe, jane@acme.com, jane",
    // Quoted inside a translated confirm() sentence, not shown on its own.
    "This group was deleted"]);
  const missing = [];
  for (const f of sources.filter((x) => x.endsWith(".js") && !/config\.js$/.test(x))) {
    const code = readFileSync(f, "utf8").split("\n").filter((l) => !/^\s*\/\//.test(l)).join("\n")
      .replace(/`(?:[^`\\]|\\.)*`/g, "``");
    for (const m of code.matchAll(/"((?:[^"\\\n]|\\.)*)"/g)) {
      const s = m[1].replace(/\\"/g, "\"").replace(/\\n/g, "\n").trim();
      if (!/^[A-Z][a-z]/.test(s) || !/[a-z]{2}/.test(s)) continue;
      if (notInterface.has(s) || s in TR) continue;
      missing.push(`${f.slice(root.length)}: ${s}`);
    }
  }
  assert.deepEqual(missing, []);
});
