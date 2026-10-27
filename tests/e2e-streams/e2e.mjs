// End-to-end check of live streams (Phase 6, docs/streaming.md): a local LiveKit (`livekit-server`
// on PATH), a stand-in postbox (mock-postbox.mjs; the real routes are tested in Rust), the real calls
// service and the real inbox pages in headless Chrome, with synthetic media only (inject.js) — never
// a camera or microphone.
//
//   npm i --no-save puppeteer-core@24.10.0 && node e2e.mjs [viewers=4]
//   BUDGET=5 CAPACITY=2 node e2e.mjs        # the over-budget refusal
//
// Ports 7980/7981, 50100-50400/udp, 8560, 8561 and 8891 on 127.0.0.1.
import { spawn } from "node:child_process";
import { writeFileSync, readFileSync } from "node:fs";
import path from "node:path";
import puppeteer from "puppeteer-core";
import { createMockPostbox } from "./mock-postbox.mjs";
import { createStatic } from "./serve.mjs";

const REPO = process.env.REPO || path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
const HERE = path.dirname(new URL(import.meta.url).pathname);
const N = Number(process.argv[2] || 4);
const SECRET = "x".repeat(48);
const P = { lk: 7980, calls: 8560, pb: 8891, web: 8561 };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, detail = "") => { results.push({ name, ok: Boolean(ok), detail }); console.log((ok ? "PASS " : "FAIL ") + name + (detail ? "  — " + detail : "")); };

const procs = [];
function run(cmd, args, env, name) {
  const p = spawn(cmd, args, { env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "pipe"] });
  p.stdout.on("data", (d) => { if (process.env.VERBOSE) process.stdout.write(`[${name}] ${d}`); });
  p.stderr.on("data", (d) => { if (process.env.VERBOSE) process.stdout.write(`[${name}] ${d}`); });
  procs.push(p);
  return p;
}

const lkYaml = path.join(process.env.TMPDIR || "/tmp", "pp-streams-livekit.yaml");
writeFileSync(lkYaml, `port: ${P.lk}
bind_addresses: ["127.0.0.1"]
rtc:
  tcp_port: 7981
  port_range_start: 50100
  port_range_end: 50400
  use_external_ip: false
  node_ip: 127.0.0.1
keys:
  APIlocal: ${SECRET}
logging:
  level: warn
`);
run("livekit-server", ["--config", lkYaml], {}, "lk");

const members = { "/k/host": "owner" };
for (let i = 0; i < N + 4; i++) members["/k/v" + i] = "member";
const pb = createMockPostbox({ members, maxViewers: Number(process.env.MAX_VIEWERS || 200) });
await new Promise((r) => pb.server.listen(P.pb, "127.0.0.1", r));
run("node", [path.join(REPO, "calls-service/src/server.js")], {
  CALLS_PORT: String(P.calls), CALLS_POSTBOX_URL: `http://127.0.0.1:${P.pb}`, CALLS_ALLOWED_ORIGINS: `http://127.0.0.1:${P.web}`,
  POSTBOX_INTERNAL_URL: `http://127.0.0.1:${P.pb}`, POSTBOX_SERVICE_TOKEN: "svc_calls_test",
  CALLS_SFU_URL: `ws://127.0.0.1:${P.lk}`, CALLS_SFU_API_URL: `http://127.0.0.1:${P.lk}`, CALLS_SFU_KEY: "APIlocal", CALLS_SFU_SECRET: SECRET,
  CALLS_SFU_BUDGET_MBPS: process.env.BUDGET || "400", CALLS_STREAM_VIEWER_MBPS: "2.2", CALLS_STREAM_START_RESERVE: "1",
}, "calls");
const web = createStatic(path.join(REPO, "site-inbox"), P.calls);
await new Promise((r) => web.listen(P.web, "127.0.0.1", r));
await sleep(1500);

const browser = await puppeteer.launch({
  executablePath: process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", headless: true,
  args: ["--autoplay-policy=no-user-gesture-required", "--disable-background-timer-throttling", "--disable-renderer-backgrounding", "--disable-backgrounding-occluded-windows", "--use-fake-ui-for-media-stream", "--no-first-run"],
});
const inject = readFileSync(path.join(HERE, "inject.js"), "utf8");
const GROUP = { id: pb.GID, name: "Crew" };

async function page(name) {
  const p = await (await browser.createBrowserContext()).newPage();
  await p.setViewport({ width: 1280, height: 800 });
  p.on("pageerror", (e) => console.log(`[${name}] pageerror`, e.message));
  await p.evaluateOnNewDocument(inject);
  await p.goto(`http://127.0.0.1:${P.web}/native-call.html`, { waitUntil: "load" });
  await p.evaluate((n) => window.PigeonpostNative.configure({ token: "tok-" + n, mailbox: { address: "/k/" + n, handle: null } }), name);
  return p;
}
const inspect = (p) => p.evaluate(() => window.PigeonpostLive.inspect());
const stats = (p) => p.evaluate(() => window.PigeonpostLive.stats());
async function videoFrames(p) {
  const s = (await stats(p)) || [];
  return s.filter((x) => x.source === "camera" && x.subscribed).reduce((a, x) => a + (x.frames || 0), 0);
}

try {
  // ---- go live ----
  const host = await page("host");
  const live = await host.evaluate((g) => window.PigeonpostNative.goLive(g, { title: "Sprint demo", camera: true, screen: false }), GROUP);
  check("the streamer goes live", live.ok, JSON.stringify(live));
  const sid = live.stream;
  await sleep(1500);
  let hi = await inspect(host);
  check("the streamer publishes encrypted camera and microphone", hi.published.includes("cam") && hi.published.includes("mic") && hi.sendEpoch >= 1, JSON.stringify(hi.published));

  // ---- viewers ----
  const viewers = [];
  if (process.env.CAPACITY) {
    // BUDGET is set so that only CAPACITY viewers fit: the next one is refused with stream_capacity.
    const outcomes = [];
    for (let i = 0; i < Number(process.env.CAPACITY) + 1; i++) {
      const v = await page("v" + i);
      outcomes.push(await v.evaluate((g, s) => window.PigeonpostNative.watchStream(g, s), GROUP, sid));
      if (i === Number(process.env.CAPACITY)) {
        await sleep(300);
        const shown = await v.evaluate(() => document.getElementById("live-ui") ? !document.getElementById("live-ui").hidden : false);
        outcomes.push({ uiOpen: shown });
      }
    }
    const last = outcomes[outcomes.length - 2];
    check("over the media server's budget, watching is refused with stream_capacity and the screen closes", outcomes.slice(0, -2).every((o) => o.ok) && last.code === "stream_capacity" && outcomes.at(-1).uiOpen === false, JSON.stringify(outcomes));
    throw Object.assign(new Error("done"), { done: true });
  }
  for (let i = 0; i < N; i++) {
    const v = await page("v" + i);
    const r = await v.evaluate((g, s) => Promise.race([window.PigeonpostNative.watchStream(g, s), new Promise((res) => setTimeout(() => res({ ok: false, code: "timeout", state: JSON.stringify(window.PigeonpostLive.inspect()), session: window.PigeonpostCalls.inspect().session }), 20000))]), GROUP, sid);
    if (!r.ok) console.log("watch failed", i, r);
    viewers.push(v);
  }
  await sleep(6000);
  const vi = await Promise.all(viewers.map(inspect));
  check(`${N} viewers received the stream key`, vi.every((x) => x && x.epoch >= 1), JSON.stringify(vi.map((x) => x && x.epoch)));
  const frames = await Promise.all(viewers.map(videoFrames));
  check(`${N} viewers decode the streamer's video`, frames.every((f) => f > 20), JSON.stringify(frames));
  const vstats = await stats(viewers[0]);
  check("every received track is end-to-end encrypted", vstats.length > 0 && vstats.every((x) => x.encrypted), JSON.stringify(vstats.map((x) => [x.source, x.encrypted])));
  check("no decryption errors", vi.every((x) => x.decryptErrors === 0), JSON.stringify(vi.map((x) => x.decryptErrors)));
  hi = await inspect(host);
  check("the streamer wrapped the key to every viewer, verified against the key directory", hi.wrapped === N && hi.participants === N, `${hi.wrapped}/${hi.participants}`);
  const counts = await Promise.all(viewers.map((v) => v.evaluate(() => document.getElementById("live-count-n").textContent)));
  check("the live viewer count shows", counts.every((c) => c === String(N)), JSON.stringify(counts));

  // ---- security code ----
  const vcode = vi[0].code;
  check("a viewer shows a security code with the streamer", /^\d{4} \d{4} \d{4}$/.test(vcode || ""), vcode);

  // ---- latency ----
  await sleep(4000);
  const lats = (await Promise.all(viewers.map((v) => v.evaluate(() => window.__lat.slice(-6))))).flat().sort((a, b) => a - b);
  const median = lats[Math.floor(lats.length / 2)];
  check("glass-to-glass delay under 1 s (median of samples)", lats.length > 3 && median < 1000, `median ${median} ms, p90 ${lats[Math.floor(lats.length * 0.9)]} ms, n=${lats.length}`);

  // ---- reactions ----
  await viewers[1].evaluate(() => window.PigeonpostLive.react("👏"));
  await sleep(1500);
  const seen = await Promise.all([host, viewers[0]].map(inspect));
  check("a sealed reaction reaches the streamer and other viewers", seen.every((x) => x.reactions >= 1), JSON.stringify(seen.map((x) => x.reactions)));

  // ---- a viewer cannot publish ----
  const publish = await viewers[0].evaluate(async () => {
    const room = window.PigeonpostLive._room();
    const c = document.createElement("canvas"); c.width = 320; c.height = 180; c.getContext("2d").fillRect(0, 0, 10, 10);
    try { await room.localParticipant.publishTrack(c.captureStream(5).getVideoTracks()[0], { source: "camera" }); return "published"; }
    catch (e) { return "refused: " + (e && (e.message || e.name)); }
  });
  await sleep(1500);
  const hostSees = await host.evaluate(() => { const r = window.PigeonpostLive._room(); let n = 0; for (const rp of r.remoteParticipants.values()) n += rp.trackPublications.size; return n; });
  check("a viewer's ticket cannot publish (the media server refuses)", publish.startsWith("refused") && hostSees === 0, `${publish}; tracks the streamer sees from others: ${hostSees}`);

  // ---- the stage ----
  await host.evaluate(() => window.PigeonpostLive.setStage("/k/v2", true));
  await viewers[2].evaluate((s) => window.PigeonpostLive.onGroupStream({ stream_id: s, state: "live", stage: { address: "/k/v2", state: "invited", by: "/k/host" } }), sid);
  await sleep(500);
  const offered = await viewers[2].evaluate(() => !document.getElementById("live-stage-offer").hidden);
  const micBefore = await viewers[2].evaluate(() => window.PigeonpostLive.inspect().published.length);
  await viewers[2].evaluate(() => window.PigeonpostLive.acceptStage());
  await sleep(4000);
  const sp = await inspect(viewers[2]);
  const heard = await viewers[0].evaluate(() => { const r = window.PigeonpostLive._room(); let n = 0; for (const rp of r.remoteParticipants.values()) for (const p of rp.trackPublications.values()) if (p.isSubscribed && p.source === "microphone") n++; return n; });
  check("an invitation opens nothing until accepted; then the speaker publishes and is heard", offered && micBefore === 0 && sp.role === "speaker" && sp.published.includes("mic") && heard >= 2, `offered=${offered} before=${micBefore} role=${sp.role} published=${sp.published} mics-heard-by-v0=${heard}`);

  // ---- removal and key rotation ----
  // v3 is removed from the group. To prove the new key, its page keeps a copy of every key it
  // unwrapped and, playing a colluding media server, reconnects with a ticket the harness mints.
  const victim = viewers[3];
  await victim.evaluate(() => {
    const E = window.PigeonpostCallsE2EE;
    window.__keys = [];
    const orig = E.unwrapRoomKey;
    E.unwrapRoomKey = async (a) => { const k = await orig(a); window.__keys.push([a.wrapped.epoch, Array.from(k)]); return k; };
  });
  // Rejoin so the copy covers the current key.
  await victim.evaluate(() => window.PigeonpostLive.stop());
  await victim.evaluate((g, s) => window.PigeonpostNative.watchStream(g, s), GROUP, sid);
  await sleep(3000);
  const epochBefore = (await inspect(host)).epoch;
  await fetch(`http://127.0.0.1:${P.pb}/test/remove`, { method: "POST", body: JSON.stringify({ mailbox: "/k/v3" }) });
  await host.evaluate(() => window.PigeonpostLive.onGroupMember({}));
  await sleep(5000);
  const hostAfter = await inspect(host);
  const victimState = await victim.evaluate(() => window.PigeonpostLive.inspect());
  check("removal: the streamer rotates the key and the media server drops the removed viewer", hostAfter.epoch > epochBefore && victimState === null, `epoch ${epochBefore} → ${hostAfter.epoch}, removed page ${victimState === null ? "left" : "still in"}`);
  const stillFrames = await Promise.all(viewers.slice(0, 3).map(videoFrames));
  await sleep(2000);
  const stillFrames2 = await Promise.all(viewers.slice(0, 3).map(videoFrames));
  check("remaining viewers keep decoding after the rotation", stillFrames2.every((f, i) => f > stillFrames[i]), JSON.stringify([stillFrames, stillFrames2]));
  // The colluding reconnect with the old keys.
  const { signSfuToken } = await import(path.join(REPO, "calls-service/src/server.js"));
  const t = Math.floor(Date.now() / 1000);
  const evilToken = signSfuToken({ key: "APIlocal", secret: SECRET }, { sub: "evil-" + t, nbf: t - 5, exp: t + 300, video: { room: "stream_" + sid, roomJoin: true, canSubscribe: true, canPublish: false, hidden: true } });
  const evil = await victim.evaluate(async (url, token) => {
    const K = window.LivekitClient, E = window.PigeonpostCallsE2EE;
    class Keys extends K.BaseKeyProvider { constructor() { super({ sharedKey: true, ratchetWindowSize: 0, failureTolerance: -1 }); } }
    const keys = new Keys();
    const room = new K.Room({ e2ee: { keyProvider: keys, worker: new Worker("/calls-e2ee-worker.js?v=evil") } });
    let errors = 0;
    room.on(K.RoomEvent.EncryptionError, () => { errors++; });
    await room.setE2EEEnabled(true);
    for (const [epoch, bytes] of window.__keys) keys.onSetEncryptionKey(new Uint8Array(bytes).buffer, undefined, E.slotFor(epoch));
    await room.connect(url, token, { autoSubscribe: true });
    await new Promise((r) => setTimeout(r, 6000));
    let frames = 0;
    for (const rp of room.remoteParticipants.values()) for (const p of rp.trackPublications.values()) {
      if (p.track && p.track.receiver && p.source === "camera") (await p.track.receiver.getStats()).forEach((x) => { if (x.type === "inbound-rtp") frames += x.framesDecoded || 0; });
    }
    room.disconnect();
    return { frames, errors, keys: window.__keys.length };
  }, `ws://127.0.0.1:${P.lk}`, evilToken);
  check("a removed viewer holding every old key decodes nothing new, even via a colluding media server", evil.frames === 0 && evil.keys >= 1, JSON.stringify(evil));

  // ---- capacity ----
  const cap = await fetch(`http://127.0.0.1:${P.calls}/v1/health`).then((r) => r.json());
  check("health reports the streams", cap.streams && cap.streams.live === 1, JSON.stringify(cap.streams));

  // ---- stop ----
  await host.evaluate(() => window.PigeonpostLive.stop());
  await sleep(2000);
  const after = await Promise.all(viewers.slice(0, 3).map(inspect));
  check("stopping ends the stream for everyone", after.every((x) => x === null), JSON.stringify(after.map((x) => x && x.role)));
} catch (e) {
  if (!e.done) check("harness ran to the end", false, String(e && e.stack || e));
} finally {
  await browser.close().catch(() => {});
  web.close(); pb.server.close();
  for (const p of procs) p.kill("SIGTERM");
  writeFileSync(path.join(process.env.TMPDIR || "/tmp", "pp-streams-e2e-results.json"), JSON.stringify(results, null, 2));
  const failed = results.filter((r) => !r.ok).length;
  console.log(`\n${results.length - failed}/${results.length} passed`);
  process.exit(failed ? 1 : 0);
}
