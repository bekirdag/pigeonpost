(() => {
  "use strict";

  const ua = navigator.userAgent || "";
  const hint = navigator.userAgentData?.platform || "";
  const platform = navigator.platform || "";
  const desktopIPad = /Mac/i.test(`${platform} ${ua}`) && navigator.maxTouchPoints > 1;
  const mobile = desktopIPad || navigator.userAgentData?.mobile || /iPhone|iPad|iPod|Android/i.test(`${hint} ${ua}`) || /^(iOS|Android)$/i.test(hint);
  const chromeOS = /Chrome OS|ChromeOS|CrOS/i.test(`${hint} ${ua}`);
  // Prefer an explicit browser platform hint. Apple silicon browsers also say
  // "Intel", so never infer a Mac's chip: its release is a universal app.
  const desktop = { macos: "mac", windows: "windows", linux: "linux" }[hint.toLowerCase()];
  const fallback = `${platform} ${ua}`;
  const detected = mobile || chromeOS ? "web" : desktop || (
    /Win32|Win64|Windows/i.test(fallback) ? "windows" :
    /Mac/i.test(fallback) ? "mac" :
    /Linux/i.test(fallback) ? "linux" : null
  );

  // Unknown devices retain the same manual choices as visitors without JavaScript.
  if (!detected) return;
  const card = document.querySelector(`[data-platform="${detected}"]`);
  const web = document.getElementById("web-inbox");
  const action = card ? card.querySelector(".platform-action") : web;
  document.getElementById("device-recommendation").textContent = card
    ? `Recommended for ${card.querySelector("h3").textContent}`
    : "Recommended for this device";
  document.getElementById("recommended-title").textContent = card
    ? card.dataset.recommendationTitle : "Your inbox in the browser";
  document.getElementById("recommended-description").textContent = card
    ? card.querySelector(".platform-description").textContent
    : "Use Pigeonpost in your browser, or choose a desktop release below for another computer.";
  const primary = document.getElementById("recommended-download");
  primary.href = action.href;
  primary.replaceChildren(...Array.from(action.childNodes, node => node.cloneNode(true)));
  web.hidden = primary.href === web.href;
  document.getElementById("other-downloads").hidden = false;
})();
