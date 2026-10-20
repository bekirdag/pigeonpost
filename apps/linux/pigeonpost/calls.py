"""Incoming-call alerting. No GUI dependency.

While the app is signed in, one worker holds the calls event stream
(`GET /calls/v1/events`). A ring (`incoming`) becomes a desktop notification with Answer and
Decline; `ring_ended` withdraws it. Answer opens the web inbox with `?answer=<room>` because
WebKitGTK has no WebRTC; Decline declines from this app's own event-stream session.

Threading: the stream worker only parses. Everything that touches ring state, notifications or
the ringtone is handed to `dispatch` (the GTK main loop in the app) and runs there.
"""

import json
import random
import re
import shutil
import socket
import subprocess
import threading
import time
import unicodedata
import urllib.error
import urllib.parse
import urllib.request

from . import VERSION
from .api import APIError, NoRedirect
from .model import display_name, valid_peer

CALLS = "https://inbox.pigeonpost.dev/calls"
WEB_INBOX = "https://inbox.pigeonpost.dev/"
ROOM = re.compile(r"[A-Za-z0-9_-]{10,64}")
ADDRESS = re.compile(r"/[A-Za-z0-9._~\-/]{1,200}")  # the calls service's own address rule
KINDS = {"audio": "voice", "video": "video"}
MAX_LINE = 16 * 1024           # one SSE line
MAX_EVENT = 64 * 1024          # one event's data
RING_SECONDS = 60              # the service rings for 45 s; withdraw stale alerts even if no end arrives
READ_TIMEOUT = 75              # the service sends a keepalive every 20 s
MAX_BACKOFF = 60


def valid_room(value):
    return isinstance(value, str) and bool(ROOM.fullmatch(value))


def valid_address(value):
    return isinstance(value, str) and bool(ADDRESS.fullmatch(value)) and valid_peer(value)


def answer_url(room):
    if not valid_room(room):
        raise ValueError("invalid room")
    return WEB_INBOX + "?answer=" + room


def plain_text(value, limit=60):
    """Text safe for a notification server that may interpret markup: no tags, entities,
    control or bidi characters, and short."""
    value = "".join(c for c in str(value) if c not in "<>&" and unicodedata.category(c) not in ("Cc", "Cf", "Zl", "Zp"))
    value = " ".join(value.split())
    return value[:limit].rstrip() or "Unknown caller"


class Ring:
    __slots__ = ("room", "code", "kind", "address", "handle", "to", "session", "generation")

    def __init__(self, room, code, kind, address, handle, to):
        self.room, self.code, self.kind = room, code, kind
        self.address, self.handle, self.to = address, handle, to
        self.session = self.generation = None

    @classmethod
    def parse(cls, data):
        """An `incoming` payload, or None when anything in it is not what the service sends."""
        if not isinstance(data, dict) or not valid_room(data.get("room")) or data.get("kind") not in KINDS:
            return None
        caller = data.get("from")
        if not isinstance(caller, dict) or not valid_address(caller.get("address")):
            return None
        handle = caller.get("handle")
        handle = handle if valid_address(handle) else None
        to = data.get("to")
        if not valid_address(to):
            return None
        code = data.get("code")
        code = code if isinstance(code, str) and re.fullmatch(r"[A-Za-z0-9]{6}", code) else None
        return cls(data["room"], code, data["kind"], caller["address"], handle, to)


class ProtocolError(Exception):
    pass


class SSEParser:
    """Server-sent events, one line at a time. Oversized events are dropped, not truncated."""

    def __init__(self):
        self.reset()

    def reset(self):
        self.event, self.data, self.size, self.overflow = "message", [], 0, False

    def feed(self, raw):
        """Feed one line (bytes, with or without its newline). Returns (event, data) when the line
        completes an event, else None."""
        if len(raw) > MAX_LINE + 2:
            raise ProtocolError("line too long")
        try:
            line = raw.decode("utf-8")
        except UnicodeDecodeError:
            self.overflow = True  # poison the current event only
            return None
        line = line.rstrip("\r\n")
        if not line:
            event, data, bad = self.event, "\n".join(self.data), self.overflow or not self.data
            self.reset()
            return None if bad else (event, data)
        if line.startswith(":"):
            return None
        field, _, value = line.partition(":")
        if value.startswith(" "):
            value = value[1:]
        if field == "event":
            self.event = value[:64]
        elif field == "data":
            self.size += len(value) + 1
            if self.size > MAX_EVENT:
                self.overflow, self.data = True, []
            elif not self.overflow:
                self.data.append(value)
        return None  # id, retry and unknown fields are ignored


def decode(event, data):
    """(event, payload dict) for the events this app uses; None for anything else."""
    if event not in ("ready", "incoming", "ring_ended"):
        return None
    try:
        payload = json.loads(data)
    except ValueError:
        return None
    return (event, payload) if isinstance(payload, dict) else None


class Ringtone:
    """Optional ringtone through libcanberra's player when the desktop has it; otherwise silent."""

    def __init__(self, which=shutil.which, spawn=subprocess.Popen):
        self.which, self.spawn, self.process = which, spawn, None

    def start(self):
        if self.process:
            return
        player = self.which("canberra-gtk-play")
        if not player:
            return
        try:
            self.process = self.spawn([player, "--id=phone-incoming-call", "--loop=15", "--description=Incoming call"],
                                      stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except OSError:
            self.process = None

    def stop(self):
        process, self.process = self.process, None
        if process:
            try:
                process.terminate()
            except OSError:
                pass


def _direct(fn, *args):
    fn(*args)


def _timer(seconds, fn):
    timer = threading.Timer(seconds, fn)
    timer.daemon = True
    timer.start()


class CallAlerts:
    """The calls event stream and the rings it produces.

    `session` is the app's auth.Session (access_token / access_token(rejected=...)).
    `notifier` has show(room, title, body) and withdraw(room).
    `dispatch(fn, *args)` runs fn on the UI thread; `later(seconds, fn)` runs fn on it later.
    """

    def __init__(self, session, notifier, open_url, *, dispatch=_direct, later=_timer, name_for=None,
                 ringtone=None, base=CALLS, opener=None, insecure_loopback=False):
        scheme, host = urllib.parse.urlsplit(base)[:2]
        loopback = insecure_loopback and scheme == "http" and host.split(":")[0] in ("127.0.0.1", "localhost")
        if scheme != "https" and not loopback:
            raise ValueError("HTTPS is required")
        self.session, self.notifier, self.open_url = session, notifier, open_url
        self.dispatch, self.later = dispatch, later
        self.name_for = name_for or (lambda address, handle: display_name(handle or address))
        self.ringtone = ringtone if ringtone is not None else Ringtone()
        self.base = base.rstrip("/")
        self.opener = opener or urllib.request.build_opener(NoRedirect())
        self.rings = {}           # UI thread only
        self.session_id = None    # the live event-stream session, set by the worker
        self.generation = 0
        self.stopped = threading.Event()
        self.stopped.set()
        self.response = None
        self.ready = False
        self.thread = None
        self.lock = threading.Lock()
        self.state = "stopped"    # for tests and diagnostics: stopped, connecting, listening, signed_out

    # ---- lifecycle (UI thread) -------------------------------------------------------------------

    def start(self):
        with self.lock:
            if not self.stopped.is_set():
                return
            self.generation += 1
            self.stopped = threading.Event()
            self.thread = threading.Thread(target=self._run, args=(self.generation, self.stopped),
                                           name="pigeonpost-calls", daemon=True)
            self.thread.start()

    def stop(self):
        with self.lock:
            self.generation += 1
            self.stopped.set()
            response, self.response = self.response, None
            self.session_id = None
        self._close(response)
        self._withdraw_all()
        self.state = "stopped"

    @property
    def running(self):
        return not self.stopped.is_set()

    # ---- the stream (worker thread) --------------------------------------------------------------

    def _run(self, generation, stopped):
        failures, renewed, rejected = 0, False, None
        while not stopped.is_set():
            self.state = "connecting"
            try:
                token = self.session.access_token(rejected=rejected)
            except APIError as exc:
                if exc.status == 401 or exc.code == "invalid_grant":
                    break  # signed out; the app's own requests show the sign-in screen
                failures = self._backoff(stopped, failures)
                continue
            except Exception:
                failures = self._backoff(stopped, failures)
                continue
            try:
                response = self._open(token)
            except urllib.error.HTTPError as exc:
                exc.close()
                if exc.code == 401:
                    if renewed:
                        break  # a freshly renewed token was refused too: stop, do not loop
                    renewed, rejected = True, token
                    continue
                if exc.code == 403:
                    break  # no mailbox yet; the app restarts the stream when one is created
                failures = self._backoff(stopped, max(failures, 5) if exc.code == 429 else failures)
                continue
            except (urllib.error.URLError, OSError, ValueError, APIError, ProtocolError):
                failures = self._backoff(stopped, failures)
                continue
            with self.lock:
                if stopped.is_set():
                    self._close(response)
                    break
                self.response = response
            began, self.ready = time.monotonic(), False
            try:
                self._read(response, generation, stopped)
            except (OSError, ValueError, ProtocolError, AttributeError):
                pass  # read timeout, reset, oversized line, or the socket closed by stop()
            finally:
                with self.lock:
                    if self.response is response:
                        self.response = None
                self._close(response)
            self.session_id = None
            self.dispatch(self._stream_lost, generation)
            if self.ready:
                renewed, rejected = False, None
                if time.monotonic() - began > 30:
                    failures = 0
            failures = self._backoff(stopped, failures)
        if self.generation == generation:
            self.state = "stopped" if stopped.is_set() else "signed_out"
        stopped.set()

    def _open(self, token):
        request = urllib.request.Request(self.base + "/v1/events", method="GET", headers={
            "Accept": "text/event-stream", "Cache-Control": "no-store",
            "Authorization": "Bearer " + token, "User-Agent": "Pigeonpost-Linux/" + VERSION,
        })
        response = self.opener.open(request, timeout=READ_TIMEOUT)
        if not (response.headers.get("Content-Type") or "").startswith("text/event-stream"):
            self._close(response)
            raise ProtocolError("not an event stream")
        return response

    def _read(self, response, generation, stopped):
        parser = SSEParser()
        while not stopped.is_set():
            line = response.readline(MAX_LINE + 3)
            if not line:
                return
            if len(line) > MAX_LINE + 2:
                raise ProtocolError("line too long")
            frame = parser.feed(line)
            event = frame and decode(*frame)
            if not event:
                continue
            name, payload = event
            if name == "ready":
                session = payload.get("session")
                if isinstance(session, str) and re.fullmatch(r"[A-Za-z0-9_-]{10,64}", session):
                    self.session_id, self.ready = session, True
                    self.state = "listening"
            elif name == "incoming" and self.ready:
                ring = Ring.parse(payload)
                if ring:
                    ring.session, ring.generation = self.session_id, generation
                    self.dispatch(self._incoming, ring)
            elif name == "ring_ended" and valid_room(payload.get("room")):
                self.dispatch(self._ended, generation, payload["room"])

    def _backoff(self, stopped, failures):
        delay = min(MAX_BACKOFF, 2 ** failures) * random.uniform(0.75, 1.25)
        stopped.wait(delay)
        return min(failures + 1, 8)

    @staticmethod
    def _close(response):
        if not response:
            return
        # Closing the socket is what interrupts a blocked readline in the worker.
        try:
            sock = response.fp.raw._sock
            sock.shutdown(socket.SHUT_RDWR)
        except Exception:
            pass
        try:
            response.close()
        except Exception:
            pass

    # ---- rings (UI thread) -----------------------------------------------------------------------

    def _incoming(self, ring):
        if ring.generation != self.generation or ring.room in self.rings:
            return
        self.rings[ring.room] = ring
        name = plain_text(self.name_for(ring.address, ring.handle))
        self.notifier.show(ring.room, f"Incoming {KINDS[ring.kind]} call", name)
        self.ringtone.start()
        self.later(RING_SECONDS, lambda room=ring.room, current=ring: self.dispatch(self._expire, room, current))

    def _ended(self, generation, room):
        if generation == self.generation:
            self._withdraw(room)

    def _expire(self, room, ring):
        if self.rings.get(room) is ring:
            self._withdraw(room)

    def _stream_lost(self, generation):
        # The service forgets a closed session's rings and sends it no end; a reconnecting session
        # is rung again if the call is still ringing.
        if generation == self.generation:
            self._withdraw_all()

    def _withdraw(self, room):
        self.rings.pop(room, None)
        self.notifier.withdraw(room)
        if not self.rings:
            self.ringtone.stop()

    def _withdraw_all(self):
        for room in list(self.rings):
            self._withdraw(room)
        self.ringtone.stop()

    def answer(self, room):
        """Answer button. The web inbox answers a ring for `?answer=<room>` as soon as it sees it."""
        if not valid_room(room):
            return False
        self._withdraw(room)
        self.open_url(answer_url(room))
        return True

    def decline(self, room, background=True):
        """Decline button: decline from this app's session. Other devices stop ringing too."""
        if not valid_room(room):
            return False
        ring = self.rings.get(room)
        self._withdraw(room)
        if not ring or not ring.session or ring.session != self.session_id:
            return False  # the ring is gone or belonged to a closed session; nothing to decline
        if background:
            threading.Thread(target=self._decline, args=(room, ring.session), daemon=True).start()
            return True
        return self._decline(room, ring.session)

    def _decline(self, room, session):
        url = self.base + "/v1/rooms/" + urllib.parse.quote(room, safe="") + "/decline"
        body = json.dumps({"session": session}).encode()
        rejected = None
        for _ in range(2):
            try:
                token = self.session.access_token(rejected=rejected)
            except Exception:
                return False
            request = urllib.request.Request(url, data=body, method="POST", headers={
                "Content-Type": "application/json", "Accept": "application/json",
                "Authorization": "Bearer " + token, "User-Agent": "Pigeonpost-Linux/" + VERSION,
            })
            try:
                with self.opener.open(request, timeout=15) as response:
                    response.read(4096)
                return True
            except urllib.error.HTTPError as exc:
                exc.close()
                if exc.code != 401 or rejected:
                    return False
                rejected = token  # rejected before processing; renew once and retry
            except (urllib.error.URLError, OSError, ValueError, APIError):
                return False
        return False
