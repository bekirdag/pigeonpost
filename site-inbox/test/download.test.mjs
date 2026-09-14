import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";

const html = readFileSync(new URL("../../site/download.html", import.meta.url), "utf8");
const source = readFileSync(new URL("../../site/download.js", import.meta.url), "utf8");
const homepage = readFileSync(new URL("../../site/index.html", import.meta.url), "utf8");
const macArchive = "https://github.com/bekirdag/pigeonpost/releases/download/macos-1.0-40/Pigeonpost-Desktop-1.0-40.zip";
const webInbox = "https://inbox.pigeonpost.dev/";
const linuxArchive = "https://github.com/bekirdag/pigeonpost/releases/download/linux-desktop-1.0.5/Pigeonpost-Desktop-1.0.5-x86_64.flatpak";
const linuxArmArchive = linuxArchive.replace("x86_64", "aarch64");

function page(t, navigator = {}, runScript = true) {
  const dom = new JSDOM(html, { url: "https://pigeonpost.dev/download", runScripts: "outside-only" });
  t.after(() => dom.window.close());
  for (const [key, value] of Object.entries(navigator)) {
    Object.defineProperty(dom.window.navigator, key, { configurable: true, value });
  }
  if (runScript) dom.window.eval(source);
  return dom.window.document;
}

function platformChoices(document) {
  for (const platform of ["mac", "windows", "linux"]) {
    const card = document.querySelector(`[data-platform="${platform}"]`);
    assert.equal(card.closest("[hidden]"), null, `${platform} stays visible`);
    const action = card.querySelector(".platform-action");
    assert.equal(action.href, platform === "mac" ? macArchive : platform === "linux" ? linuxArchive : webInbox);
    assert.equal(action.querySelector("use").getAttribute("href"), `#icon-${platform}`);
    assert.equal(action.querySelector("svg").getAttribute("aria-hidden"), "true");
    assert.ok(document.getElementById(`icon-${platform}`).querySelector("path"));
    assert.ok(action.textContent.trim(), "icons have accompanying text labels");
  }
  assert.match(document.querySelector('[data-platform="windows"]').textContent, /In development/);
  assert.match(document.querySelector('[data-platform="linux"]').textContent, /Available now/);
}

for (const runScript of [false, true]) {
  test(`${runScript ? "unknown browsers" : "without JavaScript"} get manual choices without a guessed installer`, (t) => {
    const document = page(t, { userAgent: "", platform: "", userAgentData: undefined }, runScript);
    assert.equal(document.getElementById("recommended-download").hash, "#all-downloads");
    assert.equal(document.getElementById("web-inbox").hidden, false);
    platformChoices(document);
  });
}

for (const chip of ["arm", "x86"]) {
  test(`Mac browsers saying Intel get the same universal app on ${chip}`, (t) => {
    const document = page(t, {
      userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Safari/605.1.15",
      platform: "MacIntel", maxTouchPoints: 0,
      userAgentData: { platform: "macOS", architecture: chip },
    });
    assert.equal(document.getElementById("device-recommendation").textContent, "Recommended for Mac");
    assert.equal(document.getElementById("recommended-download").href, macArchive);
    assert.equal(document.querySelector("#recommended-download use").getAttribute("href"), "#icon-mac");
    assert.match(document.getElementById("recommended-description").textContent, /Apple silicon and Intel/);
    assert.equal(document.getElementById("web-inbox").hidden, false);
    platformChoices(document);
  });
}

for (const [name, navigator, icon] of [
  ["Windows", { platform: "Win32", userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" }, "windows"],
  ["ChromeOS", { platform: "Linux x86_64", userAgent: "Mozilla/5.0 (X11; CrOS x86_64)" }, "web"],
  ["ChromeOS client hint", { platform: "Linux x86_64", userAgent: "", userAgentData: { platform: "Chrome OS" } }, "web"],
  ["Android", { platform: "Linux aarch64", userAgent: "Mozilla/5.0 (Linux; Android 15)" }, "web"],
  ["Android client hint", { userAgent: "", userAgentData: { platform: "Android" } }, "web"],
  ["mobile client hint", { platform: "MacIntel", userAgent: "", userAgentData: { platform: "macOS", mobile: true } }, "web"],
  ["iOS client hint", { userAgent: "", userAgentData: { platform: "iOS" } }, "web"],
  ["iPhone", { platform: "iPhone", userAgent: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)" }, "web"],
  ["iPad desktop mode", { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", maxTouchPoints: 5 }, "web"],
]) {
  test(`${name} gets an honest browser recommendation and retains all desktop choices`, (t) => {
    const document = page(t, navigator);
    assert.equal(document.getElementById("recommended-download").href, webInbox);
    assert.equal(document.querySelector("#recommended-download use").getAttribute("href"), `#icon-${icon}`);
    assert.equal(document.getElementById("recommended-title").textContent,
      icon === "web" ? "Your inbox in the browser" : `Pigeonpost on ${name}`);
    assert.equal(document.getElementById("web-inbox").hidden, true, "avoid duplicate primary web actions");
    assert.equal(document.getElementById("other-downloads").hidden, false);
    platformChoices(document);
  });
}

for (const [hint, icon, href] of [["Windows", "windows", webInbox], ["Linux", "linux", linuxArchive], ["macOS", "mac", macArchive]]) {
  test(`${hint} client hint takes precedence over a conflicting legacy desktop platform`, (t) => {
    const document = page(t, {
      platform: hint === "macOS" ? "Linux x86_64" : "MacIntel",
      userAgent: "", userAgentData: { platform: hint }, maxTouchPoints: 0,
    });
    assert.equal(document.getElementById("recommended-download").href, href);
    assert.equal(document.querySelector("#recommended-download use").getAttribute("href"), `#icon-${icon}`);
  });
}

test("Safari without client hints still receives the Mac universal download", (t) => {
  const document = page(t, { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", userAgentData: undefined });
  assert.equal(document.getElementById("recommended-download").href, macArchive);
});

for (const [platform, href] of [["Linux x86_64", linuxArchive], ["Linux aarch64", linuxArmArchive], ["Linux arm64", linuxArmArchive]]) {
  test(`${platform} gets the matching native Linux download`, (t) => {
    const document = page(t, { platform, userAgent: `Mozilla/5.0 (X11; ${platform})` });
    assert.equal(document.getElementById("recommended-download").href, href);
    assert.equal(document.querySelector("#recommended-download use").getAttribute("href"), "#icon-linux");
    assert.equal(document.getElementById("recommended-title").textContent, "Download for Linux");
    assert.equal(document.getElementById("web-inbox").hidden, false);
    platformChoices(document);
  });
}

test("reduced Linux user agents use explicit ARM64 client hints", async (t) => {
  const document = page(t, { platform: "Linux x86_64", userAgent: "", userAgentData: {
    platform: "Linux", getHighEntropyValues: async () => ({ architecture: "arm", bitness: "64" }),
  }});
  await Promise.resolve();
  assert.equal(document.getElementById("recommended-download").href, linuxArmArchive);
});

test("unavailable architecture hints keep the default and manual alternatives", async (t) => {
  const document = page(t, { platform: "Linux x86_64", userAgentData: {
    platform: "Linux", getHighEntropyValues: async () => { throw new Error("unavailable"); },
  }});
  await Promise.resolve();
  assert.equal(document.getElementById("recommended-download").href, linuxArchive);
  assert.equal(document.querySelector('[data-platform="linux"] .platform-action-arm').href, linuxArmArchive);
});

test("the homepage has exactly one neutral Download link and no direct platform installers", (t) => {
  const dom = new JSDOM(homepage, { url: "https://pigeonpost.dev/" });
  t.after(() => dom.window.close());
  const links = Array.from(dom.window.document.querySelectorAll("a"));
  const downloads = links.filter(link => link.pathname === "/download");
  assert.equal(downloads.length, 1);
  assert.equal(downloads[0].textContent.trim(), "Download");
  assert.equal(links.some(link => /Download for (Mac|Windows|Linux)/i.test(link.textContent)), false);
  assert.equal(links.some(link => link.href.includes("/releases/download/")), false);
});
