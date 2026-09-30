These three wire fixtures are copied from `apps/ios/Tests/main.swift`, which also identifies the
web inbox tests as their source. The interpolation variable `now` is fixed to Unix timestamp
1789113600. JSON escaping was decoded and formatting expanded; the protocol fields and relative
timestamps were preserved. They contain sample data, not credentials.

Use these to check cross-client conversation behavior. When the protocol changes, compare the
Apple and web fixtures before updating the Windows copy.
