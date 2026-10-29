# iPhone CI runtime correction

The 75-minute iPhone job on PR #180 exhausted its limit after passing the tests it reached; iPad/Mac/StoreKit passed. Split the existing iPhone test classes into messaging, calendar and account suites on independent runners. Preserve all classes exactly once, keep the `native-ios` aggregate required result, preserve per-suite evidence, and run model tests once. Do not change assertions, device fixtures or store submissions. Validate YAML, shell syntax and the union of selections before pushing.
