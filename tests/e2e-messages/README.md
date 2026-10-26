# Native direct-message interoperability

The Windows Core client also has an opt-in executable against the same disposable Rust postbox:

```sh
tests/e2e-messages/windows.sh /absolute/path/to/target/debug/pigeonpost-postbox
```

It requires .NET 10 and Node. `PIGEONPOST_TEST_DOTNET` can select an already installed SDK executable.
It qualifies folded edits, version acknowledgement/info, reply context, copy ownership, receipt
privacy, exact explicit send/edit recovery after committed response loss, and whole-version
deletion local to the receiving mailbox. The ordinary Core runner has no conditional skip; this
target is explicit. Its setup file and all server state are private, synthetic and loopback-only.

The Android opt-in target uses the app's actual Kotlin `PostboxClient` against a real Rust postbox,
temporary loopback OIDC issuer and a transport fault proxy. It never uses a signed-in account or
changes production flags. The fixture enables message actions and filtering only in its own DB.

Build the postbox with `cargo build -p pigeonpost-postbox`. Prepare the Android group-crypto AAR
and host library as described in the Android build instructions: configuring the Gradle root
requires the existing AAR even though this target exercises direct messages. Use JDK 17 and the
normal Android SDK environment. Then, from the repository root:

```sh
tests/e2e-messages/android.sh /absolute/path/to/target/debug/pigeonpost-postbox
# Real MLS group actions, using the same protected loopback fixture:
tests/e2e-messages/android.sh /absolute/path/to/target/debug/pigeonpost-postbox groups
# Native calendar protocol, using the same disposable postbox and response-loss proxy:
tests/e2e-messages/android.sh /absolute/path/to/target/debug/pigeonpost-postbox calendar
```

The runner invokes `:core:messageActionsPostbox`, a `JavaExec` task in the test runtime classpath.
It does not add a conditional JUnit test or a new default skip. The harness rejects non-loopback
origins. No extra dependency is required.

The optional `groups` mode also enables groups/disappearing on the fixture and runs
`:core:groupActionsPostbox` against the host MLS library. It registers real member devices, adds a
recipient with a verified Welcome, exercises encrypted replies/edits and retained attachment keys,
checks member receipt privacy, recovers committed send/edit loss after importing saved sender
state, and reconciles whole-chain deletion through metadata-only pages. Missing crypto is a
failure for this explicit target. This does not enable group features on any production server.

The optional `calendar` mode enables the calendar only in the fixture, adds synthetic verified
sign-in claims, and runs `:core:calendarPostbox`. Contact delivery remains off; no external invitation
is sent. It checks calendar/event/import-confirmation response loss and exact replay, mailbox
ownership, attendee contact redaction, organizer-only guest links, independent RSVP and scheduling
revisions, recurring occurrence identity, personal reminders, ICS privacy, settings, feed rotation
and revocation, and unknown availability. Feed probes reject every non-loopback origin. The
calendar executable is opt-in and adds no conditional JUnit skip.

Checks cover capability discovery; distinct sender/recipient copy IDs; delivered, seen and agent
state; folded edits, fresh unread revisions and versions; reply context; foreign references and
mailbox ownership; sender/recipient receipt preferences; Filtered activity privacy; and explicit
recovery after both a send and an edit commit but their HTTP response is lost. The proxy retains
only request hashes, request references and HTTP status codes for retry comparison. The initial
loss must result in one attempt; only an explicit retry may recover the duplicate response.

Temporary credentials are written with mode `0600` inside the runner's private temporary
directory, never printed or passed in command arguments. The postbox inherits no provider or
service credentials; its DB, KMS file, loft, blobs, OIDC issuer and registry fallback all belong to
the disposable fixture. Cleanup terminates only this runner's fixture and its child, and removes
only its own temporary files. The setup file must be new and is never overwritten.
