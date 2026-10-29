# Contacts, phone numbers, names and SMS invites

Client contract for SDS §14D (R-41–R-46). Server: `crates/pigeonpost-postbox/src/contacts.rs`, `profile_names.rs`,
`store/contacts.rs`. Web: `site-inbox/contacts.js`. OpenAPI: `openapi/pigeonpost.v1.yaml` (tags `phone`, `contacts`,
`profile`).

Everything here sits behind feature flags. A client must read `GET /v1/capabilities` and show a surface only when its flag
is in `features`:

| Flag | Surface |
|---|---|
| `phone_numbers` | Settings → Phone number: add, verify, discoverable toggle, unlink |
| `contact_discovery` | Contacts → Find people I know |
| `sms_invites` | Invite by SMS on a contact that is not on Pigeonpost |
| `display_names` | Display-name setup (onboarding + settings) and names in lists and headers |
| `avatars` (existing) | Profile pictures. **Not paid-gated**, see below |

Local aliases need no flag: they never leave the device.

## 1. Privacy rules every client must keep

1. **Nothing leaves the device without an explicit action.** Reading the address book is started by the person ("Find
   people I know"), never at launch or in the background. Invites are sent one at a time, each on a tap. There is no
   "invite all", no pre-selected list and no automatic resend.
2. **Names, emails, photos and notes from the address book are never uploaded.** Only the truncated hashes in §3 are sent.
   Raw numbers are sent only in two cases: your own number while you verify it, and the one number you tap
   **Invite by SMS** on.
3. **Do not persist the hashes or the address-book snapshot on the server.** The server does not store them either (§3.4).
   A client may cache *its own* results locally (matched address per hash) so a re-check sends only new contacts.
4. **A self-set display name is not a verified identity.** Render it as specified in §6.3 and keep the address or handle one
   tap away.
5. **Aliases are local.** Never write a local alias to `PUT /v1/contacts` (that `alias` column is server-visible and belongs to
   the trust rules).

## 2. Phone-number normalisation (E.164)

All hashing and every phone request uses E.164: `+`, country calling code, national significant number, digits only, 8–15
digits after the `+`, the first one 1–9.

- **Native:** use the platform's library (below). Default region: the device region (iOS `Locale.current.region`, Android
  `TelephonyManager.networkCountryIso` → `simCountryIso` → `Locale.getDefault().country`, Windows
  `GlobalizationPreferences.HomeGeographicRegion`, Linux the territory of `LC_TELEPHONE`/`LANG`), overridable in the import
  screen.
- **Web** (`site-inbox/contacts.js`, `normalizeE164`): strip spaces, dots, dashes, parentheses and a `tel:` prefix; `+…`
  stays; `00…` (and `011…` with a NANP default) becomes `+…`; otherwise one national trunk `0` is dropped and the chosen
  country's calling code is prefixed (NANP numbers have no trunk prefix; a leading `1` is the country code). Anything that is
  not 8–15 digits afterwards is skipped and counted as "couldn't read". This is deliberately simpler than libphonenumber; a
  number it gets wrong simply fails to match.
- Extensions (`;ext=`, `x123`) are dropped. Short codes and numbers with letters are skipped.

## 3. Contact discovery

### 3.1 The hash

```
h = first 16 bytes of SHA-256( "pigeonpost:phone:v1:" || E164 )      → 32 lowercase hex characters
```

Test vectors (fictional numbers; every client's unit tests must assert these):

| E.164 | `h` |
|---|---|
| `+15555550100` | `7c0dc19d3eae376a630cee358d0f8912` |
| `+447700900123` | `5c4fd5bb0f19345b15863f865cb76cee` |
| `+61491570156` | `511ebc743c8a1d1ea8be5541f2fee95d` |

The server never compares `h` with anything stored. It computes `tag = HMAC-SHA256(K, h)[..16]` with a key derived from the
server's vault master key, and matches `tag` against the tags of verified, discoverable numbers. The same `tag` is computed
when a number is verified, so both sides agree without the database holding a dictionary-searchable hash.

> **Residual risk, stated plainly.** Phone numbers are a small space (about 10^10 for the whole world, far fewer per
> country). `h` is an unkeyed hash, so anyone who sees an upload — the server above all — can recover the numbers by trying
> every number. The truncation is for size, not secrecy. The server *could* do this; it is designed not to keep anything to
> try it on (uploads are matched in memory and discarded). The server-held key protects the *stored* table (a stolen
> database cannot be brute-forced without the key) but not a compromised running server. Opt-in discoverability and the
> limits in §3.3 bound what a malicious *client* can learn: it can only test numbers it already has, at a few hundred to a
> thousand new numbers a day, and only learns about people who chose to be found.

### 3.2 `POST /v1/contacts/discover`

Person only (an interactive member session). API keys, mailbox tokens and connectors get `403 credential_not_accepted`
or `403 human_required`.

```json
{ "hashes": ["7c0dc19d3eae376a630cee358d0f8912", "…"] }
```

- 1–500 hashes per request, each exactly 32 lowercase hex characters. Duplicates are ignored.
- Response `200`:

```json
{
  "matches": [
    { "hash": "7c0dc19d3eae376a630cee358d0f8912", "address": "/k/…", "handle": "/bekir/main", "display_name": "Bekir" }
  ],
  "checked": 120,
  "new": 14,
  "remaining_today": 986
}
```

`matches` lists only people who verified that number **and** turned on "Let people who have my number find me", whose
mailbox still exists and who have not blocked any of your mailboxes. Your own numbers are never returned. `handle` and
`display_name` may be `null` (`display_name` is filtered by its own visibility setting, §6). Use `GET /v1/avatars` for
pictures as usual.

Errors: `400 bad_request` (shape), `404 feature_disabled`, `429 rate_limited` (with `Retry-After` and `limit`:
`requests` | `daily` | `monthly`), `403 discovery_locked` (abuse lockout, with `until`), `503 discovery_busy`
(platform-wide circuit breaker).

### 3.3 Limits (server-enforced; clients should batch and cache to stay well inside them)

| Limit | Value |
|---|---|
| Hashes per request | 500 |
| Requests per account | 20 per hour |
| Requests per IP | 30 per hour |
| New hashes per account | 1,000 per 24 h (200 while the account is under 24 h old), 5,000 per 30 days |
| Platform-wide new hashes | 100,000 per hour, then `503 discovery_busy` |
| Abuse lockout | hitting the 24-hour cap on 3 different days within 30 days locks discovery for 30 days |

"New" means not checked by this account in the last 7 days. Re-checking a contact you already checked costs a request but
not budget, so a periodic refresh of the same address book is cheap. A client should still send only contacts added or
changed since its last check.

### 3.4 What the server keeps

Nothing of the upload. For rate limiting only:

- per account per day: a count of new hashes (kept 31 days);
- per account: one 12-byte mark per checked hash, `HMAC(K_rl, account || h)`, deleted after 7 days. It cannot be compared
  with any other account's marks or with the phone table.

The match itself happens in memory; hashes are never logged (the request body is not logged by the postbox or its proxy).

## 4. Phone numbers on inboxes

One verified number per mailbox, at most 3 per account; a number belongs to one mailbox on the whole platform. Person-only
endpoints (member session).

| Call | Purpose |
|---|---|
| `GET /v1/phone` | Your numbers: `{numbers:[{id, identity, country_code, masked, discoverable, verified_at}], max_numbers}` |
| `POST /v1/phone/verify` `{phone, identity?}` | Send a 6-digit code. `202 {challenge_id, expires_at, resend_after, code_length:6}` |
| `POST /v1/phone/verify/{challenge_id}` `{code, discoverable?}` | Check the code. `200 {number}` |
| `PATCH /v1/phone/{id}` `{discoverable?, identity?}` | Turn discoverability on/off, or move the number to another of your mailboxes |
| `DELETE /v1/phone/{id}` | Unlink |

- `identity` defaults to the acting mailbox (`?identity=`/header rules as elsewhere); it must be yours.
- **Discoverability is off unless the person turns it on**, either with `discoverable: true` on the verify call (only when
  the person ticked the box on that screen) or later with `PATCH`.
- `masked` looks like `+90 ••• ••• ••12`. The full number is never returned, not even to its owner.
- The code: 6 digits, valid 10 minutes, 5 wrong tries end the challenge (`410 challenge_expired`). A wrong code answers
  `400 invalid_code` with `attempts_left`. Calling `POST /v1/phone/verify` again for the same number replaces the pending
  code (subject to the resend limits).
- Resend limits: 1 per 60 s and 5 per 24 h per number; 10 codes per 24 h per account; 3 pending challenges per account;
  plus the platform SMS budget (hourly, daily, per country). A refusal is `429 rate_limited` with `Retry-After`;
  `503 sms_unavailable` when the budget or the SMS provider is exhausted or down.
- Refused numbers (premium-rate, satellite/international networks and any configured blocked prefix):
  `422 phone_not_supported`. The answer is the same whether or not the number is already on Pigeonpost; the API never says
  whether a number is registered.
- **Recycled numbers:** if someone else verifies a number you had linked, it moves to them; your link is removed and you
  get a `phone.unlinked` event with `reason: "verified_elsewhere"`.
- The SMS reads `Pigeonpost: your verification code is 123456. It expires in 10 minutes. Never share it.` followed by
  the origin-bound line `@inbox.pigeonpost.dev #123456`, so the web inbox can use the WebOTP API and iOS can offer
  the code in the keyboard (`textContentType = .oneTimeCode`). Android: SMS Retriever / User Consent API
  (`SmsRetriever.getClient(ctx).startSmsUserConsent(null)`) — never request `READ_SMS`.

Events on `/v1/events`: `phone.verified`, `phone.updated`, `phone.unlinked` (`{id, identity, reason?}`; no number).

## 5. SMS invitations

`POST /v1/contacts/invites` — person only, flag `sms_invites`, and the sender must have at least one verified number.

```json
{ "phone": "+15555550100", "confirm": true }
```

- `confirm` must be `true`. It records that a person tapped **Invite**; a client must never send it otherwise.
- `200 {"status":"sent","invite_id":"inv_…"}`, or `200 {"status":"not_sent","reason":…}` with `reason`:
  - `already_invited` — you invited this number in the last 30 days;
  - `unavailable` — the number cannot receive invites now (opted out, invited recently by others, not supported). The
    reasons are merged on purpose: the API must not reveal whether a number opted out or is on Pigeonpost.
- `429 rate_limited` (`limit`: `daily` 5 per day, `monthly` 20 per 30 days per sender), `403 phone_required` (verify
  your own number first), `503 sms_unavailable`.
- Per receiving number, across all senders: at most 1 invite per 7 days and 3 per 30 days.
- Text (one GSM-7 segment for ASCII names): `Pigeonpost: {name} invited you to chat on Pigeonpost. Join: https://pigeonpost.dev/invite?c={code} Reply STOP to opt out.`
  `{name}` is the sender's display name reduced to letters, digits, spaces and `.'-`, at most 24 characters; otherwise
  `A friend`. Never the sender's number or address.
- The link is not needed to join (anyone can install and sign up). `c` is an opaque random code: it says who invited
  (for a future referral credit) but nothing about the invitee. The page offers the downloads and **Stop invitations to
  this number**, which calls `POST /v1/sms/opt-out {"code": "…"}` (public; always answers `200 {"ok":true}`).
- "Reply STOP": Twilio's standard opt-out keywords stop further SMS from our sender at Twilio; the opt-out link
  additionally records a keyed hash of the number so the postbox never tries again.
- The server keeps, for 30 days: the invite code, the sender account and the number's keyed hash (dedup and throttles).
  Opt-outs are kept as the keyed hash only, for as long as the service runs.

## 6. Names

### 6.1 Self-set display name (`display_names`)

Per mailbox, free for everyone, chosen during onboarding or in Settings → Profile.

| Call | Purpose |
|---|---|
| `GET /v1/profile/name` | `{name: {display_name, visibility, updated_at} | null}` |
| `PUT /v1/profile/name` `{display_name, visibility?}` | Set (10 changes per mailbox per day) |
| `DELETE /v1/profile/name` | Remove |
| `GET /v1/profiles?addresses=a,b` | Up to 100 addresses → `{profiles: {a: {address, handle, display_name} | null}}` |

All take `?identity=` for the acting mailbox. Mailbox tokens and API keys may set their own mailbox's name; a connector
acting for the member may not. Lookup requires a credential (no anonymous lookup).

Rules (server-enforced; clients should pre-validate): 1–40 characters after trimming, single spaces; no `/` or `@`; no
control, format, bidi-override, zero-width or invisible filler characters; must not contain `pigeonpost` and must not be
`admin`, `support`, `official`, `verified`, `security`, `system` or `moderator` (any case). Send NFC-normalised text.

Visibility: `known` (default — people you have written to, your contacts and org/group relations), `everyone`, `nobody`,
with the same meaning as profile pictures. Event: `profile.name` `{address, handle, display_name|null}` to the owner and to
correspondents who may see it.

### 6.2 Local alias (no flag)

What *you* call someone. Stored on the device only, never sent to the server. There is no encrypted account-state sync in
Pigeonpost yet; when it exists, aliases move into it end-to-end encrypted. Until then an alias set on the phone does not
appear on the laptop. Storage: web `localStorage` key `ppi_aliases_v1:<sign-in subject>` (JSON `{peer: alias}`; the browser does not know the account id, so it uses the token's `sub`, cleared on sign-out), iOS/macOS
the app's sandboxed `UserDefaults` suite or a file in Application Support (excluded from iCloud backup is not required),
Android app-private `SharedPreferences` (subject + inbox scoped, backup disabled), Windows `ApplicationData.Current.LocalSettings`, Linux
`$XDG_DATA_HOME/pigeonpost/aliases.json` (mode 600). Up to 64 characters, same character rules as display names minus the
reserved words. Removing a conversation does not remove its alias; signing out does.

### 6.3 Which name to show, and how

Precedence for a peer:

1. local alias → plain text;
2. server contact alias (legacy `contacts.alias`) → plain text;
3. self-set display name → shown as **`~Name`** (tilde, secondary text colour) everywhere it appears outside the contact's own
   detail screen, and never with any badge;
4. the Phase 1d name derived from a handle (`/bekir/main` → "Bekir");
5. the short `/k/…` form.

The conversation header always shows the address or handle as its subtitle, and tapping the title opens the peer info sheet
with the full address/handle and copy. A display name must never be rendered in the handle's style (monospace / leading
`/`), never replace the handle in the "to" field, and never be used for search-to-send (sending always resolves the address).

**Handles stay paid.** Nothing here changes how handles are bought, claimed or shown.

## 7. Profile pictures

The server has never checked a plan for pictures (`avatars.rs` only checks the `avatars` flag), and no client checks one.
Free accounts can upload. The flag is on globally in production (checked 2026-10-29); the existing limits stay: 10 MB upload, JPEG/PNG/WebP
(HEIC converted on the device), metadata stripped, isolated decoder, 5 changes per mailbox per day, reportable via
`POST /v1/report-avatar`.

## 8. Web (`site-inbox`) — shipped in this round

- **Settings → Phone number** (`phone_numbers`): add (country picker + number), code entry with WebOTP
  (`navigator.credentials.get({otp:{transport:['sms']}})` where available), discoverable toggle (off by default), move to
  another mailbox, unlink.
- **Settings → Contacts → Find people I know** (`contact_discovery`): the browser cannot read an address book, so the person
  picks a `.vcf` or `.csv` export (Google Contacts, iCloud, Outlook). The file is parsed in the page, numbers normalised
  and hashed with WebCrypto, and only hashes are sent. Results: **On Pigeonpost** (Start chat) and, when `sms_invites` is
  on, **No match found** (Invite by SMS, one tap per person, with a confirmation that shows the exact text). The
  file and the list stay in memory and are gone when the sheet closes.
- **Display name** (`display_names`): a one-time prompt after the inbox opens ("What should people call you?", skippable),
  and Settings → Profile → Display name with visibility.
- **Edit name** on a conversation (peer info sheet): local alias.
- **Profile pictures** for free users: unchanged flow, no plan check.

## 9. Native tasks (follow-up native round)

Each platform: gate every surface on its flag; implement the hash with the test vectors in §3.1 as a unit test; implement
name precedence (§6.3) with a table test shared with `site-inbox/test/contacts.test.mjs`; never upload names; store aliases
locally (§6.2); add `phone.*` and `profile.name` event handling; update the store privacy declarations before release.

### iOS / iPadOS / macOS (`apps/ios`, `apps/mac`)

1. **Contacts access:** `import Contacts`. Add `NSContactsUsageDescription` ("Pigeonpost checks which of your contacts
   use Pigeonpost. Only scrambled numbers leave your device."). Check `CNContactStore.authorizationStatus(for: .contacts)`;
   request with `CNContactStore().requestAccess(for: .contacts)` only after the person taps **Find people I know**.
2. **iOS 18 limited access:** handle `.limited` as a first-class state: fetch only what is shared, show "You shared N
   contacts" and offer `ContactAccessButton(queryString:)` (ContactsUI, iOS 18) inside the search field and the
   `.contactAccessPicker(isPresented:)` modifier for "Choose more contacts". Re-read on `CNContactStoreDidChange`. Do not
   nag for full access.
3. **Fetch:** `CNContactFetchRequest(keysToFetch: [CNContactIdentifierKey, CNContactGivenNameKey, CNContactFamilyNameKey,
   CNContactPhoneNumbersKey] as [CNKeyDescriptor])`, `enumerateContacts(with:)` on a background queue.
4. **E.164:** PhoneNumberKit (MIT) via SwiftPM, default region `Locale.current.region?.identifier`.
5. **Hash:** `CryptoKit.SHA256`, first 16 bytes, hex. Cache `{h: address}` locally (Application Support, file
   protection `.completeUntilFirstUserAuthentication`).
6. **Phone verification UI:** `TextField` with `.textContentType(.oneTimeCode)`, `.keyboardType(.numberPad)`.
7. **Invites:** an **Invite** button per non-matched contact → confirmation sheet showing the SMS text → `POST
   /v1/contacts/invites`. Do *not* use `MFMessageComposeViewController` (that would send from the person's own number and
   is a different product decision).
8. **Display name / alias:** onboarding step, Settings → Profile, peer sheet "Edit name". Render `~Name` per §6.3.
9. **Privacy:** App Privacy labels: declare *Contacts* as collected for App Functionality, not used for tracking (hashes
   leave the device and the server keeps 7-day per-account marks, which is longer than real-time servicing, so "not
   collected" would be wrong). *Phone number*: collected, linked to the user, App Functionality. Update
   `docs/apple-privacy-labels.md` in the same release.

### Android (`apps/android`)

1. **Permission:** `<uses-permission android:name="android.permission.READ_CONTACTS"/>`; request with
   `registerForActivityResult(ActivityResultContracts.RequestPermission())` only from **Find people I know**, after an in-app
   prominent disclosure screen (Play User Data policy). Use `shouldShowRequestPermissionRationale` for the second ask; after
   "Don't ask again", deep-link to app settings.
2. **Query:** `contentResolver.query(ContactsContract.CommonDataKinds.Phone.CONTENT_URI, arrayOf(Phone.CONTACT_ID,
   Phone.DISPLAY_NAME_PRIMARY, Phone.NUMBER, Phone.NORMALIZED_NUMBER), null, null, null)` on `Dispatchers.IO`.
3. **E.164:** prefer `NORMALIZED_NUMBER`; else `PhoneNumberUtils.formatNumberToE164(number, countryIso)` with the country
   from `TelephonyManager.networkCountryIso`/`simCountryIso`/`Locale`.
4. **Hash:** `MessageDigest.getInstance("SHA-256")`, first 16 bytes, hex.
5. **Code entry:** SMS User Consent API (`play-services-auth-api-phone`), `autofillHints = SMS_OTP`. Never `READ_SMS`.
6. **Invites, names, aliases:** as iOS 7–8 (aliases in app-private SharedPreferences, subject + inbox scoped).
7. **Play Data safety:** add *Contacts* (collected, not processed ephemerally, not shared, optional, App functionality —
   the per-account abuse-prevention marks persist for seven days) and
   *Phone number* (collected, not shared, optional, App functionality, Account management). Update
   `docs/android-play-data-safety.md`.

### Windows (`apps/windows`)

1. **Primary path: manual import** (most Windows people stores are empty): `FileOpenPicker` (`.vcf`, `.csv`) with
   `InitializeWithWindow`, parse with the same rules as the web (port `parseVcard`/`parseCsv` from
   `site-inbox/contacts.js`, keep its tests).
2. **Optional People access:** `ContactManager.RequestStoreAsync(ContactStoreAccessType.AllContactsReadOnly)` needs the
   `contacts` capability in `Package.appxmanifest`; add it only if the Store listing's privacy text is updated in the same
   submission. `ContactPicker` (`Windows.ApplicationModel.Contacts`) with `InitializeWithWindow` lets a person pick
   contacts without the capability; offer it beside file import.
3. **E.164:** libphonenumber-csharp (Apache-2.0) with `GlobalizationPreferences.HomeGeographicRegion`.
4. **Hash:** `SHA256.HashData`, first 16 bytes, hex.
5. Phone verification, invites, names, aliases (`ApplicationData.Current.LocalSettings`) as above.

### Linux (`apps/linux`)

1. **vCard import:** `Gtk.FileDialog` (GTK 4.10+; goes through the file-chooser portal under Flatpak, so no extra
   permission) for `.vcf`/`.csv`. Parse vCard 3.0 and 4.0 (`TEL`, `TEL;TYPE=cell`, `TEL;VALUE=uri:tel:+…`, line folding,
   quoted-printable 2.1 values) and CSV (Google `Phone N - Value`, Outlook `Mobile Phone`/`Business Phone`).
2. Do not read Evolution Data Server or GNOME Contacts directly (needs extra Flatpak permissions).
3. **E.164:** the `phonenumbers` Python package (Apache-2.0), default region from `LC_TELEPHONE`/`LANG`.
4. **Hash:** `hashlib.sha256(...).digest()[:16].hex()`.
5. Phone verification, invites, names, aliases (`$XDG_DATA_HOME/pigeonpost/aliases.json`, mode 600) as above.

## 10. Account deletion

`deploy/account-deletions.py` removes the account's phone numbers, pending codes, invites it sent, discovery counters and
marks, and its display names. SMS budget rows are kept for cost accounting without the account id. Opt-outs are not linked
to any account and stay.

## Review corrections — 29 October 2026

- Discovery marks are renewed when they expire, even before the reaper removes the old row; repeated checks do not repeatedly spend the daily allowance.
- Web imports use batches of at most 200 numbers and stop at the returned remaining allowance, retaining partial results. Unchecked numbers are never listed as unmatched.
- A missing match may mean a person has not opted into discovery. The UI says “No match found”; it does not claim that person has no account.
- Import disclosure and Play Data safety guidance acknowledge temporary abuse-prevention records. Contact names stay on the device.
