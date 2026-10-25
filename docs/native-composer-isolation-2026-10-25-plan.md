# Native composer isolation

CI found ordinary draft text mixed with message edits in the existing iPhone composer and group suites. Message actions start synchronously, but the same native input view and its pending text callbacks currently span both contexts.

1. Give each direct-message edit a native input identity containing its mailbox, conversation, subject and edit UUID. Capture that identity in text/focus/attachment bindings and reject callbacks from retired contexts.
2. Retire UIKit/AppKit delegates before removing a text view. Preserve IME composition within one context; a different edit gets a different view and undo/composition state.
3. Give the shared group composer a new input generation on edit entry and exit, with generation-bound text and focus.
4. Wait for the intended edit text before UI tests type. Wait for the calendar editor to dismiss before opening the newly created event's button.
5. Run actual iPhone/iPad edit, reply and ordinary-send regressions, both app builds and hosted Mac checks. Propagate qualified changes through the stacked calendar PRs. Do not merge failing CI or submit stores.

Docdex symbols located the definitions. Swift impact graphs return no edges; check both native targets and the shared group view explicitly. No schema, network, billing, or release-policy changes.
