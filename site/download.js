// A universal app is the correct Mac download on both chip families. Browser
// user agents often say "Intel" on Apple silicon, so never infer a chip from them.
(() => {
  "use strict";
  const ua = navigator.userAgent || "";
  const platform = navigator.userAgentData?.platform || navigator.platform || "";
  const desktopIPad = /Mac/i.test(`${platform} ${ua}`) && navigator.maxTouchPoints > 1;
  const mobile = desktopIPad || /iPhone|iPad|iPod|Android/i.test(ua);
  const mac = !mobile && /Mac/i.test(`${platform} ${ua}`);
  const otherPlatform = mobile || /Windows|Win32|Win64|Linux|CrOS/i.test(`${platform} ${ua}`);
  const badge = document.getElementById("device-recommendation");

  if (mac) {
    badge.textContent = "Recommended for your Mac";
  } else if (otherPlatform) {
    badge.textContent = "Recommended for this device";
    document.getElementById("recommended-title").textContent = "Your inbox in the browser";
    document.getElementById("recommended-description").textContent = "Use Pigeonpost on this device, or choose a Mac download below.";
    const primary = document.getElementById("recommended-download");
    primary.href = document.getElementById("web-inbox").href;
    primary.textContent = "Open the web inbox";
  }
})();
