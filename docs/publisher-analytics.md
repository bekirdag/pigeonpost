# Publisher app analytics

Account → App analytics (`/account#analytics`) and Back office → App analytics
(`/staff#analytics`) share the same report. Publishers select one of their own publisher
profiles; staff with the finance role can search all publishers (including the platform owner when assigned that role). Publisher
finance members can read reports without permission to edit apps. Staff reads require strong
sign-in and are audited. No buyer identifiers, job contents or bank details are returned.

## Metrics

- Jobs: server-recorded submissions in the selected UTC interval across both Nest and
  publisher-hosted execution paths, with current success/failure/cancellation/in-progress state.
  Synthetic integration review jobs are excluded. Counts cover retained operational records.
- Active users: distinct submitting accounts across the selected apps, not a sum of per-app users.
- New installations: installation records created in the interval. Active installations: current
  active records, explicitly a snapshot rather than a historical count.
- Earnings: the finance ledger's accruals dated in the interval, from card and credit sales.
  Gross, tax, commission, publisher share and currently reversed share are shown separately.
  Net earnings = publisher share minus reversed share. A later reversal updates the original
  sales cohort. This is not a cash-flow report of reversals by their reversal date.
- Currency totals are separate (TRY, USD, EUR). No exchange rates or inferred revenue from
  current listing prices. Pending holds are not sales. A missing finance response is unavailable,
  never zero. Publisher earnings are before any payout withholding.
- Balances are current publisher-wide payable, reserve and recoverable ledger balances.
  Paid out is bank-reconciled transfers settled in the interval; prepared, sent and returned
  transfers are excluded. App filters never pretend to allocate publisher-wide bank transfers.

The UI's end date is inclusive; API `from`/`to` are RFC3339 instants, `from <= event < to`.
Default30days; maximum366days. Per-app results include withdrawn listings. Charts include
zero days. CSV uses minor units and guards spreadsheet formula injection; unavailable money is
blank. If finance has an orphaned listing attribution it remains in aggregate earnings and a
notice explains why it cannot be shown against a current app.

## API and deployment

- `GET /v1/publishers/{publisher}/analytics?from=&to=&listing_id=` — PublisherManage team read.
- `GET /v1/staff/publishers/{publisher}/analytics?...` — StaffFinanceRead, same data.
- `GET /v1/staff/publisher-analytics?q=&offset=` — staff publisher picker,50per page.
- saas_be: `GET /v1/internal/tenants/{tenant}/marketplace/finance/publishers/{publisher}/analytics`
  uses existing marketplace:finance authorization. Postbox's existing
  POSTBOX_SAAS_FINANCE_URL and POSTBOX_SAAS_FINANCE_TOKEN_FILE are reused server-side only.

Deploy the additive saas_be endpoint before postbox, then publish matching account/staff assets.
No financial schema change, no payment writes, no new dependencies. SQLite adds three query
indexes after the hosted-job schema migration. Usage uses a SQLite read snapshot; money uses a
MySQL repeatable-read snapshot. They can be sampled seconds apart. A report over10000financial
day/app/currency groups fails explicitly instead of silently truncating; narrow the range.
