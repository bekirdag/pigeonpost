import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";

const html = readFileSync(new URL("../../site/download.html", import.meta.url), "utf8");
const source = readFileSync(new URL("../../site/download.js", import.meta.url), "utf8");

function page(t, navigator = {}, runScript = true) {
  const dom = new JSDOM(html, { url: "https://pigeonpost.dev/download", runScripts: "outside-only" });
  t.after(() => dom.window.close());
  for (const [key, value] of Object.entries(navigator)) {
    Object.defineProperty(dom.window.navigator, key, { configurable: true, value });
  }
  if (runScript) dom.window.eval(source);
  return dom.window.document;
}

function macChoices(document) {
  return ["apple-silicon-download", "intel-download"].map((id) => {
    const card = document.getElementById(id);
    assert.equal(card.closest("[hidden]"), null, `${id} stays available`);
    const link = card.querySelector("a");
    assert.match(link.href, /^https:\/\/github\.com\/bekirdag\/pigeonpost\/releases\/download\/macos-[\d.-]+\/Pigeonpost-Desktop-[\d.-]+\.zip$/);
    return link.href;
  });
}

test("without JavaScript both Mac choices and the primary link download the universal app", (t) => {
  const document = page(t, {}, false);
  const choices = macChoices(document);
  assert.equal(choices[0], choices[1]);
  assert.equal(document.getElementById("recommended-download").href, choices[0]);
  assert.match(document.body.textContent, /macOS 14/);
});

for (const chip of ["arm", "x86"]) {
  test(`Mac browsers saying Intel still get the universal app on ${chip}`, (t) => {
    const document = page(t, {
      userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Safari/605.1.15",
      platform: "MacIntel", maxTouchPoints: 0,
      userAgentData: { platform: "macOS", architecture: chip },
    });
    assert.equal(document.getElementById("device-recommendation").textContent, "Recommended for your Mac");
    assert.equal(document.getElementById("recommended-download").href, macChoices(document)[0]);
  });
}

for (const [name, navigator] of [
  ["Windows", { platform: "Win32", userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" }],
  ["Linux", { platform: "Linux x86_64", userAgent: "Mozilla/5.0 (X11; Linux x86_64)" }],
  ["ChromeOS", { platform: "Linux x86_64", userAgent: "Mozilla/5.0 (X11; CrOS x86_64)" }],
  ["Android", { platform: "Linux aarch64", userAgent: "Mozilla/5.0 (Linux; Android 15)" }],
  ["iPhone", { platform: "iPhone", userAgent: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)" }],
  ["iPad desktop mode", { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", maxTouchPoints: 5 }],
]) {
  test(`${name} gets the web option and can still download for either Mac`, (t) => {
    const document = page(t, navigator);
    assert.equal(document.getElementById("recommended-download").href, "https://inbox.pigeonpost.dev/");
    assert.equal(document.getElementById("recommended-title").textContent, "Your inbox in the browser");
    assert.equal(macChoices(document).length, 2);
  });
}

test("unknown devices retain working manual download choices", (t) => {
  const document = page(t, { platform: "", userAgent: "", userAgentData: undefined });
  assert.equal(document.getElementById("recommended-download").href, macChoices(document)[0]);
});
