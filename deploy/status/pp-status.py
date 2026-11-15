#!/usr/bin/env python3
"""status.pigeonpost.dev monitor: probes from two locations, one merged public status.json.

Two probers run the same file every minute (docs: deploy/status/README.md):

  * inside  (raijin, `pp-status.py probe --vantage inside`): checks what only the main server can
    reach (loopback services) plus every public endpoint, then pushes its report over SSH to the
    outside host, where a forced command runs `pp-status.py ingest`.
  * outside (nests-1, `pp-status.py run`): checks the public endpoints from another provider and
    what only it can reach (Nests, the marketplace build plane over its tunnel, the mail
    provider), merges the latest inside report, writes the public status.json and the history,
    and sends one alert per change.

Merge rule. A check that runs from both locations fails only when both fail ("fail"); one failing
is a "split" (shown as degraded when it persists, never as down). When the other location has not
reported for a while the remaining one decides alone ("fail1"), but needs five failed runs in a
row instead of three. A check that only one location can run is authoritative ("fail").

No secrets reach the public file or this repository: deeper checks read host-local credential files
(systemd LoadCredential), and status.json carries public names only, never hosts, ports or IPs.
"""
import concurrent.futures
import json
import os
import socket
import ssl
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request

KEEP_SECONDS = 7 * 86400
STALE_REPORT = 180  # an inside report older than this is ignored
UA = "Mozilla/5.0 (compatible; pigeonpost-status/2; +https://status.pigeonpost.dev)"
LOCATIONS = {"inside": "Location 1", "outside": "Location 2"}

# (component key, public name, group, description, [(check id, where, kind, target)]).
# where: "both", "inside" or "outside". Keys of the first twelve components predate v2: keep them.
G_WEB, G_MSG, G_CALLS, G_DEV, G_NESTS, G_PAY = (
    "Websites", "Messaging and sign-in", "Calls and meetings", "Agents and developers",
    "Nests and marketplace", "Payments")
COMPONENTS = [
    ("web", "Website", G_WEB, "pigeonpost.dev", [("web", "both", "health", "https://pigeonpost.dev/")]),
    ("inbox", "Web inbox", G_WEB, "inbox.pigeonpost.dev", [("inbox", "both", "health", "https://inbox.pigeonpost.dev/")]),
    ("account", "Account", G_WEB, "account.pigeonpost.dev", [("account", "both", "health", "https://account.pigeonpost.dev/")]),
    ("enterprise", "Enterprise", G_WEB, "enterprise.pigeonpost.dev", [("enterprise", "both", "health", "https://enterprise.pigeonpost.dev/")]),
    ("partner", "Partner portal", G_WEB, "partner.pigeonpost.dev", [("partner", "both", "health", "https://partner.pigeonpost.dev/")]),
    ("management", "Management", G_WEB, "management.pigeonpost.dev", [("management", "both", "health", "https://management.pigeonpost.dev/")]),
    ("help", "Help centre", G_WEB, "help.pigeonpost.dev", [("help", "both", "health", "https://help.pigeonpost.dev/")]),
    ("developers", "Developer docs", G_WEB, "developers.pigeonpost.dev", [("developers", "both", "health", "https://developers.pigeonpost.dev/")]),

    ("postbox", "Messaging", G_MSG, "Sending and receiving messages", [("postbox", "both", "health", "https://postbox.pigeonpost.dev/health")]),
    ("auth", "Sign-in", G_MSG, "Signing in to the apps and websites", [("auth", "both", "health", "https://auth.pigeonpost.dev/realms/pigeonpost-prod/.well-known/openid-configuration")]),
    ("registry", "Handle registry", G_MSG, "Looking up and claiming handles", [("registry", "both", "health", "https://registry.pigeonpost.dev/health")]),
    ("directory", "Directory", G_MSG, "Finding people and agents", [("directory", "both", "health", "https://directory.pigeonpost.dev/health")]),
    ("signin_codes", "Sign-in codes", G_MSG, "Six-digit codes and invitations sent by mail", [
        ("mail_api", "both", "alive", "https://api.postmarkapp.com/"),
        ("mail_account", "outside", "postmark", "https://api.postmarkapp.com/server")]),
    ("push", "Push notifications", G_MSG, "Alerts on phones and desktops", [
        ("push_apple", "both", "tls", "api.push.apple.com:443"),
        ("push_google", "both", "tls", "fcm.googleapis.com:443")]),

    ("calls", "Calls", G_CALLS, "Voice and video calls", [
        ("calls_local", "inside", "alive", "http://127.0.0.1:8795/health"),
        ("calls", "both", "alive", "https://inbox.pigeonpost.dev/calls/health")]),
    ("sfu", "Group calls", G_CALLS, "Calls with more than six people", [
        ("sfu_local", "inside", "health_insecure", "https://193.164.5.36:7443/"),
        ("sfu", "both", "health", "https://inbox.pigeonpost.dev/sfu/")]),
    ("relay", "Call relay", G_CALLS, "Connects calls through strict networks", [("turn", "both", "stun", "193.164.5.36:3478")]),
    ("notes", "Meeting notes", G_CALLS, "Transcripts and summaries of calls", [
        ("notes_speech", "inside", "health", "http://127.0.0.1:8798/health"),
        ("notes_intel", "inside", "alive", "http://127.0.0.1:8797/health"),
        ("notes_model", "inside", "health", "http://127.0.0.1:11452/health")]),

    ("tunnel", "Agent SSH", G_DEV, "SSH access for agents", [("tunnel", "both", "health", "https://tunnel.pigeonpost.dev/healthz")]),
    ("cli", "Command-line tool", G_DEV, "The pigeonpost package on npm", [("cli", "both", "npm", "https://registry.npmjs.org/@bekirdag%2fpigeonpost/latest")]),

    ("nests", "Nests", G_NESTS, "Hosted agent workspaces", [
        ("nests_control", "outside", "alive", "http://10.66.0.2:8790/"),
        ("nests_metrics", "outside", "health", "http://127.0.0.1:9790/metrics")]),
    ("nests_models", "Nests AI models", G_NESTS, "The model gateway Nests use", [("nests_broker", "outside", "alive", "http://10.67.255.1:8788/")]),
    ("marketplace_build", "Marketplace build, scan and test", G_NESTS, "Building, checking and testing marketplace apps", [
        ("buildd", "outside", "health", "http://10.71.0.1:8795/healthz")]),

    ("billing", "Payments", G_PAY, "Plans, credits and invoices", [
        ("billing_local", "inside", "health", "http://127.0.0.2:3001/health/ready"),
        ("billing", "both", "health", "https://app-pigeonpost.masaas.org/saas-api/health/ready")]),
    ("bridge", "Store and checkout", G_PAY, "Buying in the apps and on the web", [
        ("bridge_local", "inside", "health", "http://127.0.0.1:8787/healthz"),
        ("bridge", "both", "health", "https://pigeonpost.dev/api/healthz")]),
]
CHECK_IDS = {c[0] for comp in COMPONENTS for c in comp[4]}


# ---------------------------------------------------------------- probes -----------------------

def _http(url, timeout, insecure=False, headers=None):
    req = urllib.request.Request(url, headers={"user-agent": UA, **(headers or {})})
    ctx = None
    if insecure:  # the media host's own certificate is self-signed (raijin pins it in nginx)
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=ctx) as r:
            return r.status, r.read(65536)
    except urllib.error.HTTPError as e:
        return e.code, b""


def _stun(target, timeout):
    host, port = target.rsplit(":", 1)
    tid = os.urandom(12)
    req = struct.pack("!HHI", 0x0001, 0, 0x2112A442) + tid  # RFC 5389 Binding Request
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.settimeout(timeout / 2)
        for _ in range(2):
            s.sendto(req, (host, int(port)))
            try:
                data, _ = s.recvfrom(2048)
            except socket.timeout:
                continue
            if len(data) >= 20 and data[0:2] == b"\x01\x01" and data[8:20] == tid:
                return True
    return False


def _tls(target, timeout):
    host, port = target.rsplit(":", 1)
    with socket.create_connection((host, int(port)), timeout=timeout) as raw:
        with ssl.create_default_context().wrap_socket(raw, server_hostname=host):
            return True


def _credential(name):
    d = os.environ.get("CREDENTIALS_DIRECTORY")
    if not d:
        return None
    try:
        with open(os.path.join(d, name)) as f:
            return f.read().strip() or None
    except OSError:
        return None


def probe(kind, target, timeout=8):
    """One check. Returns (ok, ms); ok is None when the check cannot run here (no credential)."""
    start = time.monotonic()
    try:
        if kind in ("health", "health_insecure"):
            code, _ = _http(target, timeout, insecure=kind == "health_insecure")
            ok = code == 200
        elif kind == "alive":  # services that want sign-in even for /health: any answer below 500
            code, _ = _http(target, timeout)
            ok = code < 500
        elif kind == "npm":
            code, body = _http(target, timeout)
            ok = code == 200 and bool(json.loads(body or b"{}").get("version"))
        elif kind == "postmark":
            token = _credential("postmark.token")
            if not token:
                return None, None
            code, _ = _http(target, timeout, headers={"accept": "application/json", "x-postmark-server-token": token})
            ok = code == 200
        elif kind == "stun":
            ok = _stun(target, timeout)
        elif kind == "tls":
            ok = _tls(target, timeout)
        else:
            raise ValueError(kind)
    except Exception:
        return False, None
    return bool(ok), int((time.monotonic() - start) * 1000)


def probe_all(vantage, prober=probe):
    jobs = [(cid, kind, target) for comp in COMPONENTS for cid, where, kind, target in comp[4]
            if where in ("both", vantage)]
    out = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=12) as pool:
        futs = {pool.submit(prober, kind, target): cid for cid, kind, target in jobs}
        for f in concurrent.futures.as_completed(futs):
            ok, ms = f.result()
            if ok is not None:
                out[futs[f]] = {"ok": ok, "ms": ms}
    return out


# ---------------------------------------------------------------- merge -----------------------

SEVERITY = {"ok": 0, "split": 1, "fail1": 2, "fail": 3}


def merge_sample(comp, outside, inside):
    """One run's verdict for a component: (sample, locations, latency_ms).

    outside/inside: {check id: {"ok", "ms"}} for this run, or None when that location has no fresh
    report. sample is "ok" | "split" | "fail1" | "fail" | None (no data)."""
    worst, seen, ms = None, set(), None
    for cid, where, _kind, _target in comp[4]:
        res = {}
        for v, rep in (("outside", outside), ("inside", inside)):
            if rep is not None and where in ("both", v) and cid in rep:
                res[v] = rep[cid]
        if not res:
            continue
        seen.update(res)
        if ms is None:
            ms = (res.get("outside") or res.get("inside"))["ms"]
        fails = [v for v, r in res.items() if not r["ok"]]
        if where != "both":
            s = "fail" if fails else "ok"  # only this location can see it: authoritative
        elif len(res) == 2:
            s = "fail" if len(fails) == 2 else ("split" if fails else "ok")
        else:
            s = "fail1" if fails else "ok"  # the other location is not reporting
        if worst is None or SEVERITY[s] > SEVERITY[worst]:
            worst = s
    return worst, sorted(seen), ms


def _code(sample):
    """History entries are [t, ok, ms] (v1) or [t, ok, ms, code] (v2)."""
    if len(sample) > 3 and sample[3]:
        return sample[3]
    return "ok" if sample[1] else "fail"


def state_of(samples, now):
    """operational | degraded | down | unknown from the recent history (newest last)."""
    recent = [s for s in samples if s[0] > now - 300]
    if not recent:
        return "unknown"
    codes = [_code(s) for s in samples[-5:]]
    last3 = codes[-3:]
    if len(last3) == 3 and all(c == "fail" for c in last3):
        return "down"
    if len(codes) == 5 and all(c in ("fail", "fail1") for c in codes):
        return "down"
    if sum(1 for c in last3 if c != "ok") >= 2:
        return "degraded"
    return "operational"


def uptime(samples, since):
    xs = [s for s in samples if s[0] >= since]
    return None if not xs else round(100.0 * sum(1 for s in xs if s[1]) / len(xs), 2)


def build(now, history, outside, inside, incidents=()):
    """Append this run to history (in place) and return the public status document."""
    components = []
    for comp in COMPONENTS:
        key, name, group, desc, _checks = comp
        sample, seen, ms = merge_sample(comp, outside, inside)
        samples = [s for s in history.get(key, []) if s[0] >= now - KEEP_SECONDS]
        if sample is not None:
            samples.append([now, sample in ("ok", "split"), ms, sample])
        history[key] = samples
        state = state_of(samples, now)
        components.append({
            "key": key, "name": name, "group": group, "description": desc, "state": state,
            "latency_ms": ms,
            "checked_from": len(seen),
            "uptime_24h": uptime(samples, now - 86400),
            "uptime_7d": uptime(samples, now - KEEP_SECONDS),
            # 90 one-hour bars for the last 90 hours (newest last): share of good runs, null when none.
            "bars": [uptime([s for s in samples if now - (i + 1) * 3600 < s[0] <= now - i * 3600], 0)
                     for i in range(89, -1, -1)],
        })
    rank = {"operational": 0, "unknown": 1, "degraded": 1, "down": 2}
    worst = max(components, key=lambda c: rank[c["state"]])["state"] if components else "operational"
    overall = {"unknown": "degraded"}.get(worst, worst)
    live = [v for v, rep in (("inside", inside), ("outside", outside)) if rep is not None]
    return {
        "schema": 2,
        "generated_at": now, "overall": overall, "components": components,
        "incidents": list(incidents)[:20],
        "checked_from": len(live),
        "locations": [{"key": v, "name": LOCATIONS[v], "reporting": v in live} for v in ("inside", "outside")],
    }


# ---------------------------------------------------------------- alerts ----------------------

FLAP_WINDOW, FLAP_EVENTS, FLAP_QUIET = 3600, 4, 1800


def alert_events(status, astate, now):
    """Compare component states with what was last announced; return the lines of one message.

    A component is announced down when it enters "down" and recovered when it is back to
    "operational" (degraded in between announces nothing). More than FLAP_EVENTS announcements in
    FLAP_WINDOW mark it unstable: one line says so and it stays quiet until it has not changed for
    FLAP_QUIET, then one line gives its steady state. astate is updated in place."""
    down, up, flapping, steady = [], [], [], []
    comps = list(status["components"])
    comps.append({"key": "_inside", "name": "Location 1 checks",
                  "state": "operational" if status["locations"][0]["reporting"] else "down"})
    for c in comps:
        st = astate.setdefault(c["key"], {"down": False, "events": [], "flapping": False, "changed": now})
        st["events"] = [t for t in st["events"] if t > now - FLAP_WINDOW]
        if c["state"] == "down" and not st["down"]:
            event = "down"
        elif c["state"] == "operational" and st["down"]:
            event = "up"
        else:
            event = None
        if event:
            st["down"] = event == "down"
            st["changed"] = now
            st["events"].append(now)
            if st["flapping"]:
                continue
            if len(st["events"]) > FLAP_EVENTS:
                st["flapping"] = True
                flapping.append(c["name"])
                continue
            (down if event == "down" else up).append(c["name"])
        elif st["flapping"] and now - st["changed"] >= FLAP_QUIET:
            st["flapping"] = False
            st["events"] = []
            steady.append(f'{c["name"]} ({"down" if st["down"] else "operational"})')
    lines = []
    if down:
        lines.append("Down: " + ", ".join(down))
    if up:
        lines.append("Recovered: " + ", ".join(up))
    if flapping:
        lines.append("Unstable, alerts paused: " + ", ".join(flapping))
    if steady:
        lines.append("Steady again: " + ", ".join(steady))
    return lines


def send_alert(lines, mention_postbox, post=None):
    """One Pigeonpost message to the staff mailbox; Postmark when that fails or the postbox is down.
    Returns the channels that accepted it."""
    post = post or _post_json
    text = "Pigeonpost status\n" + "\n".join(lines) + "\nhttps://status.pigeonpost.dev"
    sent = []
    token, to = _credential("alerts.token"), os.environ.get("PP_STATUS_ALERT_TO")
    if token and to:
        url = os.environ.get("PP_STATUS_POSTBOX", "https://postbox.pigeonpost.dev") + "/v1/send"
        if post(url, {"to": to, "body": text}, {"authorization": f"Bearer {token}"}):
            sent.append("pigeonpost")
    pm, pm_to = _credential("postmark.token"), os.environ.get("PP_STATUS_FALLBACK_TO")
    if pm and pm_to and (not sent or mention_postbox):
        body = {"From": os.environ.get("PP_STATUS_FALLBACK_FROM", "alerts@pigeonpost.dev"), "To": pm_to,
                "Subject": ("Pigeonpost status: " + "; ".join(lines))[:200], "TextBody": text,
                "MessageStream": "outbound"}
        if post("https://api.postmarkapp.com/email", body,
                {"accept": "application/json", "x-postmark-server-token": pm}):
            sent.append("postmark")
    return sent


def _post_json(url, body, headers):
    req = urllib.request.Request(url, data=json.dumps(body).encode(), method="POST",
                                 headers={"content-type": "application/json", "user-agent": UA, **headers})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return 200 <= r.status < 300
    except Exception:
        return False


# ---------------------------------------------------------------- files -----------------------

def load(path, default):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return default


def write_atomic(path, data, mode=0o644):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = f"{path}.tmp"
    with open(tmp, "w") as f:
        json.dump(data, f, separators=(",", ":"))
    os.chmod(tmp, mode)
    os.replace(tmp, path)


def env_path(name, default):
    return os.environ.get(name, default)


def validate_report(doc, now):
    """An inside report as pushed over SSH; returns the cleaned results or raises ValueError."""
    if not isinstance(doc, dict) or doc.get("vantage") != "inside":
        raise ValueError("not an inside report")
    t = doc.get("generated_at")
    if not isinstance(t, int) or abs(now - t) > 600:
        raise ValueError("clock")
    results = doc.get("results")
    if not isinstance(results, dict):
        raise ValueError("results")
    clean = {}
    for cid, r in results.items():
        if cid not in CHECK_IDS or not isinstance(r, dict) or not isinstance(r.get("ok"), bool):
            continue
        ms = r.get("ms")
        clean[cid] = {"ok": r["ok"], "ms": ms if isinstance(ms, int) and 0 <= ms < 120000 else None}
    return {"vantage": "inside", "generated_at": t, "received_at": now, "results": clean}


# ---------------------------------------------------------------- commands --------------------

STATE_DIR = env_path("PP_STATUS_STATE_DIR", "/var/lib/pp-status")


def cmd_probe(vantage):
    """inside: probe, write the report, push it to the outside host."""
    now = int(time.time())
    report = {"vantage": vantage, "generated_at": now, "results": probe_all(vantage)}
    path = env_path("PP_STATUS_REPORT", os.path.expanduser("~/.local/state/pp-status/inside.json"))
    write_atomic(path, report, 0o600)
    out = os.environ.get("PP_STATUS_OUT")
    if out:  # this location's own view, served only when the outside host does not answer
        hpath = os.path.join(os.path.dirname(path), "history.json")
        history = load(hpath, {})
        status = build(now, history, None, report["results"])
        write_atomic(hpath, history, 0o600)
        write_atomic(out, status)
    target = os.environ.get("PP_STATUS_PUSH")
    if target:
        key = os.path.expanduser(env_path("PP_STATUS_PUSH_KEY", "~/.ssh/pp_status_ed25519"))
        with open(path, "rb") as f:
            subprocess.run(["ssh", "-i", key, "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
                            "-o", "IdentitiesOnly=yes", target, "ingest"],
                           stdin=f, check=True, timeout=30, stdout=subprocess.DEVNULL)
    return report


def cmd_ingest():
    """outside, as the SSH forced command: accept one inside report on stdin."""
    raw = sys.stdin.buffer.read(65537)
    if len(raw) > 65536:
        raise SystemExit("too large")
    report = validate_report(json.loads(raw), int(time.time()))
    write_atomic(os.path.join(STATE_DIR, "inside.json"), report, 0o640)


def cmd_run(now=None, prober=probe, post=None):
    """outside: probe, merge the inside report, write status.json, alert on changes."""
    now = int(now or time.time())
    outside = probe_all("outside", prober)
    rep = load(os.path.join(STATE_DIR, "inside.json"), None)
    inside = rep["results"] if rep and now - rep.get("received_at", 0) <= STALE_REPORT else None
    history = load(os.path.join(STATE_DIR, "history.json"), {})
    incidents = load(os.path.join(STATE_DIR, "incidents.json"), [])
    status = build(now, history, outside, inside, incidents if isinstance(incidents, list) else [])
    write_atomic(os.path.join(STATE_DIR, "history.json"), history, 0o640)
    write_atomic(env_path("PP_STATUS_OUT", os.path.join(STATE_DIR, "public", "status.json")), status)
    astate = load(os.path.join(STATE_DIR, "alerts.json"), {})
    first = not astate
    lines = alert_events(status, astate, now)
    if first:  # the first run only learns the current state
        lines = []
    pending = astate.pop("_pending", [])
    if lines or pending:
        lines = (pending + lines)[-20:]  # retry what no channel accepted last time, in one message
        down_keys = {c["key"] for c in status["components"] if c["state"] == "down"}
        via = send_alert(lines, "postbox" in down_keys, post)
        astate["_last_send"] = {"t": now, "via": via}
        if not via:
            astate["_pending"] = lines
    write_atomic(os.path.join(STATE_DIR, "alerts.json"), astate, 0o640)
    with open(os.path.join(STATE_DIR, "pp_status.prom.tmp"), "w") as f:
        f.write(f"pp_status_last_run_seconds {now}\npp_status_inside_reporting {int(inside is not None)}\n")
    os.replace(os.path.join(STATE_DIR, "pp_status.prom.tmp"), os.path.join(STATE_DIR, "pp_status.prom"))
    return status, lines


if __name__ == "__main__":
    args = sys.argv[1:]
    cmd = args[0] if args else "run"
    if cmd == "probe":
        vantage = args[args.index("--vantage") + 1] if "--vantage" in args else "inside"
        rep = cmd_probe(vantage)
        bad = sorted(k for k, r in rep["results"].items() if not r["ok"])
        print(f"{len(rep['results'])} checks, failing: {' '.join(bad) or 'none'}")
    elif cmd == "ingest":
        cmd_ingest()
    elif cmd == "run":
        status, lines = cmd_run()
        print(status["overall"], f"from {status['checked_from']} locations;",
              " ".join(f'{c["key"]}={c["state"]}' for c in status["components"] if c["state"] != "operational") or "all operational",
              "| alert: " + "; ".join(lines) if lines else "")
    else:
        raise SystemExit(f"unknown command {cmd}")
