// The composer's emoji picker, in jsdom.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";

const read = (name) => readFileSync(new URL("../" + name, import.meta.url), "utf8");

function rig({ storage = true } = {}) {
  const dom = new JSDOM(
    `<!doctype html><form id="f"><div class="composer-pill"><button id="attach" type="button"></button><textarea id="box"></textarea><button id="send" type="submit"></button></div></form><p id="away"></p>`,
    { runScripts: "outside-only", url: "https://inbox.example/" },
  );
  const { window } = dom;
  if (!storage) Object.defineProperty(window, "localStorage", { get() { throw new Error("refused"); } });
  window.eval(read("emoji.js"));
  const box = window.document.getElementById("box");
  const picker = window.PigeonpostEmoji.attach(box);
  const $ = (s) => window.document.querySelector(s);
  const $$ = (s) => [...window.document.querySelectorAll(s)];
  const key = (el, k) => { const e = new window.KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true }); el.dispatchEvent(e); return e; };
  const visible = () => $$(".emoji-cell").filter((c) => !c.hidden && !c.closest("[hidden]"));
  const pick = (char) => visible().find((c) => c.textContent === char).click();
  return { window, box, picker, $, $$, key, visible, pick };
}

test("the button sits between the box and Send, and is not a submit button", () => {
  const { box, picker, $ } = rig();
  assert.equal(box.nextElementSibling, picker.button);
  assert.equal(picker.button.type, "button");
  assert.equal(picker.button.getAttribute("aria-label"), "Emoji");
  assert.equal($("#send").previousElementSibling, picker.button);
});

test("attaching twice adds one button", () => {
  const { window, box, $$ } = rig();
  assert.equal(window.PigeonpostEmoji.attach(box), null);
  assert.equal($$(".emoji-btn").length, 1);
});

test("the button opens and closes the panel", () => {
  const { picker, $ } = rig();
  picker.button.click();
  assert.equal($(".emoji-panel").hidden, false);
  assert.equal(picker.button.getAttribute("aria-expanded"), "true");
  picker.button.click();
  assert.equal($(".emoji-panel").hidden, true);
  assert.equal(picker.button.getAttribute("aria-expanded"), "false");
});

test("an emoji goes in at the caret, and the composer hears it as typing", () => {
  const { window, box, picker, pick } = rig();
  box.value = "hello there";
  box.setSelectionRange(5, 5);
  let inputs = 0;
  box.addEventListener("input", () => inputs++);
  picker.button.click();
  pick("😀");
  assert.equal(box.value, "hello😀 there");
  assert.equal(box.selectionStart, 5 + "😀".length, "the caret lands after the emoji");
  assert.equal(inputs, 1);
  assert.equal(window.document.querySelector(".emoji-panel").hidden, false, "stays open for another");
});

test("an emoji replaces selected text", () => {
  const { box, picker, pick } = rig();
  box.value = "a word b";
  box.setSelectionRange(2, 6);
  picker.button.click();
  pick("👍");
  assert.equal(box.value, "a 👍 b");
});

test("nothing is inserted while the box is locked for sending", () => {
  const { box, picker, pick } = rig();
  picker.button.click();
  box.disabled = true;
  pick("😀");
  assert.equal(box.value, "");
});

test("search narrows by word and says when nothing matches", () => {
  const { window, picker, $, visible } = rig();
  picker.button.click();
  const search = $(".emoji-search");
  search.value = "pizza";
  search.dispatchEvent(new window.Event("input", { bubbles: true }));
  assert.deepEqual(visible().map((c) => c.textContent), ["🍕"]);
  assert.equal($(".emoji-none").hidden, true);
  search.value = "zzzz";
  search.dispatchEvent(new window.Event("input", { bubbles: true }));
  assert.equal(visible().length, 0);
  assert.equal($(".emoji-none").hidden, false);
});

test("Enter in search takes the first result and does not send the message", () => {
  const { window, box, picker, $, key } = rig();
  let submitted = 0;
  $("#f").addEventListener("submit", (e) => { e.preventDefault(); submitted++; });
  picker.button.click();
  const search = $(".emoji-search");
  search.value = "pizza";
  search.dispatchEvent(new window.Event("input", { bubbles: true }));
  const e = key(search, "Enter");
  assert.equal(e.defaultPrevented, true);
  assert.equal(box.value, "🍕");
  assert.equal(submitted, 0);
});

test("Escape closes only the panel and gives focus back to the button", () => {
  const { window, box, picker, $, key } = rig();
  let boxSawEscape = 0;
  box.addEventListener("keydown", (e) => { if (e.key === "Escape") boxSawEscape++; });
  picker.button.click();
  box.focus();
  key(box, "Escape");
  assert.equal($(".emoji-panel").hidden, true);
  assert.equal(boxSawEscape, 0, "a reply or an edit in progress is left alone");
  assert.equal(window.document.activeElement, picker.button);
});

test("a press outside closes the panel, a press inside does not", () => {
  const { window, picker, $ } = rig();
  picker.button.click();
  $(".emoji-search").dispatchEvent(new window.Event("pointerdown", { bubbles: true }));
  assert.equal($(".emoji-panel").hidden, false);
  $("#away").dispatchEvent(new window.Event("pointerdown", { bubbles: true }));
  assert.equal($(".emoji-panel").hidden, true);
});

test("sending closes the panel", () => {
  const { window, picker, $ } = rig();
  picker.button.click();
  $("#f").dispatchEvent(new window.Event("submit", { bubbles: true, cancelable: true }));
  assert.equal($(".emoji-panel").hidden, true);
});

test("the panel is one Tab stop and the arrow keys move inside it", () => {
  const { window, picker, $, key, visible } = rig();
  picker.button.click();
  assert.equal(visible().filter((c) => c.getAttribute("tabindex") === "0").length, 1);
  key($(".emoji-search"), "ArrowDown");
  assert.equal(window.document.activeElement, visible()[0]);
  key(visible()[0], "ArrowRight");
  assert.equal(window.document.activeElement, visible()[1]);
  assert.equal(visible().filter((c) => c.getAttribute("tabindex") === "0").length, 1);
});

test("recent emoji come first, newest first, and survive reopening", () => {
  const { picker, $, pick } = rig();
  picker.button.click();
  pick("😀");
  pick("🍕");
  picker.button.click();
  picker.button.click();
  const recent = [...$("[data-recent]").querySelectorAll(".emoji-cell")].map((c) => c.textContent);
  assert.deepEqual(recent, ["🍕", "😀"]);
});

test("the picker works when the browser refuses storage", () => {
  const { box, picker, pick } = rig({ storage: false });
  picker.button.click();
  pick("😀");
  assert.equal(box.value, "😀");
});

test("every entry has a character and a name, and none is listed twice", () => {
  const { window } = rig();
  const items = Array.from(window.PigeonpostEmoji.sets).flatMap((s) => Array.from(s.items));
  assert.ok(items.length > 300);
  for (const i of items) assert.ok(i.char && /\S/.test(i.words), JSON.stringify(i));
  assert.equal(new Set(items.map((i) => i.char)).size, items.length);
});

test("the page loads the picker before both composers and attaches it to each", () => {
  const html = read("index.html");
  const at = (name) => html.indexOf(`src="${name}?v=`);
  assert.ok(at("emoji.js") > 0 && at("emoji.js") < at("groups.js") && at("emoji.js") < at("app.js"));
  assert.match(read("app.js"), /PigeonpostEmoji\?\.attach\(compose\)/);
  assert.match(read("groups.js"), /PigeonpostEmoji\?\.attach\(\$\("group-compose"\)\)/);
});
