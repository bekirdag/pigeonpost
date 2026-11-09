// Pigeonpost for teams — interface language (SDS D-05: English first, then Turkish).
//
// The English text in the HTML and the scripts is the source; each language is a dictionary from
// that English text to its translation (i18n-tr.js). Loaded synchronously in <head>, after the
// dictionaries and before any page script, so it can watch the document as it is built:
//  - every text node and the few attributes people read (aria-label, placeholder, title, alt,
//    aria-valuetext, the page description) are swapped for their translation when one exists;
//  - text built from a template ("{1} of {2} seats used") matches by pattern, and the parts that
//    filled the holes are translated too when they are themselves interface words;
//  - window.confirm() questions go through the same table.
// Anything without an entry stays as it is, so a missing translation shows English, never a gap.
// Text inside translate="no", code, pre, textareas and the iyzico payment form is never touched.
//
// The language is ?lang=en|tr (remembered), then the remembered choice, then the browser's
// languages (any Turkish one picks Turkish), then English. Nothing here parses HTML.
(function () {
  "use strict";
  var KEY = "ppe_lang";
  var SUPPORTED = { en: true, tr: true };
  var DICTS = { tr: window.PPE_STRINGS_TR || {} };

  function remembered() { try { return window.localStorage.getItem(KEY); } catch (e) { return null; } }
  function remember(lang) { try { window.localStorage.setItem(KEY, lang); } catch (e) { /* storage blocked */ } }

  /// Pure: which language to use. Exported for tests.
  function pick(query, stored, languages) {
    var q = String(query || "").toLowerCase();
    if (SUPPORTED[q]) return q;
    if (SUPPORTED[stored]) return stored;
    var list = languages || [];
    for (var i = 0; i < list.length; i++) {
      var l = String(list[i] || "").toLowerCase();
      if (l === "tr" || l.indexOf("tr-") === 0) return "tr";
      if (l === "en" || l.indexOf("en-") === 0) return "en";
    }
    return "en";
  }

  var params = null;
  try { params = new URLSearchParams(window.location.search); } catch (e) { params = null; }
  var asked = params ? params.get("lang") : null;
  var nav = window.navigator || {};
  var lang = pick(asked, remembered(), nav.languages && nav.languages.length ? nav.languages : [nav.language]);
  if (asked && SUPPORTED[String(asked).toLowerCase()]) {
    remember(lang);
    // Drop ?lang= from the address bar and keep everything else the page reads.
    try {
      params.delete("lang");
      var rest = params.toString();
      window.history.replaceState(window.history.state, "", window.location.pathname + (rest ? "?" + rest : "") + window.location.hash);
    } catch (e) { /* leave the address as it is */ }
  }
  document.documentElement.setAttribute("lang", lang);

  // ---- the table -------------------------------------------------------------------------------

  function compile(dict) {
    var exact = Object.create(null);
    var byKey = Object.create(null);
    var patterns = [];
    Object.keys(dict).forEach(function (en) {
      byKey[en.replace(/\{#(\d+)\}/g, "{$1}")] = dict[en];
      if (/\{#?\d+\}/.test(en)) {
        var holes = [];
        var source = en.split(/(\{#?\d+\})/).map(function (part) {
          var m = /^\{(#?)(\d+)\}$/.exec(part);
          if (m) { holes.push(Number(m[2])); return m[1] ? "(\\d[\\d.,]*)" : "([\\s\\S]*?)"; }
          return part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
        }).join("");
        patterns.push({ re: new RegExp("^" + source + "$"), holes: holes, to: dict[en], literal: en.replace(/\{#?\d+\}/g, "").length });
      } else {
        exact[en] = dict[en];
      }
    });
    // Longest literal text first, so "Delete {1}?" is not taken for "{1}?".
    patterns.sort(function (a, b) { return b.literal - a.literal; });
    return { exact: exact, byKey: byKey, patterns: patterns };
  }
  var table = lang === "en" ? null : compile(DICTS[lang] || {});

  /// Translate one English string, or a template by its key ("{1} seats") with `vars` filling the
  /// holes. Exported as PPE_I18N.t.
  function t(text, vars) {
    var key = String(text);
    var out = table && table.byKey[key] !== undefined ? table.byKey[key] : translate(key);
    if (vars) out = out.replace(/\{(\d+)\}/g, function (m, n) { return vars[n] !== undefined ? String(vars[n]) : m; });
    return out;
  }

  function translate(text) {
    if (!table) return text;
    var trimmed = text.trim();
    if (!trimmed) return text;
    var lead = text.slice(0, text.indexOf(trimmed));
    var tail = text.slice(lead.length + trimmed.length);
    var hit = table.exact[trimmed];
    if (hit !== undefined) return lead + hit + tail;
    for (var i = 0; i < table.patterns.length; i++) {
      var p = table.patterns[i];
      var m = p.re.exec(trimmed);
      if (!m) continue;
      var values = {};
      p.holes.forEach(function (n, k) { values[n] = hole(m[k + 1]); });
      return lead + p.to.replace(/\{(\d+)\}/g, function (s, n) { return values[n] !== undefined ? values[n] : s; }) + tail;
    }
    return text;
  }
  // A hole's own text is translated too when it is interface text ("Admin", " on 4 Nov 2026").
  // Each hole is shorter than the text it came from, so this always ends.
  function hole(value) { return translate(value); }

  // ---- the document ----------------------------------------------------------------------------

  var ATTRS = ["aria-label", "placeholder", "title", "alt", "aria-valuetext"];
  var SKIP = { SCRIPT: 1, STYLE: 1, CODE: 1, PRE: 1, NOSCRIPT: 1 };

  function skipped(node) {
    for (var n = node; n && n.nodeType === 1; n = n.parentNode) {
      if (SKIP[n.nodeName] || n.getAttribute("translate") === "no") return true;
    }
    return false;
  }
  function textNode(node) {
    var parent = node.parentNode;
    // A textarea's text is what someone typed.
    if (!parent || parent.nodeName === "TEXTAREA" || skipped(parent)) return;
    var next = translate(node.data);
    if (next !== node.data) node.data = next;
  }
  function attributes(el) {
    if (skipped(el)) return;
    for (var i = 0; i < ATTRS.length; i++) {
      var v = el.getAttribute(ATTRS[i]);
      if (v) { var next = translate(v); if (next !== v) el.setAttribute(ATTRS[i], next); }
    }
    if (el.nodeName === "META" && el.getAttribute("name") === "description") {
      var c = el.getAttribute("content");
      var tc = c && translate(c);
      if (tc && tc !== c) el.setAttribute("content", tc);
    }
  }
  function walk(root) {
    if (!root) return;
    if (root.nodeType === 3) { textNode(root); return; }
    if (root.nodeType !== 1 && root.nodeType !== 9 && root.nodeType !== 11) return;
    if (root.nodeType === 1) { if (skipped(root)) return; attributes(root); }
    var kids = root.childNodes;
    for (var i = 0; i < kids.length; i++) walk(kids[i]);
  }

  if (table && window.MutationObserver) {
    var observer = new window.MutationObserver(function (records) {
      for (var i = 0; i < records.length; i++) {
        var r = records[i];
        if (r.type === "childList") { for (var j = 0; j < r.addedNodes.length; j++) walk(r.addedNodes[j]); }
        else if (r.type === "characterData") textNode(r.target);
        else if (r.type === "attributes") attributes(r.target);
      }
    });
    observer.observe(document.documentElement, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ATTRS.concat(["content"]) });
    walk(document.documentElement);
    document.addEventListener("DOMContentLoaded", function () { walk(document.documentElement); });

    var confirmEnglish = window.confirm;
    window.confirm = function (message) { return confirmEnglish.call(window, translate(String(message))); };
  }

  // ---- the switch ------------------------------------------------------------------------------

  function addSwitch() {
    var top = document.querySelector("header.top");
    if (!top || document.getElementById("ppe-lang")) return;
    // Not on the sign-in callback or the payment pages: reloading those would repeat a step.
    if (/^\/(auth|checkout)(\/|$)/.test(window.location.pathname)) return;
    var other = lang === "tr" ? "en" : "tr";
    var b = document.createElement("button");
    b.type = "button";
    b.id = "ppe-lang";
    b.className = "btn btn-quiet lang-switch";
    b.setAttribute("translate", "no");
    b.setAttribute("lang", other);
    b.textContent = other === "tr" ? "Türkçe" : "English";
    b.setAttribute("aria-label", other === "tr" ? "Türkçe kullan" : "Use English");
    b.addEventListener("click", function () { remember(other); window.location.reload(); });
    var account = document.getElementById("ppe-account");
    if (account && account.parentNode === top) top.insertBefore(b, account); else top.appendChild(b);
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", addSwitch); else addSwitch();

  window.PPE_I18N = {
    lang: lang,
    /// BCP 47 tag for Intl formatting of dates and money.
    locale: lang === "tr" ? "tr-TR" : "en",
    t: t,
    pick: pick,
    translate: translate,
  };
})();
