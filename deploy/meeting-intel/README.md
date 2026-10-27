# meeting-intel and whisper on raijin

Meeting notes (docs/meeting-summaries.md). Two systemd units on raijin, both loopback-only:

| Unit | Listens | Runs |
|---|---|---|
| `pigeonpost-whisper` | 127.0.0.1:8798 | whisper.cpp server, CPU, 6 threads, `CPUQuota=600%`, `MemoryMax=3G` |
| `pigeonpost-meeting-intel` | 127.0.0.1:8797 | `meeting-intel/` (Node, no dependencies), `MemoryMax=1536M`, no writable paths |

Both use `DynamicUser=yes` and `IPAddressAllow=localhost`. meeting-intel talks to whisper, the local
model on 127.0.0.1:11452, and the postbox internal API through the existing tunnel (127.0.0.1:18991).

## Pinned versions

| Item | Version / source | SHA-256 |
|---|---|---|
| whisper.cpp | tag `v1.9.5` (commit `d1be6fde11ac6e0407606b4e42fe72d34add8037`, MIT) | built from source |
| `ggml-small.bin` (multilingual) | huggingface `ggerganov/whisper.cpp` | `1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b` |
| `ggml-silero-v6.2.0.bin` (VAD) | huggingface `ggml-org/whisper-vad` | `2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987` |
| Summary model | `qwen-latest-flash-next` on the box's local OpenAI-compatible server | shared, not managed here |

## Before any change: the box is shared

raijin runs many other workloads (load average ~45-55 on 48 threads, swap full, ~30 GB available).
Check `free -g`, `cat /proc/pressure/cpu` and `vmstat 2 3` first, build with `nice` and at most `-j 8`,
and never stop or reconfigure another service. Never `pkill` by name: stop things by unit or PID.

## Install or upgrade whisper

```sh
# as wodo
git clone --depth 1 --branch v1.9.5 https://github.com/ggml-org/whisper.cpp ~/build/whisper.cpp-v1.9.5
cd ~/build/whisper.cpp-v1.9.5
nice -n 15 cmake -B build -DCMAKE_BUILD_TYPE=Release -DGGML_CUDA=OFF -DBUILD_SHARED_LIBS=OFF -DGGML_NATIVE=ON
nice -n 15 cmake --build build -j 8 --config Release
sh models/download-ggml-model.sh small && sh models/download-vad-model.sh silero-v6.2.0
sha256sum models/ggml-small.bin models/ggml-silero-v6.2.0.bin   # compare with the table above
# as root (one sudo -S)
install -d /opt/pigeonpost-whisper/bin /opt/pigeonpost-whisper/models /opt/pigeonpost-whisper/public
install -m 0755 build/bin/whisper-server /opt/pigeonpost-whisper/bin/
install -m 0644 models/ggml-small.bin models/ggml-silero-v6.2.0.bin /opt/pigeonpost-whisper/models/
install -m 0644 deploy/meeting-intel/pigeonpost-whisper.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now pigeonpost-whisper
```

## Install or upgrade meeting-intel

Secrets are generated on the host and never leave it:

- `/etc/pigeonpost-meeting-intel/signing.pem` — P-256 signing key (root 0600). Generate once with
  `openssl ecparam -name prime256v1 -genkey -noout | openssl pkcs8 -topk8 -nocrypt`. Its public key
  (`openssl ec -in signing.pem -pubout -conv_form uncompressed -outform DER | tail -c 65 | base64 -w0`)
  must equal `PINNED_KEYS` in `site-inbox/calls-summary.js`. Rotating it means shipping the new public
  key in the inbox first (keep both pinned for a release), then swapping the file.
- `/etc/pigeonpost-meeting-intel/meeting-intel.env` (root 0600, from `meeting-intel.env.example`):
  `INTEL_SERVICE_TOKEN` (random, also `CALLS_INTEL_TOKEN` in `/etc/pigeonpost-calls/calls.env`) and
  `POSTBOX_SERVICE_TOKEN` (also `POSTBOX_SVC_TOKEN_MEETING_INTEL` in the postbox's `postbox.env`).

```sh
# as root, from a checkout of the merged commit
rsync -a --delete meeting-intel/src meeting-intel/package.json /opt/pigeonpost-meeting-intel/
install -m 0644 deploy/meeting-intel/pigeonpost-meeting-intel.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now pigeonpost-meeting-intel
curl -s 127.0.0.1:8797/v1/health   # key_id, queue, rtf; no content
```

The calls service needs `CALLS_INTEL_URL=http://127.0.0.1:8797` and `CALLS_INTEL_TOKEN`, then a restart.

## Qualification on the box

`tests/e2e-meeting-summaries/make-fixture.sh` (macOS: synthetic voices only) makes the three-speaker
EN/TR fixture. Copy it and `meeting-intel/` + `tests/e2e-meeting-summaries/stt-check.mjs` to raijin and
run against a whisper server (the production one, or a temporary one on another loopback port):

```sh
WHISPER_URL=http://127.0.0.1:8798/inference node tests/e2e-meeting-summaries/stt-check.mjs <fixture-dir>
```

It prints the real-time factor and the brief, and fails unless every speaker's key phrases (English and
Turkish) are transcribed, owners are only people who took tasks, the brief decrypts for each recipient key
and all decoded audio was wiped.

## Turning the feature on and off

`meeting_summaries` is a postbox flag (`PUT /internal/v1/features/meeting_summaries` with the ops token).
Off: hosts are no longer offered notes and requests are refused; existing notes stay readable until they
expire. Emergency stop without the postbox: `systemctl stop pigeonpost-meeting-intel` — requests then fail
with `summary_unavailable` and running sessions (with their in-memory audio) are gone.

## Rollback

`systemctl disable --now pigeonpost-meeting-intel pigeonpost-whisper`, remove the two `CALLS_INTEL_*`
lines and restart `pigeonpost-calls`. Nothing on disk holds meeting audio or text.
