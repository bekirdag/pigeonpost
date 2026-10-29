# Meeting consent ordering — evidence

- Baseline 78a8118e: both new delayed-response regressions fail; capture remains active during withdrawal and an older yes overwrites a newer no.
- Withdrawal now pauses/publishes/discards immediately. Per-participant writes preserve durable order, and affirmative responses must still match the current participant and decision. A third regression checks departure during the write.
- Full calls-service + meeting-intel qualification: 91 tests pass, none skipped. All traffic is local and synthetic; no production call or microphone was used.
- Docdex impact links the changed module to server.js, the summary integration suite and the broader calls tests. The compact-storage rollback documentation also now requires zero undecodable/remaining binary records; zero conversions alone was insufficient.
- CI and safe production deployment remain pending.
