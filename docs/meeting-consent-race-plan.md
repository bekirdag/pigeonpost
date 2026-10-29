# Meeting consent ordering

Review the delayed consent paths in calls-service/src/summaries.js. Reproduce withdrawal latency and an earlier affirmative response overwriting a later refusal, then pause capture before any network wait and order durable updates per participant. Validate requester presence and the current consent revision after awaits. Keep chunk encryption and normal call signaling unchanged.

Run the full calls-service and meeting-intel suites, qualify CI, then deploy only when there are no live calls or notes jobs. Back up the running source and verify health/source hashes afterward. Keep physical-device checks distinct from synthetic coverage.
