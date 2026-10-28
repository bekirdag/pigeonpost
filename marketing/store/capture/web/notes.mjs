// "meeting-notes": the real Meeting notes reader (site-inbox/meeting-notes.js) opening the "Launch
// sync" brief from seed/demo-world.json. The brief is sealed in this process with meeting-intel's own
// sealBrief to the notes key this browser registered, and opened by the page with its own key.
//
//   node notes.mjs
import { readFileSync } from "node:fs";
import path from "node:path";
import { sealBrief } from "../../../../meeting-intel/src/crypto.js";
import { createStatic } from "../../../../tests/e2e-streams/serve.mjs";
import { browser, shoot, sleep, DEVICES, HERE, REPO, P, localOnly, CLOCK, EXTRA_OUT } from "./lib.mjs";

const world = JSON.parse(readFileSync(path.join(HERE, "../../seed/demo-world.json"), "utf8"));
const n = world.meeting_notes;
const NAME = (h) => ({ "/maya/research-agent": "Research agent" })[h] || h.slice(1, 2).toUpperCase() + h.slice(2);
const started = new Date(); started.setHours(10, 0, 0, 0); started.setDate(started.getDate() - ((started.getDay() + 3) % 7)); // last Thursday, 10:00
const brief = {
  v: 1, title: n.meeting, started_at: started.toISOString(), duration_s: 42 * 60,
  participants: ["Maya", "Leo", "Ana", "Priya", "Research agent"],
  summary: n.summary, decisions: n.decisions,
  action_items: n.action_items.map((a) => ({ owner: NAME(a.owner), task: a.text.replace(/ by Friday$/, ""), due: a.owner === "/priya" ? "Friday" : null })),
  open_questions: ["Do we need a second round of user testing before May 20?"],
  generated_by: { stt: "whisper.cpp", llm: "local model" },
};

const web = createStatic(path.join(REPO, "site-inbox"), P.calls);
await new Promise((r) => web.listen(P.web, "127.0.0.1", r));
const b = await browser();
let failed = false;
try {
  const page = await b.newPage();
  await localOnly(page);
  await page.setViewport({ width: 440, height: 894, hasTouch: true });
  page.__touch = true;
  await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "dark" }]);
  await page.exposeFunction("__ppSeal", (meetingId, key) => sealBrief(meetingId, Buffer.from(JSON.stringify(brief)), [{ account_id: "acct_maya", keys: [key] }]));
  await page.evaluateOnNewDocument(CLOCK);
  await page.evaluateOnNewDocument(`window.__PP_WORLD = ${JSON.stringify(world)};`);
  await page.evaluateOnNewDocument(readFileSync(path.join(HERE, "inbox-fixture.js"), "utf8"));
  await page.goto(`http://127.0.0.1:${P.web}/`, { waitUntil: "load" });
  await page.waitForFunction(() => window.PigeonpostNotes && document.getElementById("notes-btn") && !document.getElementById("notes-btn").hidden, { timeout: 30000 });
  await page.evaluate(() => window.PigeonpostNotes.open());
  await page.waitForSelector(".notes-list button", { timeout: 15000 });
  await page.click(".notes-list button");
  await page.waitForSelector(".notes-brief", { timeout: 15000 });
  for (const cls of Object.keys(DEVICES)) await shoot(page, cls, "meeting-notes", { settle: 1200, dir: EXTRA_OUT });
} catch (e) { failed = true; console.error(e); }
finally { await b.close().catch(() => {}); web.close(); }
process.exit(failed ? 1 : 0);
