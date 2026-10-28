# Load tests — postbox and calls service (Phase 9, 2026-10-28)

Synthetic accounts on disposable local instances only. Nothing touched production. Harness and how
to re-run it: `tests/load/README.md`.

## Setup

- **Machine:** MacBook Pro (MacBookPro18,2, Apple M1 Max, 10 cores, 64 GB), macOS 14.7.4, APFS SSD.
  The load generator (Node 24.19) ran on the same machine as the server, so the absolute numbers
  are a lower bound. Production (45.132.181.100, Linux, Docker) has a different CPU and disk; the
  ratios between runs are what transfer.
- **Builds:** postbox 0.7.18 `--release`, rustc 1.91.1:
  - **main** = `59f3e0f`;
  - **#152** = PR #152 head `0c8c4d6`, which adds the mailbox index and a credential throttle that
    never blocks valid credentials.

  calls-service from `59f3e0f` (Node, no changes).
- **Database:** a fresh SQLite file per run with WAL and `synchronous=FULL` (production settings).
  History is seeded by copying real sealed rows, marked read, to a chosen row count:
  - 6,000 rows ≈ production today (~5,900);
  - 300,000 rows ≈ one week of the 10× model.
- **calls → postbox internal API:** through a proxy adding 40 ms round trip, standing in for the SSH
  tunnel from raijin to the postbox host.

## Load model (assumption)

Launch is 1,000 active accounts (~35× today's 29). Each sends 20 messages/day, so 20k messages/day
and ~40k stored rows/day (sender and recipient copies).

| | Busy-minute peak at launch (1×) | Tested (10×) |
|---|---|---|
| Sends | ≈ 2.5/s | 25/s |
| Polls (agents polling every 30 s) | ≈ 33/s | 330/s |
| `/v1/events` streams | ≈ 2,000 | 2,000 |
| Call setups | ≈ 0.2/s | 2/s, ramped to 40/s |
| Org API | 20 keys, 540/min each (180/s), plus 20 keys bursting past the limit | |
| Group fan-out | | 50, 200 and 1,000 members (the cap) |

## Findings

| Sev | Finding | Status |
|---|---|---|
| **High** | Every mailbox query filtered on `COALESCE(owner, recipient) = ?` (16 sites in `store.rs`: inbox listing, quota sum, …), and no index matched it. `EXPLAIN` showed `SCAN messages` plus a temp B-tree, so each send and each poll read the whole table, blobs included. Throughput was inversely proportional to table size, and 10× load failed at today's table size. | **Fixed in #152** (`messages_by_mailbox ON messages(COALESCE(owner, recipient), created_at)`); measured below. |
| Medium | Each poll loads the mailbox's **entire history**, every row with its blob (`list_for`), and filters read/sent/thread in Rust. With the index, the poll knee is 100–200/s at 300k rows (about 600 rows per mailbox), so 330 polls/s still queues. | Follow-up (not in Phase 9): push `read`/`direction`/`thread_id` filters and a `LIMIT` into SQL; paginate `include_read`. |
| Medium | No request timeout, concurrency limit or load shedding on the postbox router. Under overload, abandoned requests keep running and each holds a full mailbox history in memory. **RSS reached 5.6 GB** in the overloaded 10× run. nginx gives up at 60 s; the postbox does not. | Follow-up: a tower `ConcurrencyLimit`/`LoadShed` + `TimeoutLayer` on the public router, and the `list_for` fix above. |
| Medium | Stored size is large: a 20-byte body is 2.75 KB per copy, a 300-byte body 3.8 KB, and a 2,000-byte body 10.9 KB (≈ 5.4 bytes per body byte, ~2.7 KB fixed). The ciphertext fields are hex, 2 bytes per byte; the ratio fits a hex-encoded sealed envelope that is wrapped and hex-encoded again (≈ 4×) plus fixed fields. The format was not traced further. A sent message costs both copies (5.5–22 KB); SDS §22.4 assumed 2 KB. | Follow-up: confirm the layering, then a more compact storage encoding (raw bytes in the BLOB column, or one encoding layer), with a migration. |
| Medium | PR #152's first head (`8d150ee`): the new per-address throttle on refused credentials also refused **valid** credentials. Under the flood, a real member's sends from the same address went 376/376 → 429. | **Fixed in #152** `0c8c4d6` (verified below: 377/377 sent). |
| Low | calls-service sends call-history events to the postbox one at a time, so the queue drains at ~1/RTT (~21 events/s at 40 ms). At 40 calls/s the queue reached 972 of its 1,000 cap (46 s to drain); beyond that, events are dropped. 10× launch (2 calls/s) never queues. | Follow-up: send events in batches or several at a time. |
| Low | Ended rooms stay in `rooms` for 60 s and count toward `roomsTotal` (2,000), so more than ~33 call setups/s sustained would answer `503 busy`. | Follow-up: count only live rooms. |
| Info | An unbounded reconnect burst of 1,200 clients mostly fails **on macOS** because the listen backlog is capped at 128 (`kern.ipc.somaxconn`); it is a harness artifact. With 100 in flight, all reconnect in 0.5 s. In production nginx fronts the service and Linux's backlog is larger. | none |
| Info | One org API sustained step on main at 300k rows showed p99 3.2 s, with p50 0.9 ms. It came right after the seeding write of 1.8 GB, most likely a WAL checkpoint stalling the single connection. It did not repeat at 6k rows or on #152 (p99 9.5 ms at 300k). | Watch the WAL size after bulk writes. |

## Postbox

### Send throughput vs table size (closed loop, 120 accounts)

`ok/s` and p99 at 8 concurrent senders. Inbox = `GET /v1/inbox?include_read=true` (the full
listing), 8 concurrent.

| Message rows (DB size) | main sends/s | main send p99 | indexed sends/s | indexed send p99 | main inbox/s | indexed inbox/s |
|---|---|---|---|---|---|---|
| 6,000 (37 MB) | 88 | 173 ms | 748 | 26 ms | 96 | 157 |
| 20,000 (120 MB) | 41 | 289 ms | 395 | 57 ms | 34 | 124 |
| 80,000 (472 MB) | **11.7** | 974 ms | 211 (#152: **369**) | 103 ms (#152: 47 ms) | 9.7 | 37 (#152: 75) |

On main, throughput fell from ~95/s to ~11/s between today's table size and 80k rows. At launch
volume (~40k rows/day) the 1× load would saturate within days.

### Full runs (500 accounts)

**Send ramp (closed loop)**

| Concurrency | main @ 6k rows: ok/s | p50 / p99 ms | #152 @ 300k rows: ok/s | p50 / p99 ms | #152 CPU |
|---|---|---|---|---|---|
| 1 | 70.6 | 12 / 32 | 254.6 | 3.4 / 19.7 | 89 % |
| 4 | 74.8 | 52 / 89 | 343.1 | 9.6 / 28.8 | 130 % |
| 16 | 68.4 | 224 / 397 | 342.4 | 46 / 71 | 132 % |
| 64 | 64.0 | 958 / 1,568 | 341.1 | 187 / 257 | 132 % |
| 256 | 47.8 | 5,064 / 7,348 | 336.3 | 743 / 1,019 | 133 % |

Main's send knee is reached already at one or two workers. The #152 knee is ~340 sends/s: the
single SQLite connection behind a mutex saturates at ~1.3 cores.

**Inbox, full listing (`include_read=true`)**

| Concurrency | main @ 6k: ok/s, p99 | #152 @ 300k (~660 rows per mailbox): ok/s, p99 |
|---|---|---|
| 4 | 52.5, 130 ms | 45.9, 119 ms |
| 16 | 60.5, 433 ms | 94.3, 271 ms |
| 64 | 60.0, 1,769 ms | 98.1, 944 ms |
| 256 | 58.4, 7,718 ms | 96.1, 3,478 ms |

**Event streams and mixed load**

2,000 streams were open (4 per account, 0 refused, all opened in 0.3 s). The loads below are
open-loop; delivery = send start → `mail` frame. Every message reached every stream; 0 were lost.

| Run | Sends | Polls | Delivery, first stream p50 / p99 | Delivery, all streams p99 | CPU | Peak RSS |
|---|---|---|---|---|---|---|
| main @ 6k, **1×** (2.5 sends/s + 33 polls/s) | 2.5/s, p99 193 ms | 33/s, p99 52 ms | 12 / 193 ms | 194 ms | 28 % | 89 MB |
| main @ 6k, **10×** (25 + 330) | 21/s, p99 **25.7 s** | 101/s, p99 **28.8 s** | 20.4 / 27.4 s | 28.5 s | 108 % | 354 MB |
| #152 @ 300k, 10× (25 + 330) | 23.8/s, p99 8.7 s | 276/s, p99 9.6 s | 7.3 / 9.2 s | 9.7 s | 135 % | **5.6 GB** |
| #152 @ 300k, 25 sends + **100** polls/s | 25/s, p99 124 ms | 100/s, p99 130 ms | 6.8 / 125 ms | 131 ms | 51 % | 192 MB |
| main + index @ 300k, 25 + 200 polls/s | 25/s, p99 419 ms | 200/s, p99 420 ms | 6.5 / 449 ms | 473 ms | 84 % | 499 MB |

So with the index, 10× sends and streams are comfortable, and polls are the limit (knee 100–200/s
at this history size) until `list_for` stops loading whole histories.

**Send-rate ramp with the 2,000 streams open (no polls)**

| Offered sends/s | main @ 6k: ok/s | main delivery p99 | #152 @ 300k: ok/s | #152 send p99 | #152 delivery p99 (first / all) | #152 CPU |
|---|---|---|---|---|---|---|
| 50 | 50 | 42 ms | 50 | 25 ms | 25 / 25 ms | 27 % |
| 100 | 58.9 | 9.8 s | 100 | 23 ms | 24 / 24 ms | 47 % |
| 200 | 41.7 | 31 s | 200 | 30 ms | 31 / 32 ms | 87 % |
| 400 | 14.3 | 35 s | 277 | 5.3 s | 5.6 / 5.8 s | 118 % |
| 800 | 10.9 | 34 s | 276 | 7.7 s | 8.2 / 8.5 s | 116 % |

The knee with #152 is 200–280 sends/s with every stream open, which is 80–110× the launch peak.
Fan-out to an account's 4 streams adds < 2 ms.

### Group fan-out (real MLS clients, in-process postbox, in-memory SQLite)

60 posts by the owner and up to nine members who joined in the final commit. Then every member
fetches the new page, 64 at a time.

| Members | Commits to add everyone (≤ 200 key packages per claim) | Setup | Owner post p50 / max | All posts p50 / p99 (incl. client sync) | Fan-out read: total for all members | Read p50 / p99 |
|---|---|---|---|---|---|---|
| 50 | 1 | 1.4 s | 2.6 / 18.5 ms | 7.1 / 34.7 ms | 0.01 s | 10.7 / 13.4 ms |
| 200 | 1 | 10.1 s | 5.9 / 17.7 ms | 18.6 / 58.7 ms | 0.05 s | 14.1 / 26.3 ms |
| 1,000 | 5 | 179 s | 31.4 / 45.0 ms | 88.3 / 246 ms | 1.35 s | 79.6 / 194 ms |

Server cost per post grows with membership (~31 ms at 1,000 members), so one 1,000-member group
can take ~30 posts/s on the shared connection. Every post was decrypted by a member from the final
commit. The in-memory database flatters disk I/O, so treat these as upper bounds.

### Org API (20 keys sustained, 20 keys bursting)

| Build / rows | Sustained 180/s (9/s per key): p50 / p99 | Codes | Burst 700/key: rps, p99 | Passed per key |
|---|---|---|---|---|
| main @ 6k | 0.8 / 4.6 ms | 5,402 × 200 | 6,068/s, 44.5 ms | exactly 600 (min = max) |
| #152 `8d150ee` @ 6k | 0.8 / 4.5 ms | 5,402 × 200 | 5,728/s, 78.6 ms | exactly 600 |
| #152 `0c8c4d6` @ 6k | 0.8 / 5.2 ms | 5,402 × 200 | 6,063/s, 44.2 ms | exactly 600 |
| #152 `0c8c4d6` @ 300k | 0.8 / 9.5 ms | 5,401 × 200 | 5,760/s, 104 ms | exactly 600 |

#152's restart-safe limiter (an in-memory hot path plus a snapshot to SQLite every 10 s) costs
nothing measurable, and the limit is exact.

### Unauthenticated flood (256 workers of junk + one member sending 25/s from the same address)

| Build | Junk rps | Junk codes | Member sends | Member p50 / p99 | CPU |
|---|---|---|---|---|---|
| main @ 6k | 29,305 | 200 (`/v1/pow/challenge`), 400 (no proof of work), 401 (bad keys, unthrottled), 429 (bad org keys, per-address) | 377 × 201 | 27.6 / 38.8 ms | 239 % |
| #152 `8d150ee` @ 6k | 30,287 | 401 × 100, then 429 | **376 × 429** (valid key refused) | — | — |
| #152 `0c8c4d6` @ 6k | 29,447 | 401 × 100, then 429 | 377 × 201 | 17.0 / 28.9 ms | 201 % |
| #152 `0c8c4d6` @ 300k | 27,279 | as above | 377 × 201 | 18.2 / 36.8 ms | — |

The flood does not hurt members; refusing cheaply (#152) also lowers their latency.
`/v1/pow/challenge` answers at full speed (stateless).

## calls-service (real service, real local postbox, 40 ms internal RTT)

**Idle signaling sessions:** 3,000 sessions opened at 4,637/s (open p50 9.5 ms, p99 52 ms, 0
failed). Each costs ≈ 52 KB of RSS (59 → 211 MB).

**Call setup ramp:** 1:1 audio calls in this order:

1. start;
2. ring at the callee;
3. accept;
4. joined at the caller;
5. 10 signals each way (offer/answer, then ICE);
6. leave.

| Calls/s | Calls | Failed | Ring p50 / p99 | Setup p50 / p99 | Signal relay p50 / p99 | Events queued at end | Drain | Dropped | calls CPU | RSS |
|---|---|---|---|---|---|---|---|---|---|---|
| 2 | 20 | 0 | 48 / 54 ms | 49 / 55 ms | 0.3 / 0.9 ms | 0 | 0 s | 0 | 2 % | 241 MB |
| 5 | 50 | 0 | 48 / 484 ms | 49 / 485 ms | 0.2 / 1.3 ms | 0 | 0 s | 0 | 4 % | 269 MB |
| 10 | 100 | 0 | 47 / 237 ms | 48 / 238 ms | 0.2 / 0.7 ms | 87 | 4.3 s | 0 | 6 % | 276 MB |
| 20 | 200 | 0 | 46 / 54 ms | 46 / 56 ms | 0.2 / 0.6 ms | 377 | 17.7 s | 0 | 8 % | 276 MB |
| 40 | 400 | 0 | 46 / 52 ms | 50 / 58 ms | 0.2 / 0.5 ms | **972** | 45.6 s | 0 | 16 % | 279 MB |

Setup time is the admission round trip (40 ms) plus a few ms. The service itself is nowhere near
a limit at 40 setups/s (16 % of a core). The call-history queue is the first limit.

**Held rooms:** 100 and 300 concurrent active calls with 1,200 sessions open: RSS flat at
280 MB (rooms are a few hundred bytes each). All completed.

**Restart (SIGTERM with 1,200 sessions, 50 live calls):**

- the service exits in 53–84 ms;
- in-memory rooms, rings, knocks and the queued call events (252 in the full run) are lost;
  #155 adds an optional on-disk spool for the events;
- 1,200 clients reconnect in 0.5 s (100 in flight, p99 75 ms, 0 failed), including 2,400 postbox
  authentication lookups.

## Recommendations

1. Merge #152 (mailbox index; valid credentials never throttled) before launch. It is the
   difference between ~11 and ~370 sends/s at 80k rows.
2. Next: make `list_for` load only what the request asked for (unread, a `LIMIT`, pagination), and
   add a concurrency limit / load shedding / timeout layer, so overload degrades instead of growing
   memory without bound.
3. Shrink the stored envelope (≈ 5.4× body size plus ~2.7 KB today) before message volume grows.
4. calls-service:
   - batch call-history events (drain rate);
   - count only live rooms toward `roomsTotal`.

   Neither matters at launch scale.
5. Re-run `tests/load/postbox.mjs --only events,org,flood --seed-rows 300000` after each of these, and
   on the production host class before raising any advertised limit.

## Phase 9b re-run (2026-10-28, after the medium findings)

Same machine, same harness, 500 accounts, 300k seeded rows, both binaries built and run back to
back: **main** = `343eefb` (Phase 9 as deployed), **9b** = the Phase 9b branch (bounded listings,
change-based long polls, admission control; storage encoding measured separately below). New
scenarios: `apps` (one device per account on the apps' history long poll
`include_sent&include_read&wait=25`, back to back, every mailbox holding unread mail, plus sends)
and `overload` (history listings offered at 400/s, 4× the knee, plus 25 sends/s, open loop).

| Run | main | 9b |
|---|---|---|
| **10× mixed** (25 sends/s + 330 unread polls/s + 2,000 streams): poll p50 / p99 | 158 / 706 ms | **1.3 / 13 ms** |
| same: send p99, delivery p99, CPU, peak RSS | 652 ms, 671 ms, 137 %, 801 MB | **23 ms, 23 ms, 51 %, 203 MB** |
| Send ramp with streams, 400 / 800 offered/s: ok/s, delivery p99 | 305 / 304, 4.1 / 7.7 s | 245 / 241, 1.7 / 1.8 s (excess shed with 503) |
| **apps, 1×** (2.5 sends/s): history listings/s, send p99, CPU, RSS | 67.6/s (tight loop), 9.4 s, 901 %, 2.0 GB | 43.5/s, 605 ms, 357 %, 280 MB |
| apps, 10× (25 sends/s): listings/s, send p99, CPU, RSS | 45.9/s, 13.4 s, 750 %, 2.4 GB | 57.3/s, 854 ms, 826 %, 315 MB |
| **overload** (400 history listings/s + 25 sends/s, 60 s): listings answered, p50 | 48.8/s, **229 s** (all queued) | 61.7/s, 1.1 s; 20,191 refused 503 |
| same: sends ok, peak RSS | 61 of 1,500, **15.1 GB** | 733 of 1,500 (768 refused 503), **284 MB** |
| Full listing closed loop (`include_read`, c = 256) | 107/s, p99 3.2 s, 860 MB | 99/s, p99 3.3 s, 198 MB |

What the numbers say:

- Unread polls (agents, CLI, MCP) no longer read history: at 10× they cost 13 ms p99 and the
  postbox idles at half a core.
- Memory is bounded under any arrival rate: 15.1 GB → 284 MB at 4× the listing knee. Excess work
  is refused with `503` + `Retry-After` instead of queued; what is accepted is answered in about a
  second. (A listing that waits over 1 s for one of the 32 listing slots is refused, so listings
  cannot crowd sends out of the ordinary slots: sends served under overload went 61 → 733.)
- The apps' tight loop is gone, but full-history apps are still the most expensive client. With
  500 devices each parked on `wait=25`, every timeout returns a full ~600-row listing (500 / 25 s
  = 20 listings/s before any change), and each change re-lists the history of both mailboxes; a
  600-row listing costs ~80 ms of CPU (opening each envelope). The fix is client-side: the apps
  moving to `since` + `after` deltas (`docs/inbox-paging.md`), after a `message.deleted` event
  exists. Until then the full-listing knee (~100/s on this machine) is the capacity limit for
  app-heavy load; at launch scale (1,000 accounts) it is not reached.
- The full-listing knee itself is unchanged (~100/s): those listings return the same rows.

**Storage** (`docs/ops/message-storage-encoding.md`): stored bytes per send (two copies plus
metadata), 20 / 300 / 2,000-byte bodies: 13,028 / 15,102 / 29,439 → **2,430 / 3,454 / 10,622**
(5.4× → 4.4×, 3.9×, 2.8× smaller); 5,884 / 6,908 / 14,076 with a reply quote. The remaining
~1 KB floor is the hex and padding inside envelope v3 (protocol). SDS §22.4 assumed 2 KB per
message; now ~1.2–1.7 KB per copy for short messages.
