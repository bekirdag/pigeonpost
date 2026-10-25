#!/usr/bin/python3
"""Certbot deploy hook: validate, publish and hot-reload the TURN certificate."""

import fcntl
import grp
import hashlib
import os
from pathlib import Path
import shutil
import socket
import ssl
import subprocess
import tempfile
import time
import uuid


HOST = "turn.pigeonpost.dev"
LINEAGE = Path("/etc/letsencrypt/live") / HOST
DEST = Path("/etc/turnserver-tls")


def command(*args):
    return subprocess.run(args, check=True, capture_output=True, timeout=15).stdout


def service_pid():
    pid = int(command("systemctl", "show", "coturn", "--property=MainPID", "--value"))
    if pid <= 0:
        raise RuntimeError("Coturn is not running; certificate was not published")
    return pid


def publish(target):
    temporary = DEST / (".current-" + uuid.uuid4().hex)
    try:
        temporary.symlink_to(target)
        os.replace(temporary, DEST / "current")
        directory = os.open(DEST, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        temporary.unlink(missing_ok=True)


def reload_certificate():
    # SIGHUP resets the log; SIGUSR2 reloads SSL contexts without losing allocations.
    command("systemctl", "kill", "--kill-who=main", "--signal=USR2", "coturn")


def install():
    fullchain = (LINEAGE / "fullchain.pem").read_bytes()
    key = (LINEAGE / "privkey.pem").read_bytes()
    if len(fullchain) > 65536 or len(key) > 16384:
        raise RuntimeError("Certificate input exceeds the size limit")
    gid = grp.getgrnam("turnserver").gr_gid
    if DEST.is_symlink() or DEST.stat().st_uid != 0:
        raise RuntimeError("Certificate destination must be a root-owned directory")
    previous = os.readlink(DEST / "current")
    pid = service_pid()
    release = Path(tempfile.mkdtemp(prefix="cert-", dir=DEST))
    published = False
    try:
        os.chown(release, 0, gid)
        os.chmod(release, 0o750)
        for name, data in (("fullchain.pem", fullchain), ("privkey.pem", key)):
            with (release / name).open("xb") as output:
                os.fchown(output.fileno(), 0, gid)
                os.fchmod(output.fileno(), 0o640)
                output.write(data)
                output.flush()
                os.fsync(output.fileno())
        chain_path = str(release / "fullchain.pem")
        # OpenSSL checks the chain, time and hostname. Python checks the key matches.
        command("openssl", "verify", "-purpose", "sslserver", "-verify_hostname", HOST,
                "-untrusted", chain_path, chain_path)
        server_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        server_context.load_cert_chain(chain_path, str(release / "privkey.pem"))
        leaf = ssl.PEM_cert_to_DER_cert(fullchain.decode().split("-----END CERTIFICATE-----")[0]
                                      + "-----END CERTIFICATE-----\n")
        fingerprint = hashlib.sha256(leaf).digest()
        if service_pid() != pid:
            raise RuntimeError("Coturn changed while validating the certificate")
        published = True
        publish(release.name)
        reload_certificate()
        client_context = ssl.create_default_context()
        deadline = time.monotonic() + 12
        while True:
            try:
                with socket.create_connection(("127.0.0.1", 5349), timeout=2) as raw:
                    with client_context.wrap_socket(raw, server_hostname=HOST) as tls:
                        served = hashlib.sha256(tls.getpeercert(binary_form=True)).digest()
                if served == fingerprint and service_pid() == pid:
                    print("TURN certificate verified and reloaded; service PID preserved")
                    return
            except (OSError, ssl.SSLError):
                pass
            if time.monotonic() >= deadline:
                raise RuntimeError("Coturn did not serve the renewed certificate")
            time.sleep(0.2)
    except BaseException:
        if published:
            publish(previous)
            reload_certificate()
        shutil.rmtree(release)
        raise
    # Keep preceding generations for server-side recovery; do not prune during renewal.


if __name__ == "__main__":
    if Path(os.environ.get("RENEWED_LINEAGE", "")) == LINEAGE:
        os.umask(0o077)
        with open("/run/lock/pigeonpost-turn-certificate.lock", "a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            install()
