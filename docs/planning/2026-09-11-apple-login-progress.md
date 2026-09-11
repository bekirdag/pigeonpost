# Apple login progress

## Inspection
- Both apps use Shared/Auth/Session.swift with ASWebAuthenticationSession and S256 PKCE to the pigeonpost-prod realm; native client pigeonpost-mobile.
- App bundle dev.pigeonpost.inbox, team AH277897AV. User completed the Services ID dev.pigeonpost.signin and supplied the Apple key outside the repository.
- Existing iOS workflow includes processing and beta-group assignment. Latest successful upload: 1.0 (29), workflow run 33196277546.
- Local Xcode 16.2 supports local validation; store uploads use the existing macOS 26 CI runner.
- Pigeonpost working tree began clean. su_iam has unrelated untracked .claude/ configuration, which must be preserved.
- Docdex symbols locate shared authorization; the Swift impact graph returned no edges, so direct call-site searches and both platform builds will supplement it.

## Current work
Inspect deployed IAM version and compatible Apple provider before implementation. No production changes applied yet.

## Validation and delivery
Pending.
