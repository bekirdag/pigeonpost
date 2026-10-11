# Windows UI refresh — progress

## Baseline

- Authorized implementation and Microsoft Store publication on 2026-10-11.
- Branch: `codex/windows-ui-refresh-20261011`; base: b118dbef2276015b8558c2f87464c2f3ef647dcb.
- Existing Windows Actions pipeline builds x64/ARM64, runs core and native UI tests, validates MSIX and installs/activates the x64 package on a disposable runner.
- Prior native screenshots show permanent sender/subject/message columns, always-visible find controls and an account footer. Staged attachments, previews and per-message copy are already implemented.
- Shared browser connected with original tabs preserved; a separate Partner Center tab is open. No Store changes yet.
- Docdex is indexed for the worktree through its HTTP MCP endpoint (the configured MCP session is bound to the original checkout). Symbols found for C#; dependency edges absent and XAML/PowerShell symbol records unavailable. Manual cross-reference inspection supplements these checks.
- Planning/profile directives reviewed. The user's explicit instruction covers implementation, versioning, git publication and Microsoft Store submission; no additional approval is required for this scope.

## Implementation and validation

- Implemented two-pane default, optional pinned subjects at widths >=1100 logical pixels, compact navigation below 760, current-subject selector, on-demand search, account menu, richer conversation rows and an integrated composer.
- Published Store baseline verified in Partner Center: x64 and ARM64 version 1.0.4.0. This update increments assembly/MSIX to 1.0.5 / 1.0.5.0.
- Local .NET 10.0.401: 74/74 core tests pass. XAML files parse as XML; native compilation remains a Windows-only check.
- `docdexd run-tests --target apps/windows/Pigeonpost.Core.Tests` selected the repository's Cargo runner and rejected the .NET target. Used the workflow's direct `dotnet run --project ... -c Release` command instead.
- Local delegation attempted a small presentation edge-case review through Docdex; the output contained invalid date assertions, so it was rejected and the focused cases were reviewed directly.
- `git diff --check` and `docdexd hook pre-commit` passed. Native tests now exercise search shortcuts, the subject selector, account menu, pinned/medium/compact layouts, draft preservation, and light/dark screenshots in addition to the existing attachment and refresh regressions.
- Store submission has not started. Native Windows build and screenshot review are next.
- First Windows run `38160997415`: core tests passed; ARM64 compilation caught WinUI generated-code CS1503 for a resource converter in a compiled binding rooted at `Window`. Changed the two badge visibility bindings to ordinary data-template bindings, which resolve resources on the template element. Retesting both architectures.
- Run `38161205926` passed all Windows jobs: x64/ARM64 native compilation and MSIX validation, release launch, standard-user UI automation, native previews, clipboard/drop/download flows, compact/pinned layouts, subject switching, archive filter, draft retention, and x64 package installation/activation.
- Inspected wide, compact, subject and dark native screenshots. Refining full-row hover/selection backgrounds and root-level keyboard tooltips; initial viewport was still at old messages, so deferred scrolling now waits for binding/layout. Added an explicit newest-message launch assertion and cleaner fixture copy for Store screenshots. Final rerun pending.
- Shared Playwright stalled on Partner Center's new web components. The existing task tab remains available through direct page CDP at the same persistent browser endpoint; no browser/profile/service restart or login reset was performed.
- Run `38161835511` compiled both architectures but the new launch assertion caught the existing initial-scroll bug. A single deferred ScrollIntoView still did not settle the virtualized extent. Tail requests now settle across bounded frames and cancel on pointer input or context changes; fixture-only geometry traces help verify the behavior. These artifacts are not approved for Store upload.
- Partner Center update draft created: Submission 6, ID `1152921505702097627`. Listing text is being prepared; no new packages uploaded and no certification request sent yet.

## Verified release candidate

- Final implementation commit: `63f01ba854583852a954254df77b4e117a0db1e3`; PR: https://github.com/bekirdag/pigeonpost/pull/37.
- Windows run `38162272856` passed all jobs. Native automation confirms initial newest-message scrolling after the virtualized extent settles, while preserving reader scroll during refresh. C# fixture diagnostics recorded the extent growing before the bounded scroll reached the tail.
- Reviewed final inbox, pinned-subjects, compact, staged-file, subject and dark screenshots. Four captioned native 1426x893 screenshots are saved in the Store listing. Their SHA-256 hashes match the local reviewed PNGs after reloading the saved listing.
- Release artifacts and verification records: `/Users/bekirdag/Documents/releases/pigeonpost/windows-1.0.5/38162272856`.
- x64 MSIX SHA-256: `19547441a1212c2dfc7ed1a340a3e30e53b279d99ffcdf10468044888daa2d49`.
- ARM64 MSIX SHA-256: `1de6a931138e1f4013993e2f0b5d0e1c720f709317447c1ffd9b66504bf657bb`.
- Package identity, publisher, architecture and 1.0.5.0 version verified from each manifest. Release packages contain no preview fixtures or UI-test controls.
- Partner Center validates both packages in Submission 6. Saved package changes remove the superseded 1.0.4 packages from the update draft. Updated description, features, release notes and four screenshots are saved. Publication remains automatic after certification.
- General CI run `38162272844` is completing the Windows lifecycle build; all other checks have passed. Certification submission awaits that final check and PR merge.

## Submission outcome

- All 15 PR checks passed. General CI `38162272844` completed successfully, including Windows Rust tests and the exact Windows client/Loft/Directory lifecycle.
- PR #37 merged on 2026-10-11 at 18:21:33 UTC: `aa4d8428e0f7a09bf02cbbb3023ae3b7909551f3`. Its tree is identical to the tested/package-source commit `63f01ba854583852a954254df77b4e117a0db1e3`.
- Submitted version 1.0.5.0 x64/ARM64 in Partner Center Submission 6 (`1152921505702097627`) on 2026-10-11 at approximately 18:22 UTC. Portal now confirms **Update in certification** and **Your product will start publishing as soon as it passes certification**.
- Public Store presence remains Submission 5 / 1.0.4.0 while Microsoft reviews the update. This is a submitted update, not a claim that 1.0.5 is already publicly available.
- Store link: https://apps.microsoft.com/detail/9N0NWJ9L8XDP. No further publishing action is scheduled or needed after certification unless Microsoft requests changes.
- Implementation, native verification, package/listing upload and certification submission are complete. Remaining external dependency: Microsoft certification and Store propagation.
