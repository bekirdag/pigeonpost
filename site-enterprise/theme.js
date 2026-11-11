// Pigeonpost — appearance (System, Light or Dark), applied before the first paint.
//
// Loaded synchronously in <head>, ahead of the stylesheets, so a chosen theme never flashes the
// other one first. A file rather than an inline script because the CSP allows only 'self' scripts.
// "system" (or nothing stored) leaves `data-theme` off and the stylesheets follow the device;
// "light" and "dark" set it on <html>, and every stylesheet honours it over the media query.
// The key is the inbox's (site-inbox/theme.js), so the three sites agree on what is stored.
//
// The same file is served as pigeonpost.dev/theme.js (site/) and enterprise.pigeonpost.dev/theme.js
// (site-enterprise/); keep the two copies identical (a test checks). Any element carrying
// `data-theme-toggle` is the switch: a click cycles System → Light → Dark, and its label, title and
// `data-choice` say which one is in force. It draws its own icons when it is empty. A page that
// draws a switch after load calls PPTheme.refresh(); a click works either way.
(function () {
  "use strict";
  var KEY = "ppi_theme";
  var ORDER = ["system", "light", "dark"];
  var NAMES = { system: "System", light: "Light", dark: "Dark" };
  var ICONS = {
    system: ["M4 5h16v11H4z", "M9 20h6M12 16v4"],
    light: ["M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8Z", "M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"],
    dark: ["M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5Z"],
  };
  var root = document.documentElement;

  function stored() {
    var v = null;
    try { v = window.localStorage.getItem(KEY); } catch (e) { /* storage blocked */ }
    return v === "light" || v === "dark" ? v : "system";
  }
  function store(choice) {
    try {
      if (choice === "system") window.localStorage.removeItem(KEY);
      else window.localStorage.setItem(KEY, choice);
    } catch (e) { /* storage blocked: the choice lasts for this page only */ }
  }

  var current = stored();
  function next() { return ORDER[(ORDER.indexOf(current) + 1) % ORDER.length]; }

  // A <picture> that swaps its source on prefers-color-scheme follows the device, not the
  // attribute; point those sources at the choice (and back to their own query for System).
  function pictures() {
    var list = document.querySelectorAll("picture source[media]");
    for (var i = 0; i < list.length; i++) {
      var s = list[i];
      var own = s.getAttribute("data-theme-media");
      if (own === null) {
        own = s.getAttribute("media") || "";
        if (!/prefers-color-scheme:\s*dark/.test(own)) continue;
        s.setAttribute("data-theme-media", own);
      }
      s.setAttribute("media", current === "dark" ? "all" : current === "light" ? "not all" : own);
    }
  }

  function drawIcons(button) {
    var NS = "http://www.w3.org/2000/svg";
    ORDER.forEach(function (name) {
      var svg = document.createElementNS(NS, "svg");
      svg.setAttribute("class", "theme-icon theme-icon-" + name);
      svg.setAttribute("viewBox", "0 0 24 24");
      svg.setAttribute("aria-hidden", "true");
      svg.setAttribute("focusable", "false");
      ICONS[name].forEach(function (d) {
        var p = document.createElementNS(NS, "path");
        p.setAttribute("d", d);
        svg.appendChild(p);
      });
      button.appendChild(svg);
    });
  }

  function toggles() {
    var list = document.querySelectorAll("[data-theme-toggle]");
    for (var i = 0; i < list.length; i++) {
      var b = list[i];
      if (!b.firstElementChild) drawIcons(b);
      b.setAttribute("data-choice", current);
      b.setAttribute("aria-label", `Theme: ${NAMES[current]}`);
      b.setAttribute("title", `Theme: ${NAMES[current]}. Switch to ${NAMES[next()]}.`);
    }
  }

  function apply(choice) {
    current = NAMES[choice] ? choice : "system";
    if (current === "system") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", current);
    var meta = document.querySelector('meta[name="color-scheme"]');
    if (meta) meta.setAttribute("content", current === "system" ? "light dark" : current);
    pictures();
    toggles();
  }
  function set(choice) { store(NAMES[choice] ? choice : "system"); apply(choice); }

  apply(current);
  document.addEventListener("click", function (e) {
    var t = e.target && e.target.closest ? e.target.closest("[data-theme-toggle]") : null;
    if (t) { e.preventDefault(); set(next()); }
  });
  // Another tab changed it: follow.
  window.addEventListener("storage", function (e) { if (e.key === KEY || e.key === null) apply(stored()); });
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", function () { pictures(); toggles(); });

  window.PPTheme = {
    get: function () { return current; },
    set: set,
    refresh: function () { pictures(); toggles(); },
  };
})();
