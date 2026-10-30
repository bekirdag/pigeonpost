# Windows names — progress

- Isolated worktree `pigeonpost-contacts-windows-20261030`, baseline `785b1e23`; main checkout preserved.
- Added bounded/validated profile-name API, free name/visibility settings and per-device/per-inbox local alias editor. Local aliases never call the contact admission/autonomy API. Self-set names have the same `~` marker as web/mobile; actual addresses remain visible.
- Alias files use the existing Windows per-user DPAPI protector, are separated by owner/origin/inbox, and are removed on sign-out. Retired dialog/request/encryption callbacks cannot write into a new login or recreate a cleared file.
- Label precedence: local alias → owned inbox/legacy contact label → self-set name → handle/key. Profile visibility revocations remove prior cached names; new conversation names load without waiting for the periodic refresh.
- Docdex symbols recovered for ConversationBuilder. Graph requests returned empty C# edges, so direct call-site review supplemented them. A local-model validator draft was invalid and was not applied.
- The system .NET SDK is too old. Existing isolated SDK is `Documents/releases/pigeonpost/continuation-20261024/tools/dotnet10/dotnet`; core build has zero warnings/errors. **501/501 core/MLS tests pass**, including nine new behavior/security checks. Reused the existing native MLS library only after confirming its source is unchanged from qualified Windows #173.
- Native x64/ARM64 builds and UI regression suite pending. No app version or Store submission changed. Phone contact import remains in the Android/iOS native work; this Windows change requests no address-book permission and sends no SMS.
