# Easy address copying and consistent app icons

## Goal

Make the current user's Pigeonpost address obvious and easy to copy on iOS, macOS, Android, Linux and the published web inbox used on Windows. Use a visible, accessible copy icon beside addresses in the inbox header, mailbox selection and account details, with clear success feedback. Copy the complete canonical readable address when assigned, otherwise the complete /k/ address; never copy display-name abbreviations or credentials. Keep mailbox switching and address copying separate actions. Preserve the focused Settings layout and existing billing, ownership and permission behavior.

Match Android's launcher and Google Play's store-listing icon to the existing iOS multicolour bird on white. Reuse the authoritative artwork, support Android adaptive/themed icon masks without clipping, and retain a repeatable asset export source. Inspect light/dark launchers and the store preview.

## Implementation order

1. Audit address display/copy call sites and icon sources. Load Docdex memories, inspect symbols and dependency impact for every changed source. Use DAG facts and manual callers where language graphs lack edges.
2. Add small reusable native copy controls and update inbox/header, picker, account and relevant handle address rows. Keep labels, tap targets, keyboard focus and copied feedback accessible. Update clipboard error handling on web.
3. Export Android icon assets from the iOS source and prepare the exact 512px Google Play icon. Validate adaptive icon safe areas, monochrome silhouette and icon resources in the packaged app.
4. Test complete copied values (readable and key addresses), mailbox-switch independence, success/failure feedback, large text and narrow layouts. Run native UI/model checks and platform builds, web tests, Linux native CI, and image/resource validation. Record actual evidence and fix failures.
5. Version and publish signed iOS/macOS/Android and Linux releases. Verify macOS notarization, Linux package checksums/attestations and Android upload certificate. Replace public store submissions, preserving ten handle products and reviewer access. Update Play listing artwork and verify saved/published state.
6. Publish verified desktop links and deploy changed web files with hash guards/backups. Merge tested changes, push and fast-forward clean production Git checkouts. Record source/build IDs and distinguish store review from availability.

## Scope and rollout

Native Windows remains an unpublished preview; update its supported web inbox route. The user's instruction authorizes commits, signing, publishing, store metadata changes and deployment. No real purchase is necessary. Work in an isolated checkout; preserve user apps, credentials, unrelated worktrees and browser tabs. Keep older public release assets immutable and retain deployment backups.
