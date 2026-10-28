# Live streams in groups

Status: Phase 6 (SDS §14A, R-29). Server, calls service and web inbox; native apps watch and go live
through their call web view (`native-call.html`, `PigeonpostNative.watchStream` / `goLive`). Behind the
postbox feature flag `streaming` (with `groups`).

A member of a group goes live with a camera and/or a shared screen and a title. The group shows a
red **LIVE** banner ("Jane is live: Sprint demo — Watch"). Members watch full screen, with a live
viewer count, reactions (👍 👏 ❤️, throttled) and the group's own chat beside the player (over the
bottom of it on phones). The streamer can invite a viewer onto the stage; nothing is opened on the
viewer's device until they accept. Stopping posts a `stream` notice in the group: "Jane streamed for
34 min · 18 viewers". Streams are not recorded (D-23).

## 1. Rules

| Item | Rule |
| --- | --- |
| Who may go live | The group setting `stream_policy`: `admins` (default) or `everyone`. Only active, non-restricted members, and only a person (`human_required` for API keys, mailbox tokens and connectors). |
| Who may watch | Active members of the group (restricted members too). Agents (API keys) may watch; a connector acting for the member may not. |
| The stage | Only people the streamer (or a group admin) invited, after they accept. Restricted members cannot be invited, and lose the stage if restricted. At most 6 besides the streamer. |
| Stop | The streamer, or a group owner/admin. |
| Streams per group | One live stream at a time (`409 stream_in_progress {stream_id}`). |
| Length | 4 hours, then the stream ends (`max_duration`). |
| Viewers | 200 per stream (`PIGEONPOST_STREAM_MAX_VIEWERS` on the postbox, `CALLS_STREAM_MAX_VIEWERS` on the calls service); an organization's owner or admin can set a lower `settings.stream_max_viewers` for its rooms (`PATCH /v1/orgs/{id}`, portal → Security; `0` restores the default), which `stream-admission` returns as `max_viewers`; and fewer when the media server's budget requires it (§5): `409 stream_full` at the configured limit, `503 stream_capacity` when the budget is the limit. |
| Ends when | The streamer stops or leaves for good (30 s grace for a reload), the streamer may no longer stream (policy changed to admins, restricted, removed), the group is deleted, four hours pass, or the calls service stops reporting the stream for 2 minutes. |

Policy changes and removals take effect on the media server and on key access, not only in the UI:
the calls service asks the postbox every 10 s (and at once when the streamer's page sees a membership
change) and removes anyone who lost their role from the media server; the streamer's browser rotates
the stream key without them (§4).

## 2. Postbox API (`/v1`, flag `streaming`)

Acting mailbox: `x-pigeonpost-identity` as for groups. Errors are `{error, message}`.

| Method | Path | Body | Result |
| --- | --- | --- | --- |
| POST | `/v1/groups/{id}/streams` | `{title (1–100), sources: ["camera","screen"], public_key}` | `201 {stream, role: "host", host_key, media: {join}}`. `public_key` is the streamer's per-stream P-256 key (§4). Errors: `stream_restricted`, `stream_in_progress {stream_id}`, `group_not_ready`, `not_member`, `human_required`, `bad_key`, `rate_limited` (6 starts/hour). |
| GET | `/v1/groups/{id}/streams` | | `{streams: [stream]}` — the live stream, if any. Active members only. |
| GET | `/v1/streams/{id}` | | `{stream, role}`. `404 no_stream` for anyone with no relationship to the group. |
| POST | `/v1/streams/{id}/watch` | `{public_key}` | Registers this device's receive key in the stream's key directory and answers `{stream, role, host_key, media: {join}}`. Up to 4 keys per member (devices); the oldest drops out. The streamer calls it after a reload to replace the sending key (`host_key_changed` event). `410 stream_ended`. |
| GET | `/v1/streams/{id}/audience` | | Streamer only: `{audience: [{address, handle, role, keys: [...]}]}` — current active members with registered keys. The streamer wraps the stream key to these keys and no others. |
| POST | `/v1/streams/{id}/stage` | `{address, on}` | Streamer/admin about someone else: invite (`on`) or take off. The invited member about themself: accept (`on`, a person only) or decline/step down. `{stream, address, role}`. Errors: `not_invited`, `not_host`, `stream_restricted`, `stage_full`. |
| DELETE | `/v1/streams/{id}` | | Stop. Streamer or group admin. Idempotent. |

`stream` objects: `{id, group_id, title, sources, by, by_handle, state: live|ended, started_at, ends_by,
ended_at, end_reason, viewers, peak_viewers, stage: [{address, state: invited|speaker}], max_viewers,
revision}`. Groups gain `stream_policy` (create, `PATCH /v1/groups/{id}`, group views; a
`settings_changed` notice says so).

Typed event `group.stream` to the group's active members: `{group_id, stream_id, state: live|ended,
title, by, revision}` plus `stage {address, state, by}` for stage changes, `host_key_changed` after the
streamer's reload, and `{reason, duration, viewers}` when it ends. Membership is rechecked at delivery,
as for every `group.*` event.

The `stream` notice is authenticated server metadata in the group's sequence, rendered by each client
in its own words: `{kind: "stream", actor: <streamer>, metadata: {stream_id, duration, viewers, reason}}`.
It carries no title, and nothing is posted as a message into the end-to-end encrypted group content.

### Internal (calls service token, `/internal/v1`)

| Path | Body | Answer |
| --- | --- | --- |
| `stream-admission` | `{stream_id, from: {mailbox}, public_key?}` | `{allowed, reason?, role: host\|speaker\|invited\|viewer, group_id, host, host_key, sources, state, ends_by, max_viewers, key_attested}` |
| `stream-check` | `{stream_id, viewers, members: [mailbox]}` | `{state, reason, ends_by, host_key, roles: {mailbox: role\|null}}` — also the heartbeat (viewer count, peak). |
| `stream-end` | `{stream_id, reason: host_left\|media_failed\|max_duration\|capacity}` | `{ok, state}` |

Ended streams' rows (title, stage, keys) are deleted 7 days after the end; the notice stays with the group.

## 3. Calls service (`/calls/v1/streams/{id}/…`, member token)

| Path | Body | Notes |
| --- | --- | --- |
| `join` | `{session, from, public_key, e2ee: true}` | Asks the postbox; refuses a key the postbox did not register (`key_not_registered`) and a browser without frame encryption (`needs_e2ee`). Viewers wait for the streamer (`not_started`). Answers `{you, role, host: {participant, mailbox, key}, publishers, viewers, max_viewers, sfu: {url, token, ttl}}`. |
| `sfu` | `{session}` | A fresh ticket for the current role. |
| `keys` | `{session, epoch, wraps: [{to, iv, key}]}` | Streamer only; relayed as `stream_key` events to each participant. ≤ 1,000 wraps per batch. |
| `react` | `{session, sealed}` | A reaction sealed with the stream key; 6 per 10 s per participant, batched to everyone every 0.5 s (`stream_reactions`). |
| `role` | `{session}` | After the stage changed: asks the postbox and applies the answer on the media server. |
| `recheck` | `{session}` | Streamer only: check everyone's role now. |
| `leave`, `stop` | `{session}` | `stop` is the streamer's, after `DELETE /v1/streams/{id}`. |

Events on the calls event stream: `stream_participant {change: joined|left|role, reason, participant,
mailbox, role, key}` (viewers' changes go to the streamer only), `stream_key`, `stream_viewers {count}`,
`stream_reactions {items}`, `stream_role {role}`, `stream_ended {reason}`.

Media-server tickets (HS256, 120 s, one room, identity = a random participant id):

| Role | Grant |
| --- | --- |
| streamer | `canPublish`, sources microphone + camera and/or screen as chosen, `canSubscribe`, no data |
| speaker (accepted) | `canPublish` microphone + camera, `canSubscribe`, no data |
| viewer, invited | subscribe only, `hidden` (invisible to the other participants), no data |

A stage change is applied to the connected participant with LiveKit's `UpdateParticipant`; the media
server itself refuses a viewer's publish ("insufficient permissions"). Stream rooms are `stream_<id>`,
created through the admin API with their own size before the first ticket is used, separate from call
rooms (whose default size stays 30).

## 4. End-to-end encryption and threat model

Media is encrypted per frame in the publishing browser with a random 256-bit **stream key** (LiveKit's
frame encryption, AES-GCM, as for large calls; `site-inbox/calls-e2ee.js`, `calls-e2ee-worker.js`).
How the key moves:

1. Every watching device makes a fresh, non-exportable P-256 key pair for the stream and registers the
   public half with the **postbox** (`POST /watch`). The streamer registered theirs when going live.
2. The calls service tells the streamer who joined the media room, with the key each one announced.
   The streamer's browser wraps the stream key (ECDH → HKDF-SHA-256 → AES-256-GCM, bound to the stream,
   both participant ids, both keys and the generation) **only to participants whose key the postbox's
   directory lists for a current active member**.
3. A viewer accepts a wrapped key only from the streamer's participant and only if it was wrapped with
   the streamer key **the postbox** returned; it refuses to show anything if the calls service names a
   different streamer key. Nothing is received from the media server before a key is held.
4. When someone leaves, a new key is made within 10 s; when someone is removed from the group (the
   calls service's check, the streamer's own refresh of the directory every 30 s, or the group event),
   within a second, and wrapped to everyone still in the directory. Old keys stay only for frames in
   flight. Reactions are sealed with a key derived from the stream key.

| Adversary | Can | Cannot |
| --- | --- | --- |
| The media server | See who is connected (random ids), when, from which address, how much flows. Drop or delay frames. | Decrypt media or reactions; publish into the stream (frames without the key do not decrypt, unencrypted tracks are refused). |
| The calls service (signaling) | Everything above plus mailboxes and the viewer count. Refuse service. Relay wrapped keys it cannot open. | Get the key: it cannot add a key of its own to the postbox's directory, and viewers check the streamer's key against the postbox. Keep a removed member watching: the streamer re-reads the directory and rotates. |
| The postbox | See who registered to watch, stream titles, times and counts. Add a key of its own to the directory (it holds hosted mailboxes' identity keys anyway). | Get media without also controlling the media path. |
| Postbox **and** calls service together | Substitute keys (a man in the middle). | Do so unnoticed by people who compare the stream security code: a viewer's info panel shows a 12-digit code over the streamer's and the viewer's keys; the streamer sees the same code next to that viewer's name. |
| A viewer | Decrypt and record what they watch (that is watching). Send reactions. | Publish (the ticket has no publish right), or read anything after removal (the key rotates and the media server drops them; tested with a viewer that kept every old key and reconnected through a colluding media server: 0 frames decoded). |

Honest limits: comparing the code is optional; one shared stream key means a speaker colluding with
the media server could forge frames that seem to come from the streamer; metadata is not hidden; the
page code comes from `inbox.pigeonpost.dev` (the vendored LiveKit client is pinned by hash). There is
no unencrypted fallback: a browser or web view without frame encryption is told "Live streams need a
newer browser or app".

## 5. Capacity (measured on the production media host, 2026-10-27)

Media host 193.164.5.36: 5 vCPU (Xeon Gold 6138), 8 GB, upload measured 1.8–2.0 Gbit/s
(`deploy/calls/README.md`). LiveKit 1.13.9. Calls and streams share one planned budget,
`CALLS_SFU_BUDGET_MBPS=400`.

Real browsers: one streamer and N headless-Chrome viewers on raijin through the production media
server and a throwaway calls service (`tests/e2e-streams/scale.mjs`), synthetic 720p camera, all
frames end-to-end encrypted:

| Viewers | Decoding | Received per viewer | Media host out | LiveKit CPU | Host CPU | Delay median / p90 |
| --- | --- | --- | --- | --- | --- | --- |
| 10 | 10/10 at 1280×720, 30 fps | 0.91 Mbit/s | 10.8 Mbit/s | 16% of a core | 4% | 74 / 90 ms |
| 30 | 30/30 | 0.95 Mbit/s | 33.9 Mbit/s | 43% | 9% | 88 / 110 ms |
| 50 | 50/50 | 0.95 Mbit/s | 54.0 Mbit/s | 62% | 14% | 91 / 119 ms |

0 decryption errors throughout. The delay is glass to glass (capture time drawn into the picture, read
back on the viewer; same clock), well under the 1 s target.

Simulated subscribers (`lk load-test` 2.18.8, one VP8 simulcast publisher at ~2.0 Mbit/s, no
decoding), from raijin:

| Subscribers | Received each | Loss | Media host out | LiveKit CPU | Host CPU |
| --- | --- | --- | --- | --- | --- |
| 25 | 2.0 Mbit/s | 0% | 59.7 Mbit/s | 32% | 7% |
| 100 | 2.0 Mbit/s | 0.9% | 240 Mbit/s | 93% | 19% |
| 155 (of 160) | 1.6 Mbit/s | 3.6% | 367 Mbit/s | 139% | 28% |

At 155 the media host sent 2.37 Mbit/s per subscriber while raijin received ~250 Mbit/s in total:
the loss is raijin's own download link, not the media host. One client site could not push the media
host further; beyond 155 the figures below are arithmetic, not observation.

What this sets:

- **CPU** is not the limit: ~0.9% of one core per viewer; 155 viewers used 1.4 of 5 vCPUs.
  `CALLS_STREAM_TOTAL_VIEWERS=400` (all streams together, ≈ 3.6 cores) keeps headroom.
- **Bandwidth** is: on the wire a viewer costs ~18% more than the media bitrate. A camera stream
  (720p capped at 1.7 Mbit/s + voice) is planned at **2.1 Mbit/s** per viewer, a stream with a shared
  screen (2.5 Mbit/s + voice + small camera) at **3.2 Mbit/s**.
- **Effective viewer limit** with nothing else on the media server: camera streams
  ⌊400 / 2.1⌋ = **190** viewers (below the 200 configured), screen streams ⌊400 / 3.2⌋ = **125**.
  Large calls and other streams reduce it further; a new stream must leave room for 10 viewers.
  Beyond the budget, starting or watching answers `503 stream_capacity` ("Live streams are very busy
  right now. Please try again in a few minutes.").
- Raising the budget toward the measured upload needs a watched real stream first (coturn shares the
  host). The first lever after that is 720p → lower top layer for screens.

## 6. Clients

Web (`site-inbox/live.js`, the call client's stream mode; `groups.js` for the banner, the Go live
sheet, the `stream` notice and the "Who can go live" setting). Android (0.4.1) has the native banner,
Watch, Go live, the policy setting and stage forwarding (`apps/android/README.md`). Native apps reuse the same page in
their call web view: `PigeonpostNative.watchStream({id, name}, streamId)` and
`PigeonpostNative.goLive({id, name}, {title, camera, screen})` return `{ok, code?}`; the page asks for
full screen while the stream is shown. The native group screens still need a LIVE banner and Watch /
Go live buttons calling these (a native-client follow-up).

Browser and app engines, watching a stream with synthetic media (2026-10-27, `tests/e2e-streams`):

| Engine | Result |
| --- | --- |
| Chrome 156 (macOS) and 155 (Linux), headless | Pass: go live, watch, stage, reactions, removal and rotation, capacity refusal (`e2e.mjs`, 18/18); 50 viewers on the production media server. |
| iOS 18.3 Simulator, Safari (WebKit) | Pass: key from the streamer, encrypted camera decoded, 0 decryption errors. |
| Android 14 emulator, Chrome 113 (the same engine as the Android app's WebView) | Pass, same checks. |
| macOS 14, a bare `WKWebView` (the Mac app's engine) | Pass, same checks. |

Not run here: the iOS app's own WKWebView (Safari in the Simulator is the same WebKit; the
CryptoKey-into-worker problem it had is handled by `calls-e2ee-worker.js` for every WebKit), Windows
WebView2 (Chromium), Linux WebKitGTK (no WebRTC, see the calls notes).

## 7. Operations

Deploy order: postbox (flag off), calls service, inbox files; then turn on `streaming` (globally or
per account) after a live check. Rollback: turn the flag off (the routes answer `feature_disabled` and
the inbox hides streams); the calls service's stream routes are inert without it. Health:
`GET /calls/v1/health` → `streams {live, viewers, load_mbps, …}`.
