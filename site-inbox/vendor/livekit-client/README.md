# livekit-client 2.22.4 (vendored)

From the npm package `livekit-client@2.22.4` (Apache-2.0, see `LICENSE`), published 2026-10-09.
npm integrity `sha512-UJ5K8gqP8uIO+tjhGeF+s6SzgLfxZKPxQjSqzzkF298QNYMnhYmXGQRMMh3GnsfaeswnsHHMSogMeb7SocH2zw==`
(the tarball was checked against it). Only two files from `dist/` are kept, unmodified:

| File | SHA-384 (as used for subresource integrity) |
| --- | --- |
| `livekit-client.umd.js` | `sha384-5qI/UmLG0CvuWCt6vqjMW1oRwzXu+MNhbw4x6yo9NARdlqi94LwQfq9wvDTIcfrD` |
| `livekit-client.e2ee.worker.js` | `sha384-+kzwHxsNneGJefe7Uqz/ay3Qkp+MmCJ/jF37uSeIHfXyGoGCsE2aV11VPzn61fJ2` |

`site-inbox/test/calls-e2ee.test.mjs` fails if either file stops matching, and `calls.js` loads the
client with that integrity value. Their source maps are not shipped (the trailing
`sourceMappingURL` comments are left in place, so the files stay byte-identical to the package).

Used by `calls.js` only for calls of more than six people, which run through the media server
(SFU): `calls.js` loads these files on demand when a call becomes large, never otherwise. The
worker performs the per-frame AES-GCM encryption and decryption; the room key it uses is
distributed by `calls-e2ee.js`, never by the SDK or the server. Allowed as a pinned, hash-checked
exception to the inbox's no-dependency rule by SDS decision D-24.

The UMD bundle includes code from these packages, all under permissive licences: `@livekit/protocol`,
`@livekit/mutex` and `@bufbuild/protobuf` (Apache-2.0), `webrtc-adapter` (BSD-3-Clause), `events`,
`jose`, `loglevel`, `machina`, `sdp-transform` and `typed-emitter` (MIT), `tslib` (0BSD).

To update: download the new version's tarball, check it against `npm view livekit-client@<v>
dist.integrity`, copy the two files, update the table above and `LIVEKIT_INTEGRITY` in `calls.js`, and
run the large-call tests in `deploy/calls/README.md` against every supported browser and web view.
