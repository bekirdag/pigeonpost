// "live": a real end-to-end encrypted group live stream. Priya goes live to "Design team" from the
// call page the apps embed (native-call.html → live.js) with an illustrated presenter camera and a
// drawn slide (avatar-media.js, presenter scene); a local LiveKit carries it; the other invented
// members watch, a few react; Maya's screen is captured. Postbox: tests/e2e-streams/mock-postbox.mjs.
//
//   node live.mjs [extra viewers=24]
import { createMockPostbox } from "../../../../tests/e2e-streams/mock-postbox.mjs";
import { startStack, browser, callPage, shoot, sleep, EXTRA_OUT } from "./lib.mjs";

const EXTRA = Number(process.argv[2] || 24);
const NAMED = ["maya", "leo", "ana", "sam", "noor", "kai", "tomas"];
const extras = Array.from({ length: EXTRA }, (_, i) => "viewer" + i);
const members = { "/k/priya": "owner" };
for (const n of [...NAMED, ...extras]) members["/k/" + n] = "member";
const pb = createMockPostbox({ members, maxViewers: 200 });
const GROUP = { id: pb.GID, name: "Design team" };
const known = { "/k/priya": "Priya", "/k/maya": "Maya", "/k/leo": "Leo", "/k/ana": "Ana", "/k/sam": "Sam", "/k/noor": "Noor", "/k/kai": "Kai", "/k/tomas": "Tomás" };
// The calls service checks the service token the stream mock expects.
const stack = await startStack({ postbox: pb, sfu: true, extraEnv: { POSTBOX_SERVICE_TOKEN: "svc_calls_test", CALLS_STREAM_VIEWER_MBPS: "2.2" } });
const browsers = [];
const open = async () => { const x = await browser(); browsers.push(x); return x; };
let failed = false;
try {
  const hostB = await open();
  const host = await callPage(hostB, "priya", { known, address: "/k/priya", scene: "presenter", viewport: { width: 640, height: 400 } });
  const live = await host.evaluate((g) => window.PigeonpostNative.goLive(g, { title: "Friday demo: new onboarding", camera: true, screen: false }), GROUP);
  console.log("live", JSON.stringify(live));
  if (!live.ok) throw new Error("could not go live");
  await sleep(2000);

  const watch = (p) => p.evaluate((g, s) => Promise.race([window.PigeonpostNative.watchStream(g, s), new Promise((r) => setTimeout(() => r({ ok: false, code: "timeout" }), 25000))]), GROUP, live.stream);
  // Everyone else first, a few per browser, small windows.
  const viewers = [];
  const pool = [...NAMED.slice(1), ...extras];
  for (let i = 0; i < pool.length; i += 6) {
    const vb = await open();
    for (const n of pool.slice(i, i + 6)) {
      const p = await callPage(vb, n, { known, address: "/k/" + n, viewport: { width: 240, height: 180 } });
      const r = await watch(p);
      if (!r.ok) console.log("watch", n, JSON.stringify(r));
      viewers.push(p);
    }
  }
  const me = await callPage(await open(), "maya", { known, address: "/k/maya", viewport: { width: 1376, height: 984 }, touch: false, clock: true });
  console.log("maya watches", JSON.stringify(await watch(me)));
  await sleep(8000);
  // A little applause from the audience, timed so it is on screen for each capture.
  const applaud = async () => {
    const r = ["👏", "🎉", "❤️", "🔥", "👏", "😍"];
    for (let i = 0; i < 6; i++) viewers[i % viewers.length].evaluate((x) => window.PigeonpostLive.react(x), r[i]).catch(() => {});
    await sleep(900);
  };
  console.log("maya sees", JSON.stringify(await me.evaluate(() => { const i = window.PigeonpostLive.inspect(); return i && { role: i.role, viewers: i.viewers, epoch: i.epoch, code: i.code }; })));
  // Widest first, so the viewer has asked for the sharpest layer before the narrower shots.
  for (const cls of ["ios-tablet", "android-tablet", "ios-phone", "android-phone"]) await shoot(me, cls, "live", { settle: 9000, before: applaud, dir: EXTRA_OUT });
} catch (e) { failed = true; console.error(e); }
finally { for (const x of browsers) await x.close().catch(() => {}); stack.stop(); }
process.exit(failed ? 1 : 0);
