// Pigeonpost developer docs: a Copy button on every command snippet (`pre[data-copy]`), styled as
// the design kit's code-block copy button (`.pp-code__copy`) in the corner of its `.pp-code` figure.
// Served from this origin; no inline scripts or handlers, so the site keeps `script-src 'self'`.
(function () {
  "use strict";

  function selectText(node) {
    var range = document.createRange();
    range.selectNodeContents(node);
    var selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
  }

  function attach(pre) {
    var source = pre.querySelector("code") || pre;
    var button = document.createElement("button");
    button.type = "button";
    button.className = "pp-code__copy";
    button.textContent = "Copy";
    button.setAttribute("aria-label", "Copy command");
    var timer = null;
    function say(text) {
      button.textContent = text;
      clearTimeout(timer);
      timer = setTimeout(function () { button.textContent = "Copy"; }, 1600);
    }
    button.addEventListener("click", function () {
      var text = source.textContent;
      if (navigator.clipboard && window.isSecureContext) {
        navigator.clipboard.writeText(text).then(
          function () { say("Copied"); },
          function () { selectText(source); say("Press Ctrl+C"); }
        );
      } else {
        selectText(source);
        say("Press Ctrl+C");
      }
    });
    (pre.closest(".pp-code") || pre).appendChild(button);
  }

  function init() {
    var blocks = document.querySelectorAll("pre[data-copy]");
    for (var i = 0; i < blocks.length; i++) attach(blocks[i]);
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
