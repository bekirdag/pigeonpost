# iPhone CI runtime progress

- Run 40944173080 / job 123695034645 was cancelled at the 75-minute limit. The preserved log shows passing tests until cancellation during IOSUXTests; iPad/Mac/StoreKit are green. This is not a pass for unexecuted tests.
- Partitioned all eight existing iPhone UI classes exactly once across three suites, retained native-ios aggregate, failure propagation and per-suite artifacts. Full model suite runs in messaging only. Manual calendar-file diagnostics select the calendar suite.
- YAML parse, bash syntax, unchanged selection union and git diff checks pass. Exact-root Docdex symbol/impact calls returned index-writer backoff; reviewed workflow dependencies directly. No application source or assertion changes.
- Fresh exact-head CI is required before merge.

- Combining contacts with the first-message UI test exposed a real discard bug: after removing the only unconfirmed message, the selected thread became Pick a conversation. Kept an empty composer only for the currently read, locally started conversation, without creating a history row; it retires when the user leaves or changes inbox. Updated core regression passes (180 checks); the existing UI assertion remains unchanged and is being requalified.

- Native snapshot confirmed the removed message remained in the UITableView accessibility tree although the visible row was gone. Clear removed hosting configurations before reload. The unchanged first-send and discard UI regressions now both pass in the combined contact build; core checks are 180/180. Fresh PR CI still gates merge.
