# Large calls (7–30 people): media server, end-to-end encryption, threat model

Live streams in groups use the same media server and frame encryption, with the stream key
distributed through the postbox's key directory; see `docs/streaming.md`.

Calls of up to six people stay a WebRTC mesh: every pair of browsers has its own DTLS-SRTP
connection (see `deploy/calls/README.md`). A seventh person moves the call onto a media server, a
self-hosted LiveKit SFU (`livekit-server` 1.13.9, Apache-2.0) on the media host. Calls can hold up to
30 people. This page describes how media stays end-to-end encrypted there, and what that protects
against and what it does not.

## How it works

1. **Admission.** The calls service decides who joins, exactly as for small calls. Members are
   checked against the postbox; somebody already in the call admits guests. A call grows past six
   only if all of these hold:
   - every participant, newcomer included, said its browser can encrypt its own frames;
   - a media server is configured;
   - the media server's bandwidth budget has room for the extra person (`call_busy` otherwise).

   There is no unencrypted fallback. A browser without frame encryption is told "This call needs a
   newer browser or app" before it knocks, or when it is refused.
2. **Moving.** Everyone already in the call gets a `topology` event and connects to the media
   server. Each keeps its direct mesh connections until the media server carries that person's
   voice; the direct connection is then closed. Nobody is dropped. A call that has moved stays on
   the media server until it ends.
3. **Tickets.** For each participant the calls service mints a LiveKit access token: HS256, 120 s,
   one room, identity = the calls service's random participant id (no name, no mailbox),
   publish/subscribe/data only, sources limited to microphone, camera and screen. The API secret
   lives only in `/etc/pigeonpost-calls/calls.env` on raijin and `/etc/livekit/livekit.env` on the
   media host.
4. **Keys** (`site-inbox/calls-e2ee.js`).
   - Every browser makes a fresh ECDH P-256 key pair for the call. The private half cannot be
     exported. The browser announces the public half through the calls service.
   - The **key leader** is the earliest participant (in the call's join order) holding the newest
     room-key generation. Whenever the set of announced keys changes (someone joins, leaves or
     reloads), the leader makes a new random 256-bit room key, one generation ("epoch") higher.
   - The leader wraps the key to every other participant: ECDH → HKDF-SHA-256 → AES-256-GCM. The
     wrap is bound to the room, both participant ids, both public keys and the epoch. The wrap
     travels as a `key` signal to that one participant.
   - The room key feeds LiveKit's frame encryption (AES-GCM per frame, in a worker; key slot =
     epoch mod 16). A new key is used for sending 1.5 s after it arrives, by which time everyone
     can decrypt with it. Old keys stay in the 16-slot ring for frames still in flight.
   - Chat and status messages are sealed separately with AES-256-GCM under a key derived from the
     same room key, bound to room, sender id and epoch. They go over the media server's data
     channel.
5. **The security code.** Everyone sees one code for the whole call: 12 digits computed with
   PBKDF2-SHA-256 (100,000 rounds) over every participant's id, identity (verified mailbox, or
   `guest:` plus the typed name) and public key. It changes whenever someone joins, leaves or
   reloads. It is shown in the call header and on the "You" row of the people panel.
6. **What browsers refuse.** A remote track that the media server describes as unencrypted is
   never subscribed or played. Media-server participants the calls service did not list are
   ignored. Frames that do not decrypt are dropped, never shown.

## Threat model

| Adversary | Can | Cannot |
| --- | --- | --- |
| The media server (or whoever runs or breaks into it) | See who is connected (random ids), when, from which IP, how much each sends, and audio levels (RTP header extension, needed for active-speaker detection). Drop, delay or reorder frames; swap whose frame goes to which tile. | Decrypt audio, video, screens, chat or status. It never receives a room key, wrapped or not. Inject media: unencrypted tracks are refused, and frames without the room key do not decrypt. |
| The calls service (signaling) | Everything above about metadata, plus names and mailboxes. Refuse service. Relay wrapped keys, which it cannot open. | Read media while the participants compare the security code. To read media it must hand out public keys of its own (a man in the middle) or add a hidden participant. Both change the code on at least one screen; a hidden participant also appears in everyone's people list. |
| Someone with network access (Wi-Fi, ISP) | See encrypted DTLS-SRTP traffic to the media host. | Read or alter media; the frames inside DTLS-SRTP are encrypted again end to end. |
| A participant | Decrypt everything sent while they are in the call (that is the point). Record it on their own device. Send key wraps or garbage that make others' media stall (denial of service). | Read media sent before they joined or after they left: the key rotates on every change, and the calls service removes leavers from the media server. |

**Honest limits.**

- **Codes must actually be compared.** As with the mesh, protection against the signaling service
  depends on people reading the code to each other (out loud, or on a channel they trust). Nobody
  is forced to.
- **The code is long and slow on purpose.** A service in the middle chooses the fake keys it hands
  out, so it could otherwise search for keys that happen to give matching codes. Twelve digits (≈40
  bits) behind 100,000 PBKDF2 rounds make that search impractical while a call is being set up, but
  not impossible for an adversary with very large computing resources. A commit-and-reveal key
  exchange would remove that search entirely; it is not built.
- **The mesh codes are weaker.** The existing six-digit per-pair codes in small calls cover the DTLS
  fingerprints with one SHA-256 and have the same searchable property, with only ≈20 bits. That is
  unchanged by this work and worth strengthening separately.
- **One shared room key.** Everyone in the call holds the same key, so a participant colluding with
  the media server could forge media or chat that appears to come from another participant.
  Per-sender keys with signatures would prevent that.
- **Metadata is not hidden** (see the table): who was in which call, when, for how long, and roughly
  who spoke.
- **The browser is the endpoint.** Pages are served by `inbox.pigeonpost.dev`; whoever controls that
  origin controls the code that does the encryption. The vendored LiveKit client is pinned by hash
  (`site-inbox/vendor/livekit-client/README.md`), and the browser checks it on load with subresource
  integrity. That protects against a corrupted file, not against a compromised origin.
- **Nothing is recorded.** The media server has no recording component (no Egress), and the published
  privacy commitment is unchanged.

## Browser and app support (tested 2026-10-20, synthetic media only)

| Client | Mechanism | Result |
| --- | --- | --- |
| Chrome 155 (desktop, headless) | encoded insertable streams | Pass: 7 participants, all decode all; frames on the wire carry the encryption trailer; a wrong key decodes 0 frames. |
| Firefox 157/158 (macOS, headless) | RTCRtpScriptTransform | Pass: joined as 7th and 8th; simulcast; decrypts all; others decrypt it. |
| Safari 17.6 (macOS 14.6) | RTCRtpScriptTransform | Pass: same code and key; decrypts 7 audio and 7 video; others decrypt its video. |
| Mac app, WKWebView (macOS 14.6) | RTCRtpScriptTransform | Pass: led the key exchange; 0 decryption errors. It publishes one 360p layer (see below). |
| iOS app, WKWebView (iOS 18.3.1 simulator) | RTCRtpScriptTransform | Pass after `calls-e2ee-worker.js`: simulcast, decrypts all, others decrypt it. |
| Android app, WebView (Chrome 113, Android 14 emulator) | encoded insertable streams | Pass: same code, 0 decryption errors, audio both ways and video decrypted by others. Emulator video is slow. |
| Edge, Windows app (WebView2) | encoded insertable streams | Not tested here: same Chromium mechanism as Chrome and Android WebView. |

Two WebKit findings shaped the code:

- **Room keys go to the worker as raw bytes.** iOS WKWebView cannot structured-clone a `CryptoKey`
  into a worker ("The object can not be cloned"). `calls-e2ee-worker.js` wraps LiveKit's unmodified
  worker and imports the key inside it.
- **Some WebKit clients send one layer.** LiveKit enables simulcast with encryption only where it
  recognises Safari ≥ 17.2 (WebKit bug 257803, a quality bug). macOS WKWebView user agents carry no
  Safari version. Those clients therefore send one layer, capped by `calls.js` at 360p and
  0.6 Mbit/s.
