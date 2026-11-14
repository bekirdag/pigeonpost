# Changelog

All notable changes to this project are documented in this file. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2026-11-14

### Added
- `PPTheme` (light and dark) from the generated `PPLight` / `PPDark` tokens, a Material 3 colour scheme mapped from the semantic roles, Geist and Geist Mono type roles with line heights.
- 181 Lucide icons as vector drawables with the `PPIcon` enum and `PPIconView`.
- Actions: `PPButton` (9 variants, 3 sizes, loading), `PPIconButton`.
- Inputs: `PPTextField` (label, hint, error), `PPOTPField`, `PPSwitch`, `PPToggleRow`.
- Display: `PPAvatar` with `PeerFace` initials and the web app's tone hash, `PPPresenceDot`, `PPBadge`, `PPTag`, `PPCard`, `PPListRow`, `PPSectionHeader`, `PPEmptyState`.
- Feedback: `PPBanner`, `PPToast`, `PPProgress`, `PPMeter`, `PPSkeleton`, `PPSpinner`.
- Messaging: `PPMessageBubble` (mine, theirs, agent; 18/6 bubble corners), `PPTicks`, `PPDayDivider`, `PPTypingIndicator`, `PPComposer`, `PPThreadRow`, `PPConversationHeader`, `PPAddressChip`, `PPAgentBadge`.
- Calls: `PPCallButton`, `PPCallControls`, `PPIncomingCallView`, `PPCallTimer`.
- Commerce: `PPPricingCard`, `PPCreditChip`, `PPCreditBalance`, `PPListingCard`, `PPNestCard`, `PPNestStatusPill`.
- Navigation: `PPTabBar`.
- `sample` app with Inbox, Conversation and Call screens.
- JVM unit tests for token mapping, the Material scheme, bubble shapes and `PeerFace`.
