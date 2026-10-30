# Meeting audio lifetime progress

- Isolated from current main 159a47ef. No changes to production processor yet.
- Source review identified decode/discard generation race and transcription-error PCM cleanup. Baseline regressions and focused impact review in progress.

- Both baseline regressions fail: a discarded in-flight decode is accepted, and the PCM copy passed to failing STT remains nonzero. Capturing generation before decode and checking it on return prevents the former; finally cleanup covers the latter.
- Exact-root Docdex symbols/impact returned index-writer backoff. Direct service import/call-site inspection used as fallback; the calls summary suite also exercises the processor API.

- Decoder review also reproduced retained overflow/late pipe buffers after termination (2 additional failing regressions with injected child process). Decoder now wipes those buffers and ignores late close events; the real subprocess remains the default dependency. Cleanup is for owned mutable buffers; this does not claim secure erasure of immutable/runtime-managed copies.

## Qualification

- Before fixes: both processor race/lifetime regressions failed and both decoder overflow/timeout cleanup regressions failed.
- After fixes: full meeting-intel plus calls-service suites pass **95/95**, no skips. A real local ffmpeg synthetic-WAV pipe round-trip passes.
- No real microphone input, messages, credentials or production test sessions were used. Production deployment awaits CI and idle-service checks.
