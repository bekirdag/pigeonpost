// "call": a real end-to-end encrypted group video call, "Launch sync", between invented people.
// Every participant is its own headless Chrome context running the call page the apps embed
// (native-call.html), with an illustrated synthetic camera (avatar-media.js). They all join the
// same scheduled meeting through the repository's call service; Maya's screen is captured.
//
//   node call.mjs [participants=8] [mesh|sfu]
import { createDemoPostbox, PEOPLE, NAMES, MEETING } from "./demo-postbox.mjs";
import { startStack, browser, callPage, shoot, sleep, SERVICE_TOKEN, DEVICES } from "./lib.mjs";

const N = Number(process.argv[2] || 8);
const transport = process.argv[3] || (N > 6 ? "sfu" : "mesh");
const who = PEOPLE.slice(0, N);
const known = Object.fromEntries(PEOPLE.map((n) => ["/" + n, NAMES[n]]));
const stack = await startStack({ postbox: createDemoPostbox({ serviceToken: SERVICE_TOKEN, transport }), sfu: transport === "sfu" });
const browsers = [];
const b = await browser(); browsers.push(b);
let failed = false;
try {
  const pages = {};
  for (const n of [...who.slice(1), who[0]]) { // everyone else first, so Maya walks into a full room
    const own = n === "maya" ? b : await browser(); if (own !== b) browsers.push(own);
    const page = await callPage(own, n, { known, viewport: n === "maya" ? { width: 440, height: 894 } : { width: 320, height: 240 }, touch: n !== "maya", clock: n === "maya" });
    await page.waitForFunction(() => window.PigeonpostCalls && window.PigeonpostCalls.inspect().session, { timeout: 30000 });
    const r = await page.evaluate((m, n) => window.PigeonpostNative.joinScheduled(m.code, { as: "/k/demo" + n, occurrenceId: m.occurrence, kind: "video", title: "Launch sync" }), MEETING, n);
    console.log("join", n, JSON.stringify(r));
    pages[n] = page;
    await sleep(800);
  }
  const me = pages.maya;
  // Let every picture arrive and the layout settle.
  await sleep(12000);
  const state = await me.evaluate(() => { const i = window.PigeonpostCalls.inspect(); return { topology: i.room && i.room.topology, peers: (i.peers || []).map((p) => [p.handle || p.name, p.connected, p.stats && p.stats.receiving && p.stats.receiving.video && p.stats.receiving.video.decoded]) }; });
  console.log("maya sees", JSON.stringify(state));
  for (const cls of Object.keys(DEVICES)) await shoot(me, cls, "call", { settle: 5000, before: async (page) => {
    // Open the call details under the name: the room-wide security code and its end-to-end note.
    await page.evaluate(() => { const pill = document.getElementById("call-pill"); if (pill && pill.getAttribute("aria-expanded") !== "true") pill.click(); });
    await sleep(800);
  } });
} catch (e) { failed = true; console.error(e); }
finally { for (const x of browsers) await x.close().catch(() => {}); stack.stop(); }
process.exit(failed ? 1 : 0);
