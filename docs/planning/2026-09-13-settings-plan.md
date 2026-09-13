# Friendly Settings across Pigeonpost apps

## Goal and acceptance

Replace the long Settings screens with a short, native menu and focused subpages on iOS, macOS, Android, Linux and the web inbox used on Windows. Preserve purchases, account-wide handles, inbox selection, contacts and permissions, archive, storage, sign-in scanning, account deletion, sign out and help. Keep permission grants explicit and preserve store pricing and receipt verification.

The initial screen should contain five clear destinations: Account, Handles, Inbox, Contacts and permissions, Help and about. Use native rows, icons, descriptive subtitles, generous tap targets, readable text and a visible back action. Place account deletion and sign out on Account. Separate owned handles from the purchase/restore form. Retain correct active/expired/provider labels. Large text, narrow layouts, keyboard and system back must work without hiding controls.

## Dependency order

1. Audit current views and their callers, fixtures and test assumptions. Use Docdex symbols and impact graphs; export the retrieval DAG. Swift/Kotlin graphs currently expose no edges, so inspect their call sites manually. Linux UI depends on existing API/auth/model/vault modules; leave those contracts intact.
2. Refactor shared SwiftUI Settings and handle sections, then Android Compose Settings and dialog back navigation. Retain one stable purchase store across subpages.
3. Refactor GTK Settings into native navigation with grouped rows and focused pages, preserving existing contact/handle workflows. Apply equivalent navigation and keyboard/focus behavior to web Settings.
4. Update affected workflow tests and add meaningful navigation/back coverage. Run Swift model/auth/handle checks, native iOS UI checks, Android core/lint/instrumentation, Linux core and GTK installed UI checks, and web browser tests. Visually inspect phone and desktop layouts, including large text.
5. Bump Android and Linux versions; build signed iOS/macOS/Android and Linux packages using existing signing and CI. Verify checksums, macOS notarization and package launches.
6. Commit and push validated changes. Publish macOS/Linux releases before updating download URLs. Upload new mobile builds, distribute internal testers and replace public review submissions while preserving all ten handle products and existing store metadata. Verify the actual resulting review states.
7. Deploy changed web files atomically with backups, verify public content and runtime manifests, fast-forward clean server source checkouts, and record final release/commit evidence.

## Validation and rollout limits

No real purchases during validation. Existing reviewed billing controllers and permission confirmations remain unchanged. Store approval is external; report submitted/in-review separately from live downloads. The native Windows preview is not yet shipping; improve its published web inbox route and do not merge unfinished Windows work. Preserve unrelated worktrees and user browser tabs.

Use a dedicated worktree and private audit directory. Keep prior release assets immutable and retain deployment backups for rollback. The user's request explicitly authorizes implementation, signing, publishing, deployment and Git synchronization; no additional approval is needed for this scope.
