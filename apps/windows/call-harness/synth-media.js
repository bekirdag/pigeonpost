// Synthetic devices for call tests: two microphones and two cameras (each a distinct tone/picture),
// and a synthetic screen. Never touches real hardware.
(function () {
  const who = new URLSearchParams(location.search).get("as") || localStorage.getItem("pp_test_as") || "guest";
  const DEV = [
    { kind: "audioinput", deviceId: "mic-a", label: "Test microphone A", groupId: "g1" },
    { kind: "audioinput", deviceId: "mic-b", label: "Test microphone B", groupId: "g2" },
    { kind: "videoinput", deviceId: "cam-a", label: "Test camera A", groupId: "g1" },
    { kind: "videoinput", deviceId: "cam-b", label: "Test camera B", groupId: "g2" },
    { kind: "audiooutput", deviceId: "default", label: "Test speaker", groupId: "g1" },
  ];
  navigator.mediaDevices.enumerateDevices = async () => DEV.map((d) => ({ ...d, toJSON() { return d; } }));
  const pick = (c, kind) => { const want = c && c.deviceId && (c.deviceId.exact || c.deviceId.ideal || c.deviceId); return typeof want === "string" ? want : kind === "audio" ? "mic-a" : "cam-a"; };
  function tone(freq) {
    const ctx = new AudioContext({ sampleRate: 48000 }); const o = ctx.createOscillator(); o.frequency.value = freq;
    const g = ctx.createGain(); g.gain.value = 0.3; const d = ctx.createMediaStreamDestination(); o.connect(g).connect(d); o.start();
    const t = d.stream.getAudioTracks()[0]; const settings = t.getSettings.bind(t); return t;
  }
  function picture(label, w = 1920, h = 1080, fps = 30) {
    const cv = document.createElement("canvas"); cv.width = w; cv.height = h; const g = cv.getContext("2d"); let f = 0;
    const draw = () => { f++; g.fillStyle = `hsl(${(f * 3) % 360} 55% 32%)`; g.fillRect(0, 0, w, h);
      for (let i = 0; i < 30; i++) { g.fillStyle = `hsl(${(i * 37 + f) % 360} 70% 60%)`; g.fillRect((i * 97 + f * 7) % w, (i * 53 + f * 3) % h, w / 14, w / 14); }
      g.fillStyle = "#fff"; g.font = `bold ${Math.round(h / 9)}px sans-serif`; g.fillText(label + " " + f, w / 24, h / 5); };
    setInterval(draw, 1000 / fps); draw(); return cv.captureStream(fps).getVideoTracks()[0];
  }
  const tag = (t, id) => { const s = t.getSettings.bind(t); t.getSettings = () => ({ ...s(), deviceId: id }); t.__device = id; return t; };
  navigator.mediaDevices.getUserMedia = async (c) => {
    const out = new MediaStream();
    if (c.audio) { const id = pick(c.audio, "audio"); out.addTrack(tag(tone(id === "mic-b" ? 880 : (who === "alice" ? 440 : who === "bob" ? 660 : 550)), id)); }
    if (c.video) { const id = pick(c.video, "video"); out.addTrack(tag(picture(who + (id === "cam-b" ? " cam-B" : "")), id)); }
    return out;
  };
  navigator.mediaDevices.getDisplayMedia = async () => new MediaStream([picture("SCREEN " + who, 1920, 1080, 15)]);
})();
