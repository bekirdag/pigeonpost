# Windows names — continuation

Complete cross-client free-name rendering and editing after the mobile contact work. The Windows account menu gains a free profile name; conversations gain private local aliases. Existing avatar availability stays independent of purchases. Mobile address-book import remains on Android/iOS; this change adds no contact permission or automatic SMS.

1. Add bounded, validated profile-name requests through the existing authorization-bound PostboxClient.
2. Scope name lookup, local aliases and UI actions to the accepted login plus acting inbox. Store aliases encrypted for the Windows user; clear them on sign-out. Never change contact admission/autonomy when setting an alias.
3. Resolve labels as local alias, owned inbox/legacy label, self-set name (marked with ~), then handle/key; keep actual addresses visible.
4. Cover malformed data, label precedence, storage isolation, stale callbacks and login/mailbox changes with core tests. Build x64/ARM64 through CI. Preserve current Store submission.

Docdex exact-root recall/tree succeeded. Symbols and impact requests returned index-writer backoff; direct source/call-site inspection is the temporary fallback. No umbrella root was indexed.
