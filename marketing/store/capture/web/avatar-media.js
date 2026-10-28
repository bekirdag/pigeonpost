// Synthetic cameras and microphones for the store screenshots: every "camera" is a canvas-drawn,
// illustrated avatar of an invented person (flat shapes in that person's colours, gentle motion),
// and every microphone is a quiet tone. Never this machine's camera or microphone, never a photo.
//
// The page sets window.__PP_PERSON = "<id>" (and optionally window.__PP_SCENE = "presenter")
// before calling getUserMedia; injected with Page.addScriptToEvaluateOnNewDocument.
(() => {
  const PEOPLE = {
    maya:  { bg: ["#dbe6ff", "#b9cdfb"], shirt: "#2563eb", skin: "#f1c7a5", hair: "#3b2a20", style: "long" },
    leo:   { bg: ["#d9f4e2", "#b4e6c5"], shirt: "#16a34a", skin: "#c68e6a", hair: "#2b1d14", style: "short" },
    ana:   { bg: ["#fde0ef", "#f9c2de"], shirt: "#db2777", skin: "#f5d0b5", hair: "#8b4a2b", style: "bun" },
    priya: { bg: ["#ebe2ff", "#d6c6fd"], shirt: "#7c3aed", skin: "#a8714f", hair: "#1f1410", style: "long" },
    sam:   { bg: ["#ffe6d5", "#fdcaa6"], shirt: "#ea580c", skin: "#e9b48f", hair: "#c58b3a", style: "curly" },
    noor:  { bg: ["#d5f3f9", "#a9e4f0"], shirt: "#0e7490", skin: "#8d5a3b", hair: "#155e75", style: "scarf" },
    kai:   { bg: ["#fdf3c7", "#f8e191"], shirt: "#ca8a04", skin: "#f0c8a0", hair: "#1a1a1a", style: "spiky" },
    tomas: { bg: ["#e2e8f0", "#cbd5e1"], shirt: "#475569", skin: "#d9a07a", hair: "#5b5b5b", style: "beard" },
  };

  function drawPerson(g, w, h, p, t, scale = 1, cx = w / 2, base = h) {
    const s = (h / 720) * scale;
    const bob = Math.sin(t / 900) * 6 * s, sway = Math.sin(t / 1300) * 8 * s;
    const hx = cx + sway, hy = base - 430 * s + bob;
    // shoulders
    g.fillStyle = p.shirt;
    g.beginPath(); g.ellipse(cx + sway * 0.4, base + 40 * s, 300 * s, 230 * s, 0, Math.PI, 0); g.fill();
    g.fillStyle = "rgba(255,255,255,.18)";
    g.beginPath(); g.moveTo(cx - 55 * s + sway * .4, base - 180 * s); g.lineTo(cx + sway * .4, base - 110 * s); g.lineTo(cx + 55 * s + sway * .4, base - 180 * s); g.closePath(); g.fill();
    // hair behind
    g.fillStyle = p.hair;
    if (p.style === "long") { g.beginPath(); g.roundRect(hx - 178 * s, hy - 120 * s, 356 * s, 330 * s, [150 * s, 150 * s, 40 * s, 40 * s]); g.fill(); }
    if (p.style === "scarf") { g.beginPath(); g.ellipse(hx, hy + 30 * s, 190 * s, 220 * s, 0, 0, Math.PI * 2); g.fill();
      g.beginPath(); g.moveTo(hx - 190 * s, hy + 40 * s); g.quadraticCurveTo(hx, hy + 330 * s, hx + 190 * s, hy + 40 * s); g.lineTo(hx + 150 * s, hy + 260 * s); g.lineTo(hx - 150 * s, hy + 260 * s); g.fill(); }
    if (p.style === "bun") { g.beginPath(); g.arc(hx, hy - 175 * s, 70 * s, 0, Math.PI * 2); g.fill(); }
    // neck (a scarf covers it)
    if (p.style !== "scarf") { g.fillStyle = p.skin; g.fillRect(hx - 42 * s, hy + 110 * s, 84 * s, 110 * s); }
    // face
    g.fillStyle = p.skin; g.beginPath(); g.ellipse(hx, hy, 140 * s, 165 * s, 0, 0, Math.PI * 2); g.fill();
    // ears
    if (p.style !== "scarf" && p.style !== "long") { g.beginPath(); g.ellipse(hx - 140 * s, hy + 10 * s, 22 * s, 34 * s, 0, 0, Math.PI * 2); g.ellipse(hx + 140 * s, hy + 10 * s, 22 * s, 34 * s, 0, 0, Math.PI * 2); g.fill(); }
    // hair on top
    g.fillStyle = p.hair;
    const cap = (r1, r2, dy) => { g.beginPath(); g.ellipse(hx, hy - dy * s, r1 * s, r2 * s, 0, Math.PI, 0); g.fill(); };
    if (p.style === "short" || p.style === "beard") cap(150, 150, 40);
    if (p.style === "long" || p.style === "bun") { cap(152, 150, 30); g.beginPath(); g.ellipse(hx - 70 * s, hy - 70 * s, 90 * s, 55 * s, -0.5, 0, Math.PI * 2); g.fill(); }
    if (p.style === "curly") for (let i = -4; i <= 4; i++) { g.beginPath(); g.arc(hx + i * 34 * s, hy - 125 * s - Math.cos(i / 3) * 25 * s, 42 * s, 0, Math.PI * 2); g.fill(); }
    if (p.style === "spiky") { g.beginPath(); g.moveTo(hx - 150 * s, hy - 40 * s); for (let i = 0; i <= 8; i++) { g.lineTo(hx - 150 * s + i * 37.5 * s, hy - (i % 2 ? 200 : 120) * s); } g.lineTo(hx + 150 * s, hy - 40 * s); g.fill(); }
    if (p.style === "beard") { g.beginPath(); g.ellipse(hx, hy + 85 * s, 125 * s, 90 * s, 0, 0, Math.PI); g.fill(); }
    // eyes (with a blink now and then)
    const blink = (t % 4200) < 140;
    g.fillStyle = "#1f2430";
    for (const dx of [-52, 52]) { g.beginPath(); g.ellipse(hx + dx * s, hy - 5 * s, 15 * s, (blink ? 2 : 17) * s, 0, 0, Math.PI * 2); g.fill(); }
    // cheeks
    g.fillStyle = "rgba(240,110,110,.22)";
    for (const dx of [-85, 85]) { g.beginPath(); g.ellipse(hx + dx * s, hy + 45 * s, 26 * s, 16 * s, 0, 0, Math.PI * 2); g.fill(); }
    // smile, opening a little as if talking
    const talk = Math.max(0, Math.sin(t / 170)) * (p.talking ? 14 : 3);
    g.strokeStyle = "#7a2f2f"; g.lineWidth = 9 * s; g.lineCap = "round";
    if (p.style === "beard") g.strokeStyle = "#f3e1d3";
    g.beginPath(); g.ellipse(hx, hy + 55 * s, 45 * s, (16 + talk) * s, 0, 0.1, Math.PI - 0.1); g.stroke();
  }

  function background(g, w, h, p, t) {
    const gr = g.createLinearGradient(0, 0, w, h); gr.addColorStop(0, p.bg[0]); gr.addColorStop(1, p.bg[1]);
    g.fillStyle = gr; g.fillRect(0, 0, w, h);
    // a soft "room": a window shape and a plant, so it reads as a camera, not a sticker
    g.fillStyle = "rgba(255,255,255,.35)"; g.fillRect(w * 0.07, h * 0.1, w * 0.22, h * 0.42);
    g.fillStyle = "rgba(0,0,0,.05)"; g.fillRect(w * 0.07, h * 0.1 + h * 0.21 - 3, w * 0.22, 6); g.fillRect(w * 0.18 - 3, h * 0.1, 6, h * 0.42);
    g.fillStyle = "rgba(22,101,52,.35)";
    for (let i = 0; i < 5; i++) { g.beginPath(); g.ellipse(w * 0.86 + Math.sin(t / 2000 + i) * 3, h * 0.55 - i * h * 0.05, w * 0.03, h * 0.09, (i - 2) * 0.45, 0, Math.PI * 2); g.fill(); }
    g.fillStyle = "rgba(120,72,40,.45)"; g.fillRect(w * 0.83, h * 0.62, w * 0.06, h * 0.12);
  }

  function slide(g, x, y, w, h, t) {
    g.fillStyle = "#ffffff"; g.fillRect(x, y, w, h);
    g.fillStyle = "#16326b"; g.font = `700 ${Math.round(h * 0.085)}px Geist, -apple-system, Helvetica, sans-serif`;
    g.fillText("New onboarding — 3 screens", x + w * 0.06, y + h * 0.16);
    const cols = ["#2563eb", "#7c3aed", "#16a34a"], titles = ["Pick a /handle", "Find your people", "Say hello"];
    for (let i = 0; i < 3; i++) {
      const px = x + w * (0.06 + i * 0.31), py = y + h * 0.26, pw = w * 0.26, ph = h * 0.64;
      g.fillStyle = "#eef2f8"; g.beginPath(); g.roundRect(px, py, pw, ph, h * 0.04); g.fill();
      g.fillStyle = cols[i]; g.beginPath(); g.arc(px + pw / 2, py + ph * 0.25, ph * 0.11, 0, Math.PI * 2); g.fill();
      g.fillStyle = "#14181f"; g.font = `600 ${Math.round(h * 0.05)}px Geist, -apple-system, Helvetica, sans-serif`;
      const tw = g.measureText(titles[i]).width; g.fillText(titles[i], px + (pw - tw) / 2, py + ph * 0.52);
      g.fillStyle = "#cfd5de"; for (let k = 0; k < 2; k++) g.fillRect(px + pw * 0.15, py + ph * (0.62 + k * 0.08), pw * (0.7 - k * 0.2), h * 0.02);
      g.fillStyle = cols[i]; g.beginPath(); g.roundRect(px + pw * 0.15, py + ph * 0.8, pw * 0.7, ph * 0.1, ph * 0.05); g.fill();
    }
    // a pointer that drifts over the second screen
    const mx = x + w * 0.5 + Math.sin(t / 900) * w * 0.05, my = y + h * 0.55 + Math.cos(t / 1100) * h * 0.04;
    g.fillStyle = "#14181f"; g.beginPath(); g.moveTo(mx, my); g.lineTo(mx + 18, my + 44); g.lineTo(mx + 26, my + 26); g.lineTo(mx + 44, my + 20); g.closePath(); g.fill();
  }

  function camera(id, w = 640, h = 360, fps = 15) {
    const p = Object.assign({}, PEOPLE[id] || PEOPLE.maya, { talking: window.__PP_TALKING === id });
    const cv = document.createElement("canvas"); cv.width = w; cv.height = h;
    const g = cv.getContext("2d");
    const seed = Object.keys(PEOPLE).indexOf(id) * 777;
    const draw = () => {
      const t = performance.now() + seed;
      if (window.__PP_SCENE === "presenter") {
        g.fillStyle = "#1d2433"; g.fillRect(0, 0, w, h);
        slide(g, w * 0.04, h * 0.08, w * 0.64, h * 0.84, t);
        g.save(); g.beginPath(); g.roundRect(w * 0.71, h * 0.5, w * 0.26, h * 0.42, 24); g.clip();
        background(g, w, h, p, t); g.restore();
        g.save(); g.beginPath(); g.roundRect(w * 0.71, h * 0.5, w * 0.26, h * 0.42, 24); g.clip();
        g.fillStyle = p.bg[1]; g.fillRect(w * 0.71, h * 0.5, w * 0.26, h * 0.42);
        drawPerson(g, w, h, Object.assign({}, p, { talking: true }), t, 0.36, w * 0.84, h * 0.97);
        g.restore();
      } else {
        background(g, w, h, p, t);
        drawPerson(g, w, h, p, t, 0.95, w / 2, h * 1.02);
      }
    };
    setInterval(draw, 1000 / fps); draw();
    return cv.captureStream(fps).getVideoTracks()[0];
  }

  function microphone(freq) {
    const ctx = new AudioContext({ sampleRate: 48000 }); const o = ctx.createOscillator(); o.frequency.value = freq;
    const gain = ctx.createGain(); gain.gain.value = 0.02; const d = ctx.createMediaStreamDestination(); o.connect(gain).connect(d); o.start();
    return d.stream.getAudioTracks()[0];
  }

  const DEV = [
    { kind: "audioinput", deviceId: "mic-a", label: "Synthetic microphone", groupId: "g1" },
    { kind: "videoinput", deviceId: "cam-a", label: "Synthetic camera", groupId: "g1" },
    { kind: "audiooutput", deviceId: "default", label: "Synthetic speaker", groupId: "g1" },
  ];
  const md = navigator.mediaDevices;
  md.enumerateDevices = async () => DEV.map((d) => ({ ...d, toJSON() { return d; } }));
  const tag = (t, id) => { const s = t.getSettings.bind(t); t.getSettings = () => ({ ...s(), deviceId: id }); return t; };
  md.getUserMedia = async (c) => {
    const who = window.__PP_PERSON || "maya";
    const out = new MediaStream();
    if (c && c.audio) out.addTrack(tag(microphone(300 + Object.keys(PEOPLE).indexOf(who) * 40), "mic-a"));
    if (c && c.video) out.addTrack(tag(window.__PP_SCENE === "presenter" ? camera(who, 1280, 720, 24) : camera(who), "cam-a"));
    return out;
  };
  md.getDisplayMedia = async () => { window.__PP_SCENE = "presenter"; return new MediaStream([camera(window.__PP_PERSON || "priya", 1280, 720)]); };
  window.__ppAvatarFrame = (id, w, h) => { const cv = document.createElement("canvas"); cv.width = w; cv.height = h; const g = cv.getContext("2d"); background(g, w, h, PEOPLE[id], 0); drawPerson(g, w, h, PEOPLE[id], 0, 0.95, w / 2, h * 1.02); return cv.toDataURL(); };
})();
