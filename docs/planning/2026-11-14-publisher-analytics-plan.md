# Publisher analytics

## Contract
Publishers see a dedicated Analytics screen with publisher selection, UTC date range, app filter, usage totals and daily activity, per-app comparison and earnings split by currency. Staff finance roles get the same report and a searchable publisher picker. No buyer identifiers, content, bank details or upstream credentials reach the report.

Usage counts server-recorded installations and real jobs (both execution paths), excluding synthetic review jobs. New installations are range-filtered; active installations are a current snapshot. Jobs are grouped by submission date with their current outcome. Distinct callers are deduplicated across apps in publisher totals.

Money comes from saas_be accruals and reversal records, not listing prices or pending holds. No 2000-row statement truncation and no mixed-currency sum. Show gross, tax, commission, reversals, net publisher earnings; current payable/reserve/recoverable balances and reconciled payouts separately at publisher scope. Filtered earnings remain attributable to listing IDs, including withdrawn apps.

## Change order
1. Inspect finance schema and dependency graphs; add bounded-date aggregated publisher analytics endpoint to saas_be and tests for attribution, currencies, reversals and large datasets.
2. Add Pigeonpost analytics module: validated range/app filter, aggregated usage, shared finance client, publisher/team gate and staff finance gate+audit. Add route tests for isolation and exact totals.
3. Add shared accessible responsive analytics UI, mount as account#analytics and staff#analytics, link from publishing console, loading/empty/error/unavailable states and CSV export. Use existing design tokens and no new dependencies.
4. Validate Rust, TypeScript and browser tests; live browser smoke only after deployment is performed. Record evidence separately.
