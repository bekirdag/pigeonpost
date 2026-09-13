# Account handle consistency

## Goal

Handles purchased through Apple or Google Play belong to the authenticated Pigeonpost account. The same account must see and use its handles on `/account`, the web inbox, iOS, Android, macOS, Linux and the Windows preview. Billing-provider identity must never replace Pigeonpost ownership or permit another account to claim a purchase.

## Build and validation plan

1. Trace Apple/Google claim, restore and renewal routes through the canonical namespace/account tables, including expiry and unassigned purchases. Compare deployed configuration and ownership records without exposing receipts, tokens or personal data.
2. Audit every client's account identifier, handle endpoint, rendering, refresh and error handling. Reproduce missing or stale handle views with focused regressions before modifying code.
3. Fix the narrowest shared layer first, then affected clients. Preserve ownership isolation, provider binding, expiry/recovery policy and safe retry behavior. Show unavailable data as an error rather than an empty account.
4. Verify Apple, Google and web handle sources together; same-account access across clients; unrelated-account rejection; active/expired/restored subscriptions; network/auth errors and refresh after a mobile purchase. Use isolated QA data for live checks, with no real charge or unrelated messages.
5. Run relevant tests/builds, review the complete diff and record evidence separately. Package and submit new app versions only where client changes are required.
6. Commit and push validated work. Back up affected production files/data and preserve rollback before deployment. Verify live account output, service health and release artifacts, then leave local/main and known server Git checkouts clean and synchronized.

## Release authorization

The user explicitly requested fixes and deployment to relevant production environments, including apps where necessary. Existing authorization covers credential use and the normal release workflow. No separate approval is needed for these steps.
