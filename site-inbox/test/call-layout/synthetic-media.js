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
  function picture(label, w, h, fps) {
    const canvas = document.createElement("canvas"); canvas.width = w; canvas.height = h;
    const g = canvas.getContext("2d"); let frame = 0;
    const draw = () => {
      frame++;
      g.fillStyle = `hsl(${(frame * 3) % 360} 45% 30%)`; g.fillRect(0, 0, w, h);
      g.fillStyle = "#fff"; g.font = `bold ${Math.round(h / 9)}px sans-serif`; g.fillText(label + " " + frame, w / 24, h / 2);
    };
    setInterval(draw, 1000 / fps); draw();
    return canvas.captureStream(fps).getVideoTracks()[0];
  }
  const tag = (track, id) => { const settings = track.getSettings.bind(track); track.getSettings = () => ({ ...settings(), deviceId: id }); return track; };
  navigator.mediaDevices.getUserMedia = async (c) => {
    const out = new MediaStream();
    if (c.audio) out.addTrack(tag(tone(who === "alice" ? 440 : 660), "mic-a"));
    if (c.video) out.addTrack(tag(picture(who, 1280, 720, 15), "cam-a"));
    return out;
  };
  navigator.mediaDevices.getDisplayMedia = async () => new MediaStream([picture("screen " + who, 1280, 720, 5)]);
})();
