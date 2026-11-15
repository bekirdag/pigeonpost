# status.pigeonpost.dev monitor

## Purpose
status.pigeonpost.dev is the one public status page. 25 components in 6 groups, checked every minute from two locations, merged into status.json.

## Architecture
- Location 1 "inside" = raijin. User timer pp-status.timer (user wodo) runs `pp-status.py probe --vantage inside` (~/pp-status/pp-status.py, unit ~/.config/systemd/user/pp-status.service). Checks loopback services (calls 127.0.0.1:8795, whisper 8798, meeting-intel 8797, local model 11452, saas_be 127.0.0.2:3001, store bridge 8787, SFU direct) plus every public endpoint. Writes ~/.local/state/pp-status/inside.json, pushes it with `ssh -i ~/.ssh/pp_status_ed25519 ppstatus@193.164.5.238` (forced command `pp-status.py ingest`, authorized_keys restricted with from="95.70.192.142"). Also writes a single-location fallback /var/www/pigeonpost-status/status.json.
- Location 2 "outside" = nests-1 (193.164.5.238, other provider, no dependency on raijin/wodomini). System timer pp-status-outside.timer runs `pp-status.py run` as user ppstatus: probes public endpoints plus Nests control, Nests model broker, marketplace build plane (10.71.0.1:8795 over wg-nestbuild) and the Postmark server (token via LoadCredential). Merges inside.json (ignored when older than 180 s), writes /var/lib/pp-status/public/status.json, history.json (7 days), alerts.json, pp_status.prom (copied to node-exporter textfile dir; Prometheus rule PigeonpostStatusStale).
- Page: site-status/ (the design kit status-help template, owned by the platforms work) is served by nginx on nests-1 from /var/www/pigeonpost-status with a Let's Encrypt cert (certbot webroot /var/www/letsencrypt, certbot.timer renews). Config: deploy/status/nginx-status.conf.
- Today the Cloudflare record still points at wodomini: wodomini /etc/nginx/sites-available/pigeonpost-status.conf (deploy/status/wodomini-status.conf) terminates TLS and proxies to nests-1 with certificate verification, falling back to raijin's copy on 502/503/504. Pending: change the Cloudflare record status.pigeonpost.dev to A 193.164.5.238, proxied, so wodomini leaves the path. Before changing it, write down the current record (type, content, proxied) and keep it as the rollback. No API token can edit pigeonpost.dev, so this is a dashboard change. Until then nothing breaks: wodomini only proxies.

## Merge rule
- A check run from both locations is "fail" only when both fail; one failing is "split" (can make degraded, never down).
- If the inside report is stale the outside decides alone ("fail1") and needs 5 failed runs in a row instead of 3.
- Checks only one location can run are authoritative.
- States: down = last 3 runs fail (or last 5 fail/fail1); degraded = 2 of last 3 not ok; unknown = no data for 5 minutes.

## Alerts
- One message per run listing changes ("Down: ...", "Recovered: ...").
- Down when entering down; recovered when back to operational.
- More than 4 announcements in an hour marks a component unstable: one line, then quiet until no change for 30 minutes, then "Steady again".
- Sent as a Pigeonpost message from /bekir/alerts (token /etc/nests/alerts.token) to /bekir; Postmark fallback (/etc/credstore/postmark.token, From alerts@pigeonpost.dev, To set in /etc/pp-status/pp-status.env) when the Pigeonpost send fails or Messaging is down.
- Unsent alerts are retried next run. The first run after a reset only learns state.

## Deploy
- `deploy/status/deploy-outside.sh page` and `deploy/status/deploy-outside.sh monitor` (backups under /var/backups/pigeonpost/ on nests-1).
- raijin: rsync pp-status.py to ~/pp-status/ and pp-status.service to ~/.config/systemd/user/, `systemctl --user daemon-reload`.

## Incidents
Write a JSON list to /var/lib/pp-status/incidents.json on nests-1 (fields title, state, text, updated).

## Tests
`python3 deploy/status/test_pp_status.py` (CI runs it).

## Add a component
Append to COMPONENTS in pp-status.py (key, name, group, description, checks); never rename an existing key. Never put hosts, ports or IPs in names/descriptions: status.json is public (a test checks this).
