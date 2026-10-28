// Synthetic devices for the call layout check: a tone for the microphone, a moving picture for the
// camera and for a shared screen. Replaces getUserMedia/getDisplayMedia before the page runs, so no
// real camera, microphone or screen is ever touched.
(function () {
  "use strict";
  const who = new URLSearchParams(location.search).get("as") || "guest";
  const DEVICES = [
    { kind: "audioinput", deviceId: "mic-a", label: "Test microphone", groupId: "g1" },
    { kind: "videoinput", deviceId: "cam-a", label: "Test camera", groupId: "g1" },
    { kind: "audiooutput", deviceId: "default", label: "Test speaker", groupId: "g1" },
  ];
  navigator.mediaDevices.enumerateDevices = async () => DEVICES.map((d) => ({ ...d, toJSON() { return d; } }));
  function tone(freq) {
    const ctx = new AudioContext({ sampleRate: 48000 });
    const osc = ctx.createOscillator(); osc.frequency.value = freq;
    const gain = ctx.createGain(); gain.gain.value = 0.2;
    const out = ctx.createMediaStreamDestination();
    osc.connect(gain).connect(out); osc.start();
    return out.stream.getAudioTracks()[0];
  }
  // `?cam=portrait` starts the camera upright (720x1280), as a phone held upright sends it;
  // `__turnCamera("landscape" | "portrait")` turns it mid-call, as turning the phone does.
  // `?cam=rotated` sends what a phone's camera does: landscape pictures from the sensor marked as
  // turned a quarter (VideoFrame rotation), which WebRTC carries as the video-orientation header
  // extension or applies before sending. Upright, then turned by changing the mark.
  const cameras = [];
  let rotation = 90;
  window.__turnCamera = (shape) => {
    rotation = shape === "portrait" ? 90 : 0;
    for (const c of cameras) { if (c.rotated) continue; const [w, h] = shape === "portrait" ? [720, 1280] : [1280, 720]; c.width = w; c.height = h; }
  };
  function picture(label, w, h, fps) {
    const canvas = document.createElement("canvas"); canvas.width = w; canvas.height = h;
    const g = canvas.getContext("2d"); let frame = 0;
    const draw = () => {
      frame++;
      const cw = canvas.width, ch = canvas.height;
      g.fillStyle = `hsl(${(frame * 3) % 360} 45% 30%)`; g.fillRect(0, 0, cw, ch);
      g.fillStyle = "#fff"; g.font = `bold ${Math.round(Math.min(cw, ch) / 9)}px sans-serif`; g.fillText(label + " " + frame, cw / 24, ch / 2);
    };
    setInterval(draw, 1000 / fps); draw();
    return { canvas, track: canvas.captureStream(fps).getVideoTracks()[0] };
  }
  const camera = (label) => {
    const cam = new URLSearchParams(location.search).get("cam");
    if (cam === "rotated") {
      const p = picture(label, 1280, 720, 15);
      p.canvas.rotated = true;
      cameras.push(p.canvas);
      const generator = new MediaStreamTrackGenerator({ kind: "video" });
      const writer = generator.writable.getWriter();
      setInterval(() => { writer.write(new VideoFrame(p.canvas, { timestamp: Math.round(performance.now() * 1000), rotation })); }, 1000 / 15);
      return generator;
    }
    const portrait = cam === "portrait";
    const p = portrait ? picture(label, 720, 1280, 15) : picture(label, 1280, 720, 15);
    cameras.push(p.canvas);
    return p.track;
  };
  const tag = (track, id) => { const settings = track.getSettings.bind(track); track.getSettings = () => ({ ...settings(), deviceId: id }); return track; };
  navigator.mediaDevices.getUserMedia = async (c) => {
    const out = new MediaStream();
    if (c.audio) out.addTrack(tag(tone(who === "alice" ? 440 : 660), "mic-a"));
    if (c.video) out.addTrack(tag(camera(who), "cam-a"));
    return out;
  };
  navigator.mediaDevices.getDisplayMedia = async () => new MediaStream([picture("screen " + who, 1280, 720, 5).track]);
})();
