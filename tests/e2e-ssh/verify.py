#!/usr/bin/env python3
"""Loopback end-to-end check of agent SSH (SDS §11) with the real binaries: a postbox, the tunnel
broker, `pigeonpost agentd run` as the agent's machine and `pigeonpost ssh` as the person.

    python3 verify.py --postbox target/debug/pigeonpost-postbox \
                      --tunnel target/debug/pigeonpost-tunnel --cli target/debug/pigeonpost

Everything is synthetic and local: a throwaway OIDC issuer (an RSA key made for this run), a fresh
database, an SSH CA made for this run, workspaces in a temporary directory, all sockets on
127.0.0.1. A recording TCP proxy sits between both SSH ends and the broker, so the run can show
what crossed the broker: the SSH version banners and ciphertext, never what was typed or printed.

Container mode is exercised when `docker info` works on this machine (it does on CI's Linux
runners); otherwise that part is reported as skipped, never as passed.

Requires Python 3.9+ and the `cryptography` package.
"""

import argparse
import base64
import http.server
import json
import os
import pty
import secrets
import select
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, padding, rsa


def b64u(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Issuer:
    def __init__(self):
        self.key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        nums = self.key.public_key().public_numbers()
        jwk = {"kty": "RSA", "kid": "fixture", "alg": "RS256", "use": "sig",
               "n": b64u(nums.n.to_bytes((nums.n.bit_length() + 7) // 8, "big")),
               "e": b64u(nums.e.to_bytes(3, "big"))}
        body = json.dumps({"keys": [jwk]}).encode()

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802
                if self.path.endswith("/protocol/openid-connect/certs"):
                    self.send_response(200)
                    self.send_header("content-type", "application/json")
                    self.end_headers()
                    self.wfile.write(body)
                else:
                    self.send_response(404)
                    self.end_headers()

            def log_message(self, *args):
                pass

        self.port = free_port()
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", self.port), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.port}/realms/fixture"

    def token(self, subject: str, strong: bool) -> str:
        now = int(time.time())
        header = b64u(json.dumps({"alg": "RS256", "typ": "JWT", "kid": "fixture"}).encode())
        claims = {"iss": self.url, "sub": subject, "iat": now, "exp": now + 3600, "auth_time": now,
                  "azp": "pigeonpost-cli", "typ": "Bearer", "aud": ["account"],
                  "preferred_username": subject}
        if strong:
            claims["acr"] = "fixture-mfa"
        payload = b64u(json.dumps(claims).encode())
        sig = self.key.sign(f"{header}.{payload}".encode(), padding.PKCS1v15(), hashes.SHA256())
        return f"{header}.{payload}.{b64u(sig)}"


class Capture:
    """A TCP proxy that records every byte in both directions."""

    def __init__(self, target_port: int):
        self.target = target_port
        self.data = bytearray()
        self.lock = threading.Lock()
        self.port = free_port()
        self.sock = socket.socket()
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind(("127.0.0.1", self.port))
        self.sock.listen(64)
        threading.Thread(target=self.accept, daemon=True).start()

    def accept(self):
        while True:
            try:
                c, _ = self.sock.accept()
            except OSError:
                return
            try:
                u = socket.create_connection(("127.0.0.1", self.target))
            except OSError:
                c.close()
                continue
            for a, b in ((c, u), (u, c)):
                threading.Thread(target=self.pipe, args=(a, b), daemon=True).start()

    def pipe(self, a, b):
        try:
            while True:
                chunk = a.recv(65536)
                if not chunk:
                    break
                with self.lock:
                    self.data.extend(chunk)
                b.sendall(chunk)
        except OSError:
            pass
        finally:
            for s in (a, b):
                try:
                    s.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

    def snapshot(self) -> bytes:
        with self.lock:
            return bytes(self.data)


class Checks:
    def __init__(self):
        self.passed = 0
        self.skipped = []

    def ok(self, cond, what):
        if not cond:
            raise SystemExit(f"FAIL: {what}")
        self.passed += 1
        print(f"  ok  {what}", flush=True)

    def skip(self, what):
        self.skipped.append(what)
        print(f"  --  SKIPPED {what}", flush=True)


def call(base, method, path, token=None, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(base + path, data=data, method=method)
    if token:
        req.add_header("authorization", f"Bearer {token}")
    if data is not None:
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            status, raw = r.status, r.read()
    except urllib.error.HTTPError as e:
        status, raw = e.code, e.read()
    try:
        return status, json.loads(raw or b"null")
    except ValueError:
        return status, raw.decode(errors="replace")


def wait_for(what, fn, timeout=30.0, every=0.25):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(every)
    raise SystemExit(f"FAIL: timed out waiting for {what}")


def private_dir(path):
    os.makedirs(path, exist_ok=True)
    os.chmod(path, 0o700)
    return path


def write_private(path, text):
    with open(path, "w") as f:
        f.write(text)
    os.chmod(path, 0o600)


def docker_ok() -> bool:
    try:
        return subprocess.run(["docker", "info"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                              timeout=20).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--postbox", required=True)
    ap.add_argument("--tunnel", required=True)
    ap.add_argument("--cli", required=True)
    ap.add_argument("--no-container", action="store_true", help="skip container mode even if docker works")
    args = ap.parse_args()
    for b in (args.postbox, args.tunnel, args.cli):
        if not os.access(b, os.X_OK):
            raise SystemExit(f"not executable: {b}")

    work = tempfile.mkdtemp(prefix="pp-e2e-ssh-")
    os.chmod(work, 0o700)
    procs = []
    containers_used = False
    try:
        c = Checks()
        print("agent SSH end-to-end (synthetic fixtures, loopback only)", flush=True)
        issuer = Issuer()
        port, internal_port, broker_port = free_port(), free_port(), free_port()
        base, internal = f"http://127.0.0.1:{port}", f"http://127.0.0.1:{internal_port}"
        ops = "svc_ops_" + secrets.token_hex(20)
        tunnel_token = "svc_tunnel_" + secrets.token_hex(20)
        capture = Capture(broker_port)
        tunnel_url = f"ws://127.0.0.1:{capture.port}"

        # The CA the way production makes it: ssh-keygen, owner-only, no passphrase.
        ca_path = os.path.join(work, "ssh_ca")
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "", "-f", ca_path], check=True)
        os.chmod(ca_path, 0o600)
        with open(os.path.join(work, "kms"), "wb") as f:
            f.write(secrets.token_bytes(32))
        env = {
            "RUST_LOG": "warn", "POSTBOX_BIND": f"127.0.0.1:{port}",
            "POSTBOX_INTERNAL_BIND": f"127.0.0.1:{internal_port}", "POSTBOX_PUBLIC_URL": base,
            "POSTBOX_DB_PATH": f"{work}/postbox.db", "POSTBOX_KMS": f"sealed-file:{work}/kms",
            "POSTBOX_LOFT_DIR": f"{work}/loft", "PIGEONPOST_BLOB_DIR": f"{work}/blobs",
            "PIGEONPOST_BLOB_MIN_FREE_MB": "1", "REGISTRY_URL": f"http://127.0.0.1:{issuer.port}",
            "OIDC_ISSUER": issuer.url, "POSTBOX_SVC_TOKEN_OPS": ops, "POSTBOX_SVC_TOKEN_TUNNEL": tunnel_token,
            "POSTBOX_STEP_UP_ACR_VALUES": "fixture-mfa", "POSTBOX_SSH_CA_KEY": ca_path,
            "POSTBOX_SHELL_BROKER_URL": tunnel_url,
        }
        procs.append(subprocess.Popen([args.postbox], env={"PATH": "/usr/bin:/bin", **env},
                                      stdout=subprocess.DEVNULL, stderr=open(f"{work}/postbox.log", "w")))
        wait_for("the postbox", lambda: _health(base))
        procs.append(subprocess.Popen([args.tunnel], env={
            "PATH": "/usr/bin:/bin", "RUST_LOG": "info", "TUNNEL_BIND": f"127.0.0.1:{broker_port}",
            "TUNNEL_POSTBOX_INTERNAL_URL": internal, "TUNNEL_SERVICE_TOKEN": tunnel_token,
            "TUNNEL_IDLE_SECS": "10", "TUNNEL_CHECK_SECS": "1", "TUNNEL_PER_IP_PER_MINUTE": "1000"}, stdout=subprocess.DEVNULL,
            stderr=open(f"{work}/tunnel.log", "w")))
        wait_for("the broker", lambda: _health(f"http://127.0.0.1:{broker_port}", "/healthz"))
        for flag in ("agent_ssh",):
            s, _ = call(internal, "PUT", f"/internal/v1/features/{flag}", ops, {"scope": "global", "enabled": True})
            c.ok(s == 200, f"flag {flag} on")

        owner = f"p8-owner-{secrets.token_hex(3)}"
        weak, strong = issuer.token(owner, False), issuer.token(owner, True)
        stranger = issuer.token("p8-stranger", True)

        # --- the agent machine -----------------------------------------------------------------
        machine = private_dir(os.path.join(work, "machine"))
        agents = {}
        modes = [("full", "unconfined"), ("tight", "allowlist")]
        use_container = not args.no_container and docker_ok()
        if use_container:
            modes.append(("boxed", "container"))
        idents = []
        for name, mode in modes:
            s, mb = call(base, "POST", "/v1/identities", weak, {"label": name})
            c.ok(s in (200, 201), f"agent mailbox for {mode} mode")
            ws = os.path.join(work, f"ws-{name}")
            os.makedirs(ws)
            with open(os.path.join(ws, "notes.txt"), "w") as f:
                f.write("workspace notes\n")
            agents[mode] = {"address": mb["address"], "token": mb["capability_token"], "ws": os.path.realpath(ws)}
            idents.append({"base_url": base, "address": mb["address"], "capability_token": mb["capability_token"],
                           "label": name, "created_at": int(time.time())})
        with open(os.path.join(work, "secret.txt"), "w") as f:
            f.write("outside the workspace\n")
        os.symlink(os.path.join(work, "secret.txt"), os.path.join(agents["allowlist"]["ws"], "escape"))
        write_private(os.path.join(machine, "postbox.json"), json.dumps({"identities": idents}))
        toml = []
        for mode, a in agents.items():
            toml.append(f'[[mailbox]]\naddress = "{a["address"]}"\nworkspace = "{a["ws"]}"\n[mailbox.shell]\n'
                        f'enabled = true\nmode = "{mode}"\nmax_sessions = 2\n')
            if mode == "unconfined":
                toml.append("unconfined_acknowledged = true\n")
            if mode == "allowlist":
                toml.append('[[mailbox.shell.command]]\nargv = ["/bin/cat"]\npaths = 1\n'
                            '[[mailbox.shell.command]]\nargv = ["/bin/echo", "allowed"]\n')
            if mode == "container":
                toml.append('image = "docker.io/library/debian:13"\nruntime = "docker"\n')
        write_private(os.path.join(machine, "agentd.toml"), "\n".join(toml))
        agent_env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": machine,
                     "PIGEONPOST_HOME": machine, "PIGEONPOST_POSTBOX": base, "PIGEONPOST_TUNNEL_URL": tunnel_url,
                     "PIGEONPOST_AGENTD_NOTIFY": "never", "PP_AGENTD_SECRET": "agentd-env-must-not-leak",
                     "SHELL": "/bin/sh"}
        if use_container:
            # Where this machine's Docker listens (Docker Desktop keeps it in a context).
            host = subprocess.run(["docker", "context", "inspect", "--format", "{{.Endpoints.docker.Host}}"],
                                  capture_output=True, text=True).stdout.strip()
            if host:
                agent_env["DOCKER_HOST"] = host

        def start_agentd():
            p = subprocess.Popen([args.cli, "agentd", "run"], env=agent_env, stdout=subprocess.DEVNULL,
                                 stderr=open(f"{work}/agentd.log", "a"))
            procs.append(p)
            return p

        agentd = start_agentd()
        for mode, a in agents.items():
            st = wait_for(f"{mode} agent connected", lambda a=a: _connected(base, strong, a["address"]), timeout=60)
            c.ok(st["mode"] == mode and st["can_open"], f"{mode} agent connected, offered and openable by its owner")
        status = json.loads(subprocess.run([args.cli, "--json", "agentd", "shell"], env=agent_env, check=True,
                                           capture_output=True, text=True).stdout)
        host_fp = status["host_key_fingerprint"]
        c.ok(all(m["ok"] for m in status["mailboxes"]), "agentd shell status: every configured shell is valid")

        # --- the person's machine ----------------------------------------------------------------
        def client_home(name, token):
            h = private_dir(os.path.join(work, name))
            write_private(os.path.join(h, "auth.json"), json.dumps({
                "issuer": issuer.url, "refresh_token": "unused", "access_token": token,
                "expires_at": int(time.time()) + 3000}))
            return h

        me = client_home("me", strong)
        cli_env = lambda home: {"PATH": "/usr/bin:/bin", "HOME": home, "PIGEONPOST_HOME": home,
                                "PIGEONPOST_POSTBOX": base, "PIGEONPOST_TUNNEL_URL": tunnel_url}

        def ssh(home, agent, *cmd, extra=(), timeout=60):
            return subprocess.run([args.cli, "ssh", agent, *extra, "--", *cmd] if cmd else
                                  [args.cli, "ssh", agent, *extra], env=cli_env(home), capture_output=True,
                                  text=True, timeout=timeout, stdin=subprocess.DEVNULL)

        full = agents["unconfined"]
        r = ssh(me, full["address"], "echo", "PPMARK-$((6*7))", "&&", "pwd", "&&", "env")
        c.ok(r.returncode == 0 and "PPMARK-42" in r.stdout, "a command runs on the agent over SSH")
        c.ok(full["ws"] in r.stdout.splitlines()[1], "it runs in the agent's workspace")
        c.ok("agentd-env-must-not-leak" not in r.stdout, "agentd's own environment does not reach the session")
        c.ok(host_fp in r.stderr, f"first use pins the host key the agent machine shows ({host_fp})")

        # An interactive terminal.
        out = interactive(args.cli, cli_env(me), full["address"],
                          [b"echo PPTTY-$((3+4))\n", b"tty\n", b"exit 3\n"], b"PPTTY-7")
        c.ok(b"PPTTY-7" in out[0] and b"/dev/" in out[0], "an interactive shell with a real terminal")
        c.ok(out[1] == 3, "the remote exit status comes back")

        # The broker saw ciphertext only.
        cap = capture.snapshot()
        c.ok(cap.count(b"SSH-2.0-") >= 2 and len(cap) > 4000, f"the capture holds the sessions ({len(cap)} bytes)")
        for plain in (b"PPMARK", b"PPTTY", b"echo ", b"workspace notes", full["ws"].encode(), b"agentd-env"):
            c.ok(plain not in cap, f"the broker never saw {plain[:24]!r}")

        # --- allowlist mode: escapes over real SSH ------------------------------------------------
        tight = agents["allowlist"]
        r = ssh(me, tight["address"], "cat", "notes.txt")
        c.ok(r.returncode == 0 and r.stdout == "workspace notes\n", "allowlist: an allowed command with a workspace path")
        for attempt in (["cat", "../secret.txt"], ["cat", "escape"], ["cat", os.path.join(work, "secret.txt")],
                        ["echo", "allowed;", "id"], ["echo", "allowed", "&&", "id"], ["sh", "-c", "id"],
                        ["cat", "notes.txt", "|", "sh"], ["LD_PRELOAD=/tmp/x.so", "cat", "notes.txt"]):
            r = ssh(me, tight["address"], *attempt)
            c.ok(r.returncode == 126 and "refused" in r.stderr and "outside the workspace" not in r.stdout,
                 f"allowlist refuses {' '.join(attempt)!r}")
        r = ssh(me, tight["address"])
        c.ok(r.returncode == 126 and "/bin/echo allowed" in r.stderr, "allowlist: no shell, and the list of what is allowed")

        # --- container mode -----------------------------------------------------------------------
        if use_container:
            containers_used = True
            boxed = agents["container"]
            r = ssh(me, boxed["address"], "id -u; hostname; ls /workspace; cat /workspace/notes.txt; "
                    "ls /sys/class/net; touch /etc/x 2>&1; ls " + machine + " 2>&1", timeout=300)
            lines = r.stdout
            c.ok(r.returncode in (0, 1, 2) and "pigeonpost-shell" in lines and "workspace notes" in lines,
                 f"container: the workspace is mounted at /workspace [{r.returncode}] {lines[-400:]} {r.stderr[-600:]}")
            c.ok(lines.splitlines()[0].strip() != "0", "container: not root")
            c.ok("eth0" not in lines, "container: no network")
            c.ok("Read-only" in lines or "read-only" in lines.lower(), "container: read-only root filesystem")
            c.ok("No such file" in lines, "container: the agent machine's Pigeonpost home is not there")
            leftover = subprocess.run(["docker", "ps", "-aq", "--filter", "label=dev.pigeonpost.shell=1"],
                                      capture_output=True, text=True).stdout.strip()
            c.ok(leftover == "", "container: removed when the session ended")
        else:
            c.skip("container mode (docker is not available here)")

        # --- authorization at the client ---------------------------------------------------------
        weakhome = client_home("weak", weak)
        r = ssh(weakhome, full["address"], "true", timeout=20)
        c.ok(r.returncode != 0 and "step-up" not in r.stdout, "without a recent strong sign-in no session opens")
        other = client_home("other", stranger)
        r = ssh(other, full["address"], "true")
        c.ok(r.returncode != 0 and "no_agent" in r.stderr, "someone else's agent: not even its existence")

        # --- revocation and kill ------------------------------------------------------------------
        p = subprocess.Popen([args.cli, "ssh", full["address"], "--", "sleep", "33333"], env=cli_env(me),
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, stdin=subprocess.DEVNULL)
        sid = wait_for("a connected session", lambda: _session(base, strong, "connected"))
        c.ok(_pgrep("sleep 33333"), "the long-running command is running on the agent")
        s, _ = call(base, "DELETE", f"/v1/shell/sessions/{sid}", weak)
        c.ok(s == 200, "the owner ends the session")
        try:
            p.wait(timeout=20)
        except subprocess.TimeoutExpired:
            p.kill()
            raise SystemExit("FAIL: the client did not end after the kill")
        c.ok(p.returncode != 0, "the client's session ends")
        wait_for("the process group to go", lambda: not _pgrep("sleep 33333"), timeout=15)
        c.ok(True, "teardown killed the session's processes on the agent")

        # Idle: the broker ends a silent session (TUNNEL_IDLE_SECS=10 here; 900 in production).
        t0 = time.time()
        r = ssh(me, full["address"], "sleep", "45", timeout=60)
        took = time.time() - t0
        c.ok(r.returncode != 0 and took < 40 and "idle" in r.stderr, f"an idle session ends ({took:.0f}s)")
        wait_for("the idle session's processes to go", lambda: not _pgrep("sleep 45"), timeout=15)

        # --- host keys ------------------------------------------------------------------------------
        # A directory (or broker) lying about the host key: the client pins what it is told, and the
        # real agent's key then fails the SSH handshake. Nothing authenticates.
        fake = ed25519.Ed25519PrivateKey.generate().public_key().public_bytes(
            serialization.Encoding.OpenSSH, serialization.PublicFormat.OpenSSH).decode()
        fake_fp = "SHA256:" + base64.b64encode(_sha256(base64.b64decode(fake.split()[1]))).decode().rstrip("=")
        db = sqlite3.connect(f"{work}/postbox.db", timeout=10)
        real = db.execute("SELECT host_key, host_fp FROM agent_shell_hosts WHERE identity = ?", (full["address"],)).fetchone()
        db.execute("UPDATE agent_shell_hosts SET host_key = ?, host_fp = ? WHERE identity = ?", (fake, fake_fp, full["address"]))
        db.commit()
        fresh = client_home("fresh", strong)
        r = ssh(fresh, full["address"], "echo", "SHOULD-NOT-RUN")
        c.ok(r.returncode != 0 and "SHOULD-NOT-RUN" not in r.stdout and "not the pinned one" in r.stderr,
             "a host key that is not the agent's is refused in the SSH handshake")
        db.execute("UPDATE agent_shell_hosts SET host_key = ?, host_fp = ? WHERE identity = ?", (real[0], real[1], full["address"]))
        db.commit()
        db.close()

        # The agent machine gets a new key: refused until approved; then the client's own pin warns.
        agentd.terminate()
        agentd.wait(timeout=10)
        os.remove(os.path.join(machine, "shell-host-ed25519"))
        agentd = start_agentd()
        st = wait_for("the new key to be reported", lambda: _pending(base, strong, full["address"]), timeout=60)
        r = ssh(me, full["address"], "true")
        c.ok(r.returncode != 0 and "host_key_pending" in r.stderr, "a new host key: no session until someone approves it")
        new_fp = st["pending_host_key_fp"]
        s, _ = call(base, "POST", f"/v1/agents/{urllib.parse.quote(full['address'], safe='')}/shell/host-key", weak,
                    {"fingerprint": new_fp})
        c.ok(s == 401, "approving a rotation needs step-up")
        s, _ = call(base, "POST", f"/v1/agents/{urllib.parse.quote(full['address'], safe='')}/shell/host-key", strong,
                    {"fingerprint": new_fp})
        c.ok(s == 200, "the owner approves the new key with step-up")
        r = ssh(me, full["address"], "true")
        c.ok(r.returncode != 0 and "host key changed" in r.stderr and new_fp in r.stderr,
             "the client still refuses a key different from its own pin")
        r = ssh(me, full["address"], "echo", "rotated", extra=("--accept-host-key", new_fp))
        c.ok(r.returncode == 0 and "rotated" in r.stdout,
             f"and accepts it only when told the exact new fingerprint {r.stderr[-300:] if r.returncode else ''}")

        # --- audit: metadata only ------------------------------------------------------------------
        db = sqlite3.connect(f"file:{work}/postbox.db?mode=ro", uri=True)
        rows = db.execute("SELECT action, details FROM audit_log WHERE action LIKE 'ssh.%'").fetchall()
        db.close()
        actions = {a for a, _ in rows}
        c.ok({"ssh.session.request", "ssh.session.open", "ssh.session.close", "ssh.host_key.rotate"} <= actions,
             "audit: request, open, close and rotation are recorded")
        blob = " ".join(d for _, d in rows)
        for plain in ("PPMARK", "PPTTY", "sleep", "notes", "lt_", "cert-v01"):
            c.ok(plain not in blob, f"audit holds no content or credentials ({plain})")
        with open(os.path.join(machine, "agentd-shell.jsonl")) as f:
            local = f.read()
        c.ok("PPMARK" not in local and '"event":"close"' in local, "the machine's local log is metadata only")

        print(f"PASS: {c.passed} checks" + (f", skipped: {', '.join(c.skipped)}" if c.skipped else ""))
    finally:
        for p in procs:
            if p.poll() is None:
                p.terminate()
                try:
                    p.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    p.kill()
        if containers_used:
            ids = subprocess.run(["docker", "ps", "-aq", "--filter", "label=dev.pigeonpost.shell=1"],
                                 capture_output=True, text=True).stdout.split()
            if ids:
                subprocess.run(["docker", "rm", "-f", *ids], stdout=subprocess.DEVNULL)
        if os.environ.get("PP_E2E_KEEP"):
            print(f"kept {work}")
        else:
            shutil.rmtree(work, ignore_errors=True)


def _sha256(b):
    import hashlib
    return hashlib.sha256(b).digest()


def _health(base, path="/health"):
    try:
        with urllib.request.urlopen(base + path, timeout=2) as r:
            return r.status == 200
    except OSError:
        return False


def _connected(base, token, address):
    s, v = call(base, "GET", f"/v1/agents/{urllib.parse.quote(address, safe='')}/shell", token)
    return v if s == 200 and v.get("connected") and v.get("enabled") else None


def _pending(base, token, address):
    s, v = call(base, "GET", f"/v1/agents/{urllib.parse.quote(address, safe='')}/shell", token)
    return v if s == 200 and v.get("pending_host_key_fp") and v.get("connected") else None


def _session(base, token, state):
    s, v = call(base, "GET", f"/v1/shell/sessions?state={state}", token)
    if s == 200 and v["sessions"]:
        return v["sessions"][0]["id"]
    return None


def _pgrep(pattern):
    return subprocess.run(["pgrep", "-f", pattern], stdout=subprocess.DEVNULL).returncode == 0


def interactive(cli, env, agent, lines, until, timeout=40):
    """Run `pigeonpost ssh` on a real terminal; type `lines` once `until`'s prompt is up."""
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(cli, [cli, "ssh", agent], {**env, "TERM": "xterm"})
    out = b""
    sent = False
    end = time.time() + timeout
    status = None
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.5)
        if r:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            out += chunk
        if not sent and (b"$" in out or b"#" in out or time.time() > end - timeout + 4):
            for line in lines:
                os.write(fd, line)
                time.sleep(0.3)
            sent = True
        done, st = os.waitpid(pid, os.WNOHANG)
        if done:
            status = os.waitstatus_to_exitcode(st)
            break
    if status is None:
        try:
            _, st = os.waitpid(pid, 0)
            status = os.waitstatus_to_exitcode(st)
        except ChildProcessError:
            pass
    return out, status


if __name__ == "__main__":
    sys.exit(main())
