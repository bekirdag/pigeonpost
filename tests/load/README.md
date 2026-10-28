# Load tests (Phase 9)

Disposable, loopback-only load harness for the postbox and the calls service. Every run starts its
own postbox on a fresh temporary SQLite database (WAL, `synchronous=FULL`, as in production), a
synthetic OIDC issuer (an RSA key made for the run) and synthetic accounts. **Never point it at a
deployed service.** Results of the 2026-10-28 run: `docs/ops/2026-10-28-load-tests.md`.

No dependencies beyond Node ≥ 22 (it uses `node:sqlite` to seed history) and a release postbox.

```sh
cargo build --release -p pigeonpost-postbox

# Postbox: setup, send ramp, inbox, 2,000 event streams + 10× mixed load, org API, unauthenticated flood
node tests/load/postbox.mjs --bin target/release/pigeonpost-postbox --out /tmp/postbox.json
node tests/load/postbox.mjs --bin … --quick                 # ~1.5 min smoke version
node tests/load/postbox.mjs --bin … --only org,flood        # re-measure the org API limiter / flood
node tests/load/postbox.mjs --bin … --seed-rows 80000       # history size (mailbox queries scale with it)
node tests/load/postbox.mjs --bin … --mailbox-index         # experiment: add the COALESCE(owner, recipient) index first

# calls-service: idle sessions, call setup ramp, held rooms, SIGTERM + reconnect storm
node tests/load/calls.mjs --bin target/release/pigeonpost-postbox --rtt-ms 40 --out /tmp/calls.json
node tests/load/calls.mjs --bin … --restart-only

# Group fan-out with real MLS clients (in-process postbox, in-memory SQLite)
GROUP_LOAD_SIZES=50,200,1000 cargo test --release -p pigeonpost-postbox group_fanout_load -- --ignored --nocapture
```

Options of `postbox.mjs`: `--accounts` (500), `--streams-per-account` (4), `--seed-rows` (300,000;
20,000 with `--quick`), `--send-rps` (25) and `--poll-rps` (330) for the mixed run, `--only
ramp,inbox,events,org,flood`. Options of `calls.mjs`: `--pairs` (600), `--idle-sessions` (3,000),
`--rtt-ms` (40, the delay a proxy adds on the calls → postbox internal API path, standing in for the
SSH tunnel), `--quick`. Set `LOAD_KEEP=1` to keep the temporary directory.

## Load model

| | Launch (1×) | Test target (10×) |
|---|---|---|
| Active accounts | 1,000 | — (500 synthetic accounts carry the rate) |
| Messages | 20,000/day; busy-minute peak ≈ 2.5 sends/s | 25 sends/s |
| Inbox polls | 1,000 agents every 30 s ≈ 33/s | 330/s |
| `/v1/events` streams | ≈ 2 per account | 2,000 open |
| Calls | ≈ 0.2 setups/s, ≈ 20 concurrent | 2 setups/s; ramp to 40/s |
| Org API | a few keys well under 600/min | 20 keys at 540/min each (180 req/s) + 20 keys bursting past 600/min |
| Groups | up to the 1,000-member cap | 50 / 200 / 1,000 members |

Production on 2026-10-28: ~29 accounts, ~200 messages/day, ~5,900 message rows.

## What each number means

- **ramp / inbox**: closed loop (N workers back to back): throughput and latency per concurrency.
  `inbox` lists with `include_read=true`, the heaviest listing.
- **events.mixed**: open loop (fixed arrival rate, the honest way to see queueing) of sends and
  unread-only polls while every stream is open. Before it, all mail is marked read (clients that
  poll acknowledge). **Delivery** = send start → the `mail` frame with that message id; `first` on
  any of the recipient's streams, `all` on every one of them.
- **org.burst**: 700 requests per key as fast as 8 workers go; exactly 600 must pass.
- **flood**: 256 workers sending junk (bad keys, bad JWTs, posts without proof of work) from the
  same address as a member who sends at the target rate.
- Latencies are milliseconds; CPU % is of one core (`ps` CPU time over the phase); RSS in MB.
- The load generator runs on the same machine as the server, so absolute numbers are a lower bound
  of what the server alone could do; ratios between runs are what to compare.
