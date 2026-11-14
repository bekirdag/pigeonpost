#!/usr/bin/env python3
"""status.pigeonpost.dev — one health pass, run every minute by pp-status.timer on raijin.

Checks each component, appends the result to a rolling history and writes the public
status.json the static page reads. No secrets: every check is an unauthenticated request.
"alive" checks count any HTTP answer below 500 as up (those services require sign-in even for
their health route); "health" checks need a 200.

Paths (overridable for tests): PP_STATUS_OUT (status.json), PP_STATUS_STATE (history),
PP_STATUS_INCIDENTS (hand-written incidents.json, optional).
"""
import json
import os
import sys
import time
import urllib.error
import urllib.request

OUT = os.environ.get("PP_STATUS_OUT", "/var/www/pigeonpost-status/status.json")
STATE = os.environ.get("PP_STATUS_STATE", os.path.expanduser("~/.local/state/pp-status/history.json"))
INCIDENTS = os.environ.get("PP_STATUS_INCIDENTS", os.path.expanduser("~/.local/state/pp-status/incidents.json"))
KEEP_SECONDS = 7 * 86400

CHECKS = [
    # key, name, group, kind, url
    ("web", "Website", "Web", "health", "https://pigeonpost.dev/"),
    ("inbox", "Web inbox", "Web", "health", "https://inbox.pigeonpost.dev/"),
    ("account", "Account", "Web", "health", "https://account.pigeonpost.dev/"),
    ("enterprise", "Enterprise", "Web", "health", "https://enterprise.pigeonpost.dev/"),
    ("partner", "Partner", "Web", "health", "https://partner.pigeonpost.dev/"),
    ("developers", "Developer docs", "Web", "health", "https://developers.pigeonpost.dev/"),
    ("postbox", "Messaging API", "Core", "health", "https://postbox.pigeonpost.dev/health"),
    ("registry", "Handle registry", "Core", "health", "https://registry.pigeonpost.dev/health"),
    ("auth", "Sign-in", "Core", "health", "https://auth.pigeonpost.dev/realms/pigeonpost-prod/.well-known/openid-configuration"),
    ("calls", "Calls", "Calls & meetings", "alive", "http://127.0.0.1:8795/health"),
    ("billing", "Payments", "Payments", "health", "http://127.0.0.2:3001/health/ready"),
    ("bridge", "Store and checkout", "Payments", "alive", "http://127.0.0.1:8787/health"),
]


def probe(kind, url, timeout=8):
    start = time.monotonic()
    req = urllib.request.Request(url, headers={"user-agent": "pigeonpost-status/1"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            code = r.status
    except urllib.error.HTTPError as e:
        code = e.code
    except Exception:
        return False, None
    ms = int((time.monotonic() - start) * 1000)
    ok = code == 200 if kind == "health" else code < 500
    return ok, ms


def load(path, default):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return default


def write_atomic(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = f"{path}.tmp"
    with open(tmp, "w") as f:
        json.dump(data, f, separators=(",", ":"))
    os.chmod(tmp, 0o644)
    os.replace(tmp, path)


def uptime(samples, since):
    xs = [s for s in samples if s[0] >= since]
    return None if not xs else round(100.0 * sum(1 for s in xs if s[1]) / len(xs), 2)


def run(now=None, prober=probe):
    now = int(now or time.time())
    history = load(STATE, {})
    components = []
    for key, name, group, kind, url in CHECKS:
        ok, ms = prober(kind, url)
        samples = [s for s in history.get(key, []) if s[0] >= now - KEEP_SECONDS] + [[now, bool(ok), ms]]
        history[key] = samples
        recent = samples[-3:]
        down = sum(1 for s in recent if not s[1])
        # One failed probe in the last three is a blip; two is degraded; three is down.
        state = "down" if down >= 3 else ("degraded" if down == 2 else "operational")
        components.append({
            "key": key, "name": name, "group": group, "state": state,
            "latency_ms": ms,
            "uptime_24h": uptime(samples, now - 86400),
            "uptime_7d": uptime(samples, now - KEEP_SECONDS),
            # 90 one-hour bars for the last 90 hours (newest last): share of good probes, null when none.
            "bars": [uptime([s for s in samples if now - (i + 1) * 3600 < s[0] <= now - i * 3600], 0)
                     for i in range(89, -1, -1)],
        })
    write_atomic(STATE, history)
    worst = "operational"
    for c in components:
        if c["state"] == "down":
            worst = "down"
        elif c["state"] == "degraded" and worst != "down":
            worst = "degraded"
    incidents = load(INCIDENTS, [])
    if not isinstance(incidents, list):
        incidents = []
    out = {"generated_at": now, "overall": worst, "components": components, "incidents": incidents[:20]}
    write_atomic(OUT, out)
    return out


if __name__ == "__main__":
    out = run()
    if "-v" in sys.argv:
        print(out["overall"], " ".join(f'{c["key"]}={c["state"]}' for c in out["components"]))
