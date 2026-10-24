// Pigeonpost inbox — appearance, applied before the first paint.
//
// Loaded synchronously in <head>, ahead of the stylesheet, so a chosen theme never flashes the
// other one first. A file rather than an inline script because the CSP allows only 'self' scripts.
// "system" (or nothing stored) leaves the attribute off, and the stylesheet follows the OS.
(function () {
  var theme = null;
  try { theme = window.localStorage.getItem("ppi_theme"); } catch (e) { /* storage blocked */ }
  if (theme !== "light" && theme !== "dark") return;
  document.documentElement.setAttribute("data-theme", theme);
  var meta = document.querySelector('meta[name="color-scheme"]');
  if (meta) meta.setAttribute("content", theme);
})();
