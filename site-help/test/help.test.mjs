// help.pigeonpost.dev: every article renders, every internal link resolves, old links still work,
// search finds what people type, and the page stays within its CSP (no inline styles, no HTML
// parsing of article text).
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { JSDOM, VirtualConsole } from "jsdom";

const here = new URL("../", import.meta.url);
const read = (p) => readFileSync(new URL(p, here), "utf8");
const html = read("index.html");
const scripts = [...html.matchAll(/<script src="\/([^"]+)"/g)].map((m) => m[1]);
const source = (p) => (p.startsWith("shared/") ? readFileSync(new URL(`../site-shared/${p.slice(7)}`, here), "utf8") : read(p));

function boot(t, hash = "") {
  const errors = [];
  const vc = new VirtualConsole();
  vc.on("jsdomError", (e) => errors.push(e));
  vc.on("error", (e) => errors.push(e));
  const dom = new JSDOM(html.replace(/<script[^>]*><\/script>/g, ""), { url: `https://help.pigeonpost.dev/${hash}`, runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: vc });
  t.after(() => dom.window.close());
  const w = dom.window;
  w.HTMLElement.prototype.scrollIntoView = function () { w.__scrolledTo = this.id; };
  w.scrollTo = () => {};
  for (const s of scripts) w.eval(source(s));
  return { w, doc: w.document, errors };
}
const go = (w, hash) => { w.location.hash = hash; w.PPHelp.route(); };

test("every script the page loads exists, and the content is well formed", (t) => {
  for (const s of scripts) assert.ok(s.startsWith("shared/") ? existsSync(new URL(`../site-shared/${s.slice(7)}`, here)) : existsSync(new URL(s, here)), s);
  const { w, errors } = boot(t);
  assert.deepEqual(errors, []);
  const H = w.PP_HELP;
  const slugs = H.articles.map((a) => a.slug);
  assert.equal(new Set(slugs).size, slugs.length, "slugs are unique");
  const cats = new Set(H.categories.map((c) => c.key));
  for (const a of H.articles) {
    assert.match(a.slug, /^[a-z0-9]+(-[a-z0-9]+)*$/, a.slug);
    assert.ok(cats.has(a.cat), `${a.slug}: category ${a.cat}`);
    assert.ok(a.title && a.summary && a.body.length, `${a.slug}: title, summary and body`);
    for (const r of a.related || []) assert.ok(slugs.includes(r), `${a.slug}: related ${r}`);
    for (const old of a.aliases || []) assert.ok(!slugs.includes(old), `${a.slug}: alias ${old} shadows an article`);
  }
  for (const c of H.categories) assert.ok(H.articles.some((a) => a.cat === c.key), `${c.key} has articles`);
  for (const s of H.popular) assert.ok(slugs.includes(s), `popular ${s}`);
  assert.ok(H.articles.length >= 60, `${H.articles.length} articles`);
});

test("every page renders, and every internal link on it resolves", (t) => {
  const { w, doc, errors } = boot(t);
  const H = w.PP_HELP;
  const bySlug = new Map(H.articles.map((a) => [a.slug, a]));
  const routes = ["#/", "#/contact", "#/search/refund", "#/search/zzzz-nothing", "#/a/no-such-article",
    ...H.categories.map((c) => `#/c/${c.key}`), ...H.articles.map((a) => `#/a/${a.slug}`)];
  const seen = new Set();
  const checked = new Set();
  const resolve = (href, from) => {
    const [page, p1, p2] = href.replace(/^#\//, "").split("/").map(decodeURIComponent);
    if (page === "" || page === "contact") return;
    if (page === "search") return assert.ok(p1, `${from}: empty search link`);
    if (page === "c") return assert.ok(H.categories.some((c) => c.key === p1), `${from}: category ${href}`);
    assert.equal(page, "a", `${from}: unknown route ${href}`);
    const a = bySlug.get(p1);
    assert.ok(a, `${from}: article ${href}`);
    if (p2) assert.ok(a.body.some((b) => Array.isArray(b) && b[0] === "h" && w.PPHelp.sectionId(b[1]) === p2), `${from}: section ${href}`);
  };
  for (const r of routes) {
    go(w, r);
    const h1 = doc.querySelector("#hc-root h1");
    assert.ok(h1 && h1.textContent.trim(), `${r}: a heading`);
    assert.ok(doc.title.endsWith("Pigeonpost Help") || doc.title.startsWith("Pigeonpost Help"), `${r}: title ${doc.title}`);
    if (r.startsWith("#/a/") && bySlug.has(r.slice(4))) {
      const a = bySlug.get(r.slice(4));
      assert.equal(h1.textContent, a.title);
      assert.equal(doc.title, `${a.title} · Pigeonpost Help`);
      assert.ok(doc.querySelector(".hc-prose").textContent.length > a.title.length + a.summary.length + 40, `${a.slug}: has a body`);
      assert.ok(!/\*\*|\]\(/.test(doc.querySelector(".hc-prose").textContent), `${a.slug}: inline markup left unparsed`);
      assert.equal(doc.querySelector('.hc-article__nav [aria-current="page"]').textContent, a.title);
      seen.add(a.slug);
    }
    for (const el of doc.querySelectorAll("[style]")) assert.fail(`${r}: inline style on <${el.tagName.toLowerCase()}> breaks the CSP`);
    for (const link of doc.querySelectorAll("a[href]")) {
      const href = link.getAttribute("href");
      if (href.startsWith("#/")) { const k = `${href}`; if (!checked.has(k)) { checked.add(k); resolve(href, r); } }
      else if (href.startsWith("#")) assert.ok(doc.getElementById(href.slice(1)), `${r}: fragment ${href}`);
      else assert.match(href, /^(https:\/\/([a-z]+\.)?pigeonpost\.dev\/|https:\/\/www\.npmjs\.com\/|mailto:[a-z]+@pigeonpost\.dev$)/, `${r}: external link ${href}`);
    }
    assert.ok(!doc.querySelector("a[href='#']"), `${r}: dead link`);
  }
  assert.equal(seen.size, H.articles.length, "every article rendered");
  assert.ok(checked.size > 100, `${checked.size} internal links checked`);
  assert.deepEqual(errors, []);
});

test("section links scroll to their heading, and every multi-section article has a table of contents", (t) => {
  const { w, doc } = boot(t);
  for (const a of w.PP_HELP.articles) {
    const heads = a.body.filter((b) => Array.isArray(b) && b[0] === "h");
    if (heads.length < 2) continue;
    go(w, `#/a/${a.slug}`);
    const toc = [...doc.querySelectorAll(".hc-toc a")];
    assert.equal(toc.length, heads.length, a.slug);
    const id = w.PPHelp.sectionId(heads[1][1]);
    go(w, `#/a/${a.slug}/${id}`);
    assert.equal(w.__scrolledTo, id, `${a.slug}: scrolled to ${id}`);
    assert.equal(doc.activeElement.id, id);
    const ids = heads.map((h) => w.PPHelp.sectionId(h[1]));
    assert.equal(new Set(ids).size, ids.length, `${a.slug}: section ids are unique`);
  }
});

test("old links keep working: audiences and the first help centre's article slugs", (t) => {
  const { w } = boot(t, "#/for/people");
  assert.equal(w.location.hash, "#/c/getting-started");
  for (const [aud, cat] of [["organizations", "enterprise"], ["partners", "partners"]]) {
    go(w, `#/for/${aud}`);
    assert.equal(w.location.hash, `#/c/${cat}`);
  }
  const old = ["sign-in", "two-factor", "handles", "inboxes", "credits", "nests", "services", "delete-account", "calls",
    "org-setup", "org-people", "org-billing", "become-partner", "publish-service", "pricing-payouts", "partner-analytics"];
  for (const slug of old) {
    go(w, `#/a/${slug}`);
    if (w.location.hash !== `#/a/${slug}`) w.PPHelp.route();
    assert.ok(w.document.querySelector(".hc-prose h1"), `old link #/a/${slug} still opens an article (${w.location.hash})`);
  }
});

test("search finds articles by what people type", (t) => {
  const { w, doc } = boot(t);
  const top = (q, n = 3) => w.PPHelp.search(q).slice(0, n).map((a) => a.slug);
  assert.ok(top("refund").includes("credits"));
  assert.ok(top("group call").includes("group-calls"));
  assert.ok(top("handle price").includes("handles"));
  assert.ok(top("delete my account").includes("delete-account"));
  assert.ok(top("2fa").includes("two-factor"));
  assert.ok(top("inbox full").includes("inbox-full"));
  assert.ok(top("payout").includes("pricing-payouts"));
  assert.equal(w.PPHelp.search("   ").length, 0);
  // The home search box routes to a results page.
  go(w, "#/");
  const form = doc.querySelector(".hc-hero form");
  form.querySelector("input").value = "meeting notes";
  form.dispatchEvent(new w.Event("submit", { cancelable: true }));
  assert.equal(w.location.hash, "#/search/meeting%20notes");
  w.PPHelp.route();
  assert.ok(doc.querySelector(".hc-rows a[href='#/a/meeting-notes']"));
});

test("article text is never parsed as HTML, and every icon exists in the kit sprite", (t) => {
  const js = read("help.js");
  assert.ok(!/innerHTML|outerHTML|insertAdjacentHTML|document\.write/.test(js));
  const sprite = read("kit/icons.svg");
  const { w } = boot(t);
  const names = new Set([...w.PP_HELP.categories.map((c) => c.icon), ...w.PP_HELP.contact.channels.map((c) => c.icon),
    ...[...js.matchAll(/icon\("([a-z0-9-]+)"/g)].map((m) => m[1]), ...[...js.matchAll(/"(alert-triangle|circle-check|info)"/g)].map((m) => m[1])]);
  for (const n of names) assert.ok(sprite.includes(`id="i-${n}"`), `icon ${n}`);
  for (const m of html.matchAll(/icons\.svg#i-([a-z0-9-]+)/g)) assert.ok(sprite.includes(`id="i-${m[1]}"`), m[1]);
});

test("the vendored design kit matches pigeonpost.dev's copy byte for byte", () => {
  for (const f of ["pp-ui.min.css", "icons.svg", "brand/logo_only_symbol_trim.png", "brand/logo_only_symbol_white_trim.png", "fonts/Geist-Variable.woff2"]) {
    assert.ok(readFileSync(new URL(`kit/${f}`, here)).equals(readFileSync(new URL(`../site/kit/${f}`, here))), f);
  }
});
