# Postmark sender and DNS for pigeonpost.dev (2026-10-29)

Postmark is now set up to send for `pigeonpost.dev`. The outstanding Cloudflare records are in place, and
`api.pigeonpost.dev` is live as the org API alias (SDS D-26). The mail feature flags `calendar_email` and
`org_invite_email` are **still off**. See "Flags held" below.

## Postmark

- Account: the one that owns server `wodo.io` (id 13343049). That server backs Novu's primary integration,
  `postmark`, whose default From is `robot@wodonetwork.com`.
- Under Sender Signatures, the domain `pigeonpost.dev` was added with domain id 8230542. Its DKIM and
  Return-Path both show **Verified**, and Postmark detects DMARC.
- The From address Pigeonpost uses is `invites@pigeonpost.dev`. It is set in two places:
  - postbox `POSTBOX_CALENDAR_FROM` for the calendar mail;
  - `POSTBOX_NOTIFY_FROM`, which defaults to that address, for org invites.

  The saas_be tenant `pigeonpost` sets no From and no integration (`novu_integration_identifier` is `""`),
  so it uses the primary `postmark` integration. The Pigeonpost workflows put the caller's `from` into the
  Postmark `From` header through `_passthrough`. The verified domain covers every address on it.

## Cloudflare records added (zone pigeonpost.dev)

| Type | Name | Content | Proxy |
|---|---|---|---|
| TXT | `20261029053605pm._domainkey` | `k=rsa;p=MIGf…` (Postmark DKIM, 1024-bit) | DNS only |
| CNAME | `pm-bounces` | `pm.mtasv.net` | DNS only |
| TXT | `_dmarc` | `v=DMARC1; p=none; rua=mailto:info@pigeonpost.dev` | DNS only |
| CNAME | `api` | `postbox.pigeonpost.dev` (same as `mcp`) | DNS only |
| CNAME | `enterprise` | `pigeonpost.dev` (same target as the `*` wildcard that serves inbox and developers) | Proxied |

- No existing record was changed. The 16 records before and the same 16 after compare equal on
  type, name, content, proxy and TTL.
- Backup: `~/Documents/releases/pigeonpost/dns-backup-20261029/` on the owner's Mac, as JSON and a BIND export.
- SPF is unchanged (`v=spf1 include:_spf.mx.cloudflare.net ~all`). Postmark needs no SPF include, because
  the custom Return-Path carries SPF alignment.
- DMARC `rua` is `info@pigeonpost.dev`, an existing Cloudflare routing rule that forwards to the owner's info@wodo.io.
  No `dmarc@` address exists, because the catch-all drops it.
- `turn.pigeonpost.dev` already existed (A `193.164.5.36`, DNS only) and was left as it was.
- Before this change, `enterprise` resolved only through the proxied wildcard. The explicit record keeps the
  same target.
- The records were made through the dashboard UI. Cloudflare's WAF blocks scripted `POST /api/v4` calls from
  the dashboard session ("Sorry, you have been blocked").

## api.pigeonpost.dev on the postbox host (45.132.181.100)

- Backup: `/var/backups/pigeonpost/api-alias-20261029T054216Z/`, holding the `pigeonpost-postbox` vhost and
  the certbot renewal conf.
- `/etc/nginx/sites-available/pigeonpost-postbox`: lines 4 (the :80 block) and 11 (the :443 block) gained
  `api.pigeonpost.dev` in `server_name`. The diff shows only those two lines. `nginx -t` passed before
  each reload.
- The Let's Encrypt cert `postbox.pigeonpost.dev` was expanded by webroot to cover postbox, mcp and api. It
  expires 2027-01-27, and `certbot renew --dry-run` succeeds.

## Verification

- `dig` against 1.1.1.1, 8.8.8.8 and 9.9.9.9:
  - the DKIM TXT matches Postmark's value byte for byte;
  - `pm-bounces` resolves to `pm.mtasv.net`;
  - `_dmarc` and the `api` CNAME are correct;
  - `enterprise` resolves to Cloudflare edge IPs.
- `https://api.pigeonpost.dev/health` returns 200 with a valid LE certificate whose SAN includes api.
  `/org/v1/org` returns 401 in the org-API error schema.
- `https://enterprise.pigeonpost.dev/` returns 200 with a valid Google Trust Services edge certificate
  (`*.pigeonpost.dev`).
- One real test message took the production path: postbox host (its own notify key) → saas_be
  the internal notification route (`POST /v1/internal/notifications/…`) → Novu `pigeonpost-meeting-verify` → Postmark `wodo.io`.
  - Response: 202, and Novu reported "Message sent".
  - Postmark message `69addf77-8c25-453a-b9eb-28be69b11570` showed `From: "Pigeonpost" <invites@pigeonpost.dev>`,
    `X-dkim-options: s=20261029053605pm;d=pigeonpost.dev`, and **Delivered** (Gmail `250 2.0.0 OK`).
  - An earlier attempt was rejected by the Novu bridge before sending, because the test payload lacked
    `event_id`. Nothing was sent that time.

## Flags held: tracking on the wodo.io stream

The `wodo.io` default transactional stream has **open tracking and link tracking on**. The test message showed
"Open Tracking: Enabled, Link Tracking: Enabled for HTML and Text", and the open pixel fired. The Pigeonpost
passthrough sets only `TextBody` and `From`, so these server defaults apply.

The SDS requires the opposite:
- §7.2: org invitations carry "No tracking pixels".
- §14B: guest links must "disable … link tracking". The invite and guest links carry a 256-bit secret in
  the URL fragment, and Postmark click tracking would rewrite those links and store them.

Turning the flags on would therefore break a design requirement on mail sent to outside people. The flags
stay **off** until this is fixed.

**Fix (saas_be, `src/modules/notifications/pigeonpost-novu-workflows.ts`):** add
`TrackOpens: false, TrackLinks: 'None'` to the Postmark `_passthrough.body` of `createRenderedEmailWorkflow`.
Then deploy and re-sync the workflows. Do not change the `wodo.io` server defaults, because other tenants
share that server. After the fix:
1. Send one test, and check that Postmark shows tracking disabled for that message.
2. Run `PUT /internal/v1/features/calendar_email {"scope":"global","enabled":true}`, and the same for
   `org_invite_email`, with the ops token on the internal port.

## Rollback

- **DNS:** delete the five records above in Cloudflare by name. The record ids are in the after-state, and
  the backup JSON lists the original 16. Removing `api` or `enterprise` lets the proxied wildcard take
  those names again.
- **Postmark:** Sender Signatures → `pigeonpost.dev` → Delete. Sends from `invites@pigeonpost.dev` then
  fail with 422 again.
- **api alias:**
  1. Copy `pigeonpost-postbox` from the backup dir back to `/etc/nginx/sites-available/`.
  2. Run `nginx -t` and `systemctl reload nginx`.
  3. Optionally re-issue the certificate without api:
     `certbot certonly --webroot -w /var/www/letsencrypt --cert-name postbox.pigeonpost.dev -d postbox.pigeonpost.dev -d mcp.pigeonpost.dev`.
