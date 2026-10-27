// Synthetic media for automated checks: never this machine's camera or microphone. The picture
// carries the capture time as 32 black/white squares so a viewer can measure glass-to-glass delay.
(() => {
  window.PIGEONPOST_INBOX = { postbox: window.__POSTBOX || "http://127.0.0.1:8891" };
  function synth(w, h, label) {
    const c = document.createElement("canvas"); c.width = w; c.height = h;
    const x = c.getContext("2d"); let f = 0;
    setInterval(() => {
      f++;
      x.fillStyle = `hsl(${(f * 3) % 360},60%,40%)`; x.fillRect(0, 0, w, h);
      x.fillStyle = "#fff"; x.font = "48px sans-serif"; x.fillText(label + " " + f, 60, h / 2);
      const t = Date.now() >>> 0;
      for (let i = 0; i < 32; i++) { x.fillStyle = (t >>> (31 - i)) & 1 ? "#fff" : "#000"; x.fillRect(i * (w / 32), 0, w / 32, 40); }
    }, 33);
    return c.captureStream(30).getVideoTracks()[0];
  }
  function tone() {
    const ac = new AudioContext(); const o = ac.createOscillator(); o.frequency.value = 440; const d = ac.createMediaStreamDestination(); o.connect(d); o.start();
    return d.stream.getAudioTracks()[0];
  }
  const md = navigator.mediaDevices;
  md.getUserMedia = async (c) => new MediaStream([...(c && c.audio ? [tone()] : []), ...(c && c.video ? [synth(1280, 720, "camera")] : [])]);
  md.getDisplayMedia = async () => new MediaStream([synth(1920, 1080, "screen")]);
  window.__lat = [];
  setInterval(() => {
    const v = document.getElementById("live-video");
    if (!v || !v.videoWidth) return;
    const c = document.createElement("canvas"); c.width = 1280; c.height = 720; const x = c.getContext("2d");
    x.drawImage(v, 0, 0, 1280, 720);
    let n = 0;
    for (let i = 0; i < 32; i++) { const d = x.getImageData(i * 40 + 20, 20, 1, 1).data; n = n * 2 + (d[0] > 128 ? 1 : 0); }
    const lat = ((Date.now() >>> 0) - n + 2 ** 32) % 2 ** 32;
    if (lat < 20000) window.__lat.push(lat);
  }, 700);
})();
