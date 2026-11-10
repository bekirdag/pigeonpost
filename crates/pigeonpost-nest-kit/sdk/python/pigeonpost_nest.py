"""Pigeonpost Nest runtime helper for Python (standard library only).

A marketplace Nest app runs as one long-lived process inside its sandbox. The platform hands it
jobs as files and reads results back as files; the app's only network peer is the model broker.
This module is that contract and nothing more:

    <root>/jobs/in/<job>.json    one job: {job_id, operation, operation_version, payload,
                                 config?}; config is the installation's configuration
    <root>/jobs/out/<job>.json   its result: {job_id, status: "succeeded", answer, sources,
                                 model_used} or {job_id, status: "failed", error_code}
    <root>/run/broker-token      the broker token for this start (nbt_...)
    NEST_BROKER_URL              http://host:port of the broker

<root> is NEST_ROOT, or /nest in the sandbox. Copy this file into your app; it is not published to
a registry. The same contract in Node: crates/pigeonpost-nest-kit/sdk/node/pigeonpost-nest.mjs.
"""

from __future__ import annotations

import http.client
import json
import os
import re
import socket
import sys
import time
import traceback
import types
import urllib.parse
from typing import Any, Callable, Dict, List, Optional

SDK_VERSION = "0.1.0"
#: Largest job file read, as the postbox caps requests at 256 KiB.
MAX_JOB_BYTES = 256 * 1024
#: Largest result the dispatcher reads back.
MAX_RESULT_BYTES = 512 * 1024
MAX_REPLY_BYTES = 512 * 1024
BROKER_TIMEOUT_S = 120
MAX_PROGRESS_PER_JOB = 100
ERROR_CODE = re.compile(r"^[a-z][a-z0-9_]{0,63}$")
EFFECT_KEY = re.compile(r"^[A-Za-z0-9._:-]{1,128}$")
EFFECT_KIND = re.compile(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$")


class NestError(Exception):
    """A job outcome the app chose: the job fails with `code` and nothing else leaves the sandbox."""

    def __init__(self, code: str, message: str = ""):
        super().__init__(message or code)
        self.code = code if ERROR_CODE.match(code or "") else "internal_error"


class BrokerError(NestError):
    """The broker refused or could not be reached. `code` is the broker's error code."""

    def __init__(self, code: str, message: str = "", status: int = 0):
        super().__init__(code, message)
        self.status = status


class _NotSent(Exception):
    pass


class _Sent(Exception):
    pass


def nest_root(env=None) -> str:
    env = os.environ if env is None else env
    return env.get("NEST_ROOT") or "/nest"


def _read_json(path: str, limit: int):
    try:
        st = os.lstat(path)
    except OSError:
        return None
    if not os.path.isfile(path) or os.path.islink(path) or st.st_size > limit:
        return None
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def config_of(raw) -> types.MappingProxyType:
    """The installation's configuration from a job, or an empty mapping when it carries none."""
    c = raw.get("config") if isinstance(raw, dict) else None
    return types.MappingProxyType(dict(c) if isinstance(c, dict) else {})


class Broker:
    """The model broker as one app sees it. The broker chooses the model, caps every request and
    charges the Nest's monthly allowance; the app never holds a provider key."""

    def __init__(self, url: Optional[str] = None, root: Optional[str] = None):
        url = os.environ.get("NEST_BROKER_URL") if url is None else url
        self.url = url if url and re.match(r"^http://[^/]+/?$", url) else None
        self.root = root or nest_root()
        self.used = False
        self.usage = {"input": 0, "output": 0, "requests": 0}

    def token(self) -> Optional[str]:
        try:
            with open(os.path.join(self.root, "run", "broker-token"), "r", encoding="utf-8") as f:
                t = f.read().strip()
        except OSError:
            return None
        return t if t.startswith("nbt_") else None

    @property
    def available(self) -> bool:
        """Whether a broker is configured for this start. A no-AI Nest never needs one."""
        return self.url is not None and self.token() is not None

    def call(self, method: str, path: str, body: Any = None, headers: Optional[Dict[str, str]] = None):
        """One request. Returns (status, parsed body). Raises _NotSent or _Sent on failure."""
        token = self.token()
        if self.url is None or token is None:
            raise BrokerError("no_broker", "no broker is configured for this Nest")
        parts = urllib.parse.urlsplit(self.url)
        data = b"" if body is None else json.dumps(body).encode("utf-8")
        hdrs = {
            "Authorization": "Bearer " + token,
            "Content-Type": "application/json",
            "Content-Length": str(len(data)),
            "Connection": "close",
        }
        hdrs.update(headers or {})
        conn = http.client.HTTPConnection(parts.hostname, parts.port or 80, timeout=BROKER_TIMEOUT_S)
        try:
            try:
                conn.connect()
            except OSError as e:
                raise _NotSent(str(e)) from e
            try:
                conn.request(method, path, body=data, headers=hdrs)
                resp = conn.getresponse()
                raw = resp.read(MAX_REPLY_BYTES + 1)
            except (OSError, http.client.HTTPException, socket.timeout) as e:
                raise _Sent(str(e)) from e
            if len(raw) > MAX_REPLY_BYTES:
                raise _Sent("reply too large")
            try:
                parsed = json.loads(raw.decode("utf-8") or "null")
            except ValueError:
                parsed = None
            return resp.status, parsed
        finally:
            conn.close()

    def complete(
        self,
        user: str = "",
        system: Optional[str] = None,
        messages: Optional[List[Dict[str, str]]] = None,
        max_tokens: int = 400,
    ) -> Dict[str, Any]:
        """One completion. Pass `messages` ([{role, content}]) or `system` and `user`.
        Returns {"text", "usage": {"input", "output"}}.

        Busy (one request at a time per Nest) is retried briefly, because the broker refuses it
        before anything is reserved or sent. Nothing else is retried: an upstream failure may
        already have reached the model provider."""
        if messages is None:
            messages = ([{"role": "system", "content": str(system)}] if system else []) + [
                {"role": "user", "content": str(user)}
            ]
        body = {"messages": messages, "max_tokens": max_tokens}
        attempt = 0
        while True:
            try:
                status, reply = self.call("POST", "/v1/chat/completions", body)
            except _NotSent as e:
                raise BrokerError("broker_unreachable", str(e)) from None
            except _Sent as e:
                raise BrokerError("broker_outcome_unknown", str(e)) from None
            reply = reply if isinstance(reply, dict) else {}
            if status == 409 and reply.get("error") == "busy" and attempt < 3:
                time.sleep(0.25 * 2**attempt)
                attempt += 1
                continue
            if status != 200:
                code = reply.get("error") if isinstance(reply.get("error"), str) else "broker_error"
                raise BrokerError(code, str(reply.get("detail") or "broker answered %d" % status), status)
            try:
                text = reply["choices"][0]["message"]["content"]
            except (KeyError, IndexError, TypeError):
                text = None
            if not isinstance(text, str):
                raise BrokerError("broker_error", "no answer in the reply", 200)
            usage = reply.get("usage") or {}
            used = {"input": int(usage.get("prompt_tokens") or 0), "output": int(usage.get("completion_tokens") or 0)}
            self.used = True
            self.usage["input"] += used["input"]
            self.usage["output"] += used["output"]
            self.usage["requests"] += 1
            return {"text": text, "usage": used}

    def budget(self) -> Dict[str, Any]:
        """This Nest's monthly allowance: {period, input_tokens: {used, limit}, output_tokens}."""
        try:
            status, reply = self.call("GET", "/v1/budget")
        except (_NotSent, _Sent) as e:
            raise BrokerError("broker_unreachable", str(e)) from None
        if status != 200 or not isinstance(reply, dict):
            raise BrokerError((reply or {}).get("error", "broker_error"), "budget refused", status)
        return reply


class Job:
    """What a handler receives."""

    def __init__(self, raw: Dict[str, Any], broker: Broker, config):
        self.id = raw.get("job_id") if isinstance(raw.get("job_id"), str) else ""
        self.operation = raw.get("operation")
        self.version = raw.get("operation_version")
        self.payload = raw.get("payload") if isinstance(raw.get("payload"), dict) else {}
        self.config = config
        self.broker = broker
        self._effects: Dict[str, Any] = {}
        self._progress_sent = 0

    def progress(self, fraction: float, message: str = "") -> None:
        """Report progress (0..1) with a short message. Best effort: a platform that does not take
        progress yet ignores it, and a failure never fails the job."""
        if self._progress_sent >= MAX_PROGRESS_PER_JOB or not self.broker.available:
            return
        self._progress_sent += 1
        try:
            f = max(0.0, min(1.0, float(fraction)))
        except (TypeError, ValueError):
            f = 0.0
        try:
            self.broker.call("POST", "/v1/progress", {"job_id": self.id, "fraction": f, "message": str(message)[:200]})
        except Exception:  # noqa: BLE001 - progress is advisory
            pass

    def effect(self, key: str, kind: str, summary: str = "", body: Any = None, requires_approval: bool = False):
        """Ask the platform to perform one external effect (a write outside the Nest) on the
        buyer's behalf. The Nest itself has no route to the outside: the effect goes to the broker,
        which holds the buyer's grant, asks for approval when `requires_approval` is set, and
        performs it at most once per `key`.

        `key` is your idempotency key: derive it from the job (its id and input, never model
        output) so that a redelivered job asks for the same effect, never a second one. The first
        request made under a key is the one approved and performed; the outcome ({reference, body})
        carries that request's body back. Returns the outcome. Raises NestError with
        `approval_required`, `effects_unavailable`, `idempotency_conflict`, `effect_refused` or
        `effect_outcome_unknown` (sent, no answer: reconciled by the platform, never retried here).
        """
        if not EFFECT_KEY.match(str(key)):
            raise NestError("invalid_effect", "effect key: 1-128 of A-Z a-z 0-9 . _ : -")
        if not EFFECT_KIND.match(str(kind)):
            raise NestError("invalid_effect", "effect kind: dotted lower-case words")
        if key in self._effects:
            return self._effects[key]
        request = {
            "job_id": self.id,
            "key": key,
            "kind": kind,
            "summary": str(summary)[:500],
            "body": {} if body is None else body,
            "requires_approval": bool(requires_approval),
        }
        try:
            status, reply = self.broker.call("POST", "/v1/effects", request, {"Idempotency-Key": key})
        except BrokerError as e:
            if e.code == "no_broker":
                raise NestError("effects_unavailable") from None
            raise
        except _NotSent as e:
            raise NestError("effects_unavailable", str(e)) from None
        except _Sent as e:
            raise NestError("effect_outcome_unknown", str(e)) from None
        reply = reply if isinstance(reply, dict) else {}
        if status == 200 and reply.get("state") == "done":
            outcome = reply.get("outcome") or {}
            self._effects[key] = outcome
            return outcome
        if status == 202 or reply.get("state") == "awaiting_approval":
            raise NestError("approval_required")
        if status == 404:
            raise NestError("effects_unavailable")
        if status == 409:
            raise NestError("idempotency_conflict")
        if status == 403:
            raise NestError("effect_refused", str(reply.get("detail") or ""))
        raise NestError("effect_outcome_unknown", "broker answered %d" % status)


def _failed(job_id: str, code: str) -> Dict[str, Any]:
    return {"job_id": job_id, "status": "failed", "error_code": code if ERROR_CODE.match(code or "") else "internal_error"}


def result_of(job_id: str, value: Any, model_used: bool) -> Dict[str, Any]:
    """Turn what a handler returned into the result the dispatcher reads."""
    shaped = value if isinstance(value, dict) and "answer" in value else {"answer": value}
    out = {
        "job_id": job_id,
        "status": "succeeded",
        "answer": shaped.get("answer"),
        "sources": shaped.get("sources") if isinstance(shaped.get("sources"), list) else [],
        "model_used": shaped["model_used"] if isinstance(shaped.get("model_used"), bool) else model_used,
    }
    if len(json.dumps(out).encode("utf-8")) > MAX_RESULT_BYTES:
        return _failed(job_id, "result_too_large")
    return out


Handler = Callable[[Job], Any]


def process_job(path: str, handlers: Dict[str, Handler], root: Optional[str] = None, url: Optional[str] = None):
    """Run one job file through the handlers and write its result. Returns the result."""
    root = root or nest_root()
    raw = _read_json(path, MAX_JOB_BYTES)
    job_id = raw.get("job_id") if isinstance(raw, dict) and isinstance(raw.get("job_id"), str) else ""
    if not isinstance(raw, dict):
        result = _failed(job_id, "invalid_job")
    else:
        handler = handlers.get("%s@%s" % (raw.get("operation"), raw.get("operation_version")))
        if handler is None:
            result = _failed(job_id, "unknown_operation")
        else:
            broker = Broker(url=url, root=root)
            try:
                job = Job(raw, broker, config_of(raw))
                result = result_of(job_id, handler(job), broker.used)
            except NestError as e:
                result = _failed(job_id, e.code)
            except Exception:  # noqa: BLE001 - the job fails, the worker keeps serving
                sys.stderr.write("pigeonpost-nest: %s: %s\n" % (job_id, traceback.format_exc()))
                result = _failed(job_id, "internal_error")
    out = os.path.join(root, "jobs", "out", os.path.basename(path))
    tmp = out[: -len(".json")] + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(result, f)
    os.replace(tmp, out)
    try:
        os.remove(path)
    except FileNotFoundError:
        pass
    return result


def pending_jobs(root: Optional[str] = None) -> List[str]:
    """The job files waiting now, oldest name first."""
    inbox = os.path.join(root or nest_root(), "jobs", "in")
    try:
        names = sorted(n for n in os.listdir(inbox) if n.endswith(".json"))
    except OSError:
        return []
    return [os.path.join(inbox, n) for n in names]


def run_once(handlers: Dict[str, Handler], root: Optional[str] = None, url: Optional[str] = None) -> int:
    """Process every waiting job once. Returns how many were processed."""
    root = root or nest_root()
    os.makedirs(os.path.join(root, "jobs", "in"), exist_ok=True)
    os.makedirs(os.path.join(root, "jobs", "out"), exist_ok=True)
    n = 0
    for path in pending_jobs(root):
        process_job(path, handlers, root=root, url=url)
        n += 1
    return n


def serve(handlers: Dict[str, Handler], root: Optional[str] = None, url: Optional[str] = None, poll_s: float = 0.5):
    """The app's main loop: process jobs as they arrive, forever. `handlers` maps
    "operation@version" to a function (job) -> result."""
    while True:
        try:
            run_once(handlers, root=root, url=url)
        except OSError:
            # A volume error must not stop the worker; the job stays in the inbox for the next pass.
            sys.stderr.write("pigeonpost-nest: %s\n" % traceback.format_exc())
        time.sleep(poll_s)
