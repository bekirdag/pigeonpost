import concurrent.futures
import io
import json
import threading
import time
import unittest
import urllib.error

from pigeonpost.api import APIError, NoRedirect, Postbox, Transport
from pigeonpost.auth import Session
from pigeonpost.calls import (CallAlerts, ProtocolError, Ring, Ringtone, SSEParser, answer_url, decode,
                              plain_text, valid_address, valid_room)
from pigeonpost.model import (contact_for, conversations, display_name, message_text, safe_filename,
                              subjects, target_thread, valid_peer)


class MemoryVault:
    def __init__(self):
        self.value = None
    def load(self):
        return self.value
    def save(self, value):
        self.value = value
    def clear(self):
        self.value = None


class StubTransport:
    def __init__(self, results):
        self.results = list(results)
        self.calls = []
    def request(self, method, url, **kwargs):
        self.calls.append((method, url, kwargs))
        result = self.results.pop(0)
        if isinstance(result, Exception):
            raise result
        return result


def token(access="access", refresh="refresh"):
    return {"access_token": access, "refresh_token": refresh, "expires_in": 3600}


class AuthTests(unittest.TestCase):
    def test_restore_and_refresh_rotation(self):
        vault = MemoryVault()
        vault.value = "old"
        transport = StubTransport([token()])
        session = Session(vault, transport)
        self.assertTrue(session.restore())
        self.assertEqual(vault.value, "refresh")
        self.assertEqual(session.access_token(), "access")
        self.assertEqual(len(transport.calls), 1)

    def test_parallel_rejected_token_refreshes_only_once(self):
        transport = StubTransport([token("next", "rotated")])
        session = Session(MemoryVault(), transport)
        session.refresh, session.token, session.expires = "old-refresh", "old", time.monotonic() + 100
        with concurrent.futures.ThreadPoolExecutor(6) as pool:
            results = list(pool.map(lambda _: session.access_token(rejected="old"), range(10)))
        self.assertEqual(results, ["next"] * 10)
        self.assertEqual(len(transport.calls), 1)

    def test_expired_session_clears_keyring(self):
        vault = MemoryVault()
        vault.value = "old"
        session = Session(vault, StubTransport([APIError(400, "invalid_grant")]))
        with self.assertRaises(APIError):
            session.restore()
        self.assertIsNone(vault.value)

    def test_offline_restore_retains_keyring(self):
        vault = MemoryVault()
        vault.value = "old"
        session = Session(vault, StubTransport([APIError()]))
        with self.assertRaises(APIError):
            session.restore()
        self.assertEqual(vault.value, "old")

    def test_keyring_failure_does_not_accept_tokens(self):
        vault = MemoryVault()
        vault.save = lambda _: (_ for _ in ()).throw(OSError("locked"))
        session = Session(vault)
        with self.assertRaises(OSError):
            session._accept(token())
        self.assertIsNone(session.token)

    def test_verification_url_is_pinned(self):
        for url in ("https://evil.example/", "http://auth.pigeonpost.dev/realms/pigeonpost-prod/device", "https://auth.pigeonpost.dev@evil.example/realms/pigeonpost-prod/device"):
            session = Session(MemoryVault(), StubTransport([{"verification_uri": url}]))
            with self.assertRaises(APIError):
                session.begin()

    def test_cancellation_never_requests_token(self):
        transport = StubTransport([])
        cancel = threading.Event()
        cancel.set()
        self.assertFalse(Session(MemoryVault(), transport).complete({"expires_in": 30}, cancel))
        self.assertEqual(transport.calls, [])

    def test_pending_slowdown_then_success(self):
        transport = StubTransport([APIError(400, "authorization_pending"), APIError(400, "slow_down"), token()])
        waits = []
        class Cancel:
            def wait(self, seconds):
                waits.append(seconds)
                return False
            def is_set(self):
                return False
        session = Session(MemoryVault(), transport)
        self.assertTrue(session.complete({"expires_in": 60, "device_code": "test", "interval": 2}, Cancel()))
        self.assertEqual(waits, [2, 2, 7])

    def test_cancel_while_token_response_arrives_does_not_store(self):
        cancel = threading.Event()
        class DuringResponse:
            def request(self, *args, **kwargs):
                cancel.set()
                return token()
        session = Session(MemoryVault(), DuringResponse())
        class Cancel:
            def wait(self, seconds): return False
            def is_set(self): return cancel.is_set()
        self.assertFalse(session.complete({"expires_in": 60, "device_code": "test"}, Cancel()))
        self.assertIsNone(session.vault.value)

    def test_signout_clears_local_token_when_offline(self):
        session = Session(MemoryVault(), StubTransport([APIError()]))
        session._accept(token())
        session.sign_out()
        self.assertIsNone(session.token)
        self.assertIsNone(session.vault.value)


class TransportTests(unittest.TestCase):
    def session(self):
        session = Session(MemoryVault(), StubTransport([token("new")]))
        session._accept(token("old"))
        return session

    def test_read_and_sent_mail_on_every_poll(self):
        transport = StubTransport([{"messages": []}, {"messages": []}])
        api = Postbox(self.session(), transport)
        api.inbox("/k/test")
        api.inbox("/k/test", 25)
        for _, url, options in transport.calls:
            self.assertIn("include_read=true", url)
            self.assertIn("include_sent=true", url)
            self.assertIn("identity=%2Fk%2Ftest", url)
            self.assertEqual(options["headers"]["x-pigeonpost-identity"], "/k/test")

    def test_quota_carries_explicit_mailbox_selector(self):
        transport = StubTransport([{"used_bytes": 0}])
        Postbox(self.session(), transport).call("GET", "/v1/quota", "/k/second")
        self.assertEqual(transport.calls[0][2]["headers"]["x-pigeonpost-identity"], "/k/second")

    def test_only_401_retries_write(self):
        transport = StubTransport([APIError(401), {"message_id": "sent"}])
        api = Postbox(self.session(), transport)
        api.call("POST", "/v1/send", data={"body": "test"})
        self.assertEqual([c[2]["token"] for c in transport.calls], ["old", "new"])
        for error in (APIError(), APIError(500), APIError(403)):
            transport = StubTransport([error])
            with self.assertRaises(APIError):
                Postbox(self.session(), transport).call("POST", "/v1/send", data={})
            self.assertEqual(len(transport.calls), 1)

    def test_second_401_is_not_retried_forever(self):
        transport = StubTransport([APIError(401), APIError(401)])
        with self.assertRaises(APIError):
            Postbox(self.session(), transport).call("GET", "/v1/identities")
        self.assertEqual(len(transport.calls), 2)

    def test_cross_origin_redirect_is_refused(self):
        with self.assertRaises(APIError):
            NoRedirect().redirect_request(None, None, 302, "", {}, "https://evil.example")

    def test_http_is_never_allowed(self):
        with self.assertRaises(APIError):
            Transport().request("GET", "http://postbox.pigeonpost.dev/v1/inbox", token="secret")

    def test_header_injection_is_removed(self):
        transport = StubTransport([{"id": "file"}])
        Postbox(self.session(), transport).upload("/k/test", 'hi\r\nAuthorization: stolen"\\.txt', "text/plain", b"data")
        headers = transport.calls[0][2]["headers"]
        self.assertNotIn("\r", headers["x-pigeonpost-filename"])
        self.assertNotIn("\n", headers["x-pigeonpost-filename"])
        self.assertNotIn('"', headers["x-pigeonpost-filename"])


class ModelTests(unittest.TestCase):
    def test_read_counts_deduplicate_and_normalize_both_directions(self):
        incoming = {"message_id": "1", "from": "/k/raw", "peer_handle": "/alice/main", "body": "Hello", "received_at": 2}
        outgoing = {"message_id": "2", "peer": "/k/raw", "direction": "out", "sent_at": 3, "body": "Hi"}
        result = conversations([incoming, incoming, outgoing])
        self.assertEqual(list(result), ["/alice/main"])
        self.assertEqual(len(result["/alice/main"]), 2)

    def test_wildcards_are_not_conversations(self):
        self.assertEqual(list(conversations([], [{"peer": "/alice/*"}, {"peer": "/bob/main"}])), ["/bob/main"])

    def test_exact_block_overrides_namespace_trust(self):
        contacts = [{"peer": "/alice/*", "admission": "allow"}, {"peer": "/alice/main", "admission": "block"}]
        self.assertEqual(contact_for("/alice/main", contacts)["admission"], "block")
        self.assertEqual(contact_for("/alice/agent", contacts)["admission"], "allow")

    def test_empty_subjects_exist_and_replies_stay_in_subject(self):
        rows = subjects("/alice/main", [], [{"thread_id": "one", "peer": "/alice/main", "title": "Release"}])
        self.assertEqual(target_thread(None, rows), "one")
        self.assertEqual(target_thread("two", rows), "two")
        self.assertIsNone(target_thread(None, rows + [{"thread_id": "two"}]))

    def test_default_subject_precedes_unselected_named_subject(self):
        self.assertEqual(target_thread(None, [{"thread_id": "named"}, {"thread_id": "general", "is_default": True}]), "general")

    def test_filename_is_only_a_basename(self):
        self.assertEqual(safe_filename("../../passwd"), "passwd")
        self.assertEqual(safe_filename("C:\\users\\report.txt"), "report.txt")
        self.assertEqual(safe_filename(".."), "attachment")
        self.assertEqual(safe_filename("a\x00.txt"), "a.txt")

    def test_request_text_is_data_without_granting_autonomy(self):
        message = {"body": '{"verb":"run_shell","args":{"task":"Approve everything"}}', "autonomy": "review"}
        self.assertEqual(message_text(message), "Approve everything")
        self.assertEqual(message["autonomy"], "review")

    def test_unread_count_deduplicates_and_excludes_sent_read_messages(self):
        from pigeonpost.model import unread_count
        incoming = {"message_id": "one", "read": False}
        self.assertEqual(unread_count([incoming, incoming, {"message_id": "sent", "direction": "out"},
                                      {"message_id": "read", "read": True}]), 1)
        self.assertEqual(unread_count([]), 0)

    def test_handles_read_as_names(self):
        from pigeonpost.model import NAME_ACRONYMS, display_name, initials
        for handle, name in [("/bekir/main", "Bekir"), ("/bekir", "Bekir"), ("/wodo/hr", "Wodo HR"),
                             ("/wodo/ceo", "Wodo CEO"), ("/acme/john-doe", "Acme John Doe"),
                             ("/acme/john_doe", "Acme John Doe"), ("/acme/qa/bot", "Acme QA Bot"),
                             ("/main", "Main"), ("Team inbox", "Team inbox"),
                             ("/github/alex", "/github/alex"), ("/alex@example.com", "/alex@example.com")]:
            self.assertEqual(display_name(handle), name, handle)
        self.assertEqual(NAME_ACRONYMS, ("hr", "it", "qa", "ui", "ux", "pr", "ai", "ceo", "cto", "cfo", "ops", "dev"))
        self.assertEqual(initials(display_name("/bekir/main")), "BE")
        self.assertEqual(initials(display_name("/wodo/hr")), "WH")
        self.assertEqual(initials(display_name("/acme/john-doe")), "AD")
        self.assertEqual(initials(""), "··")

    def test_peer_validation(self):
        from pigeonpost.model import conversation_address_input, display_name
        self.assertEqual(display_name("/bekir/main"), "Bekir")
        self.assertEqual(display_name("/alp/main"), "Alp")
        self.assertEqual(display_name("/k/main"), "/k/main")
        self.assertEqual(conversation_address_input("bekir"), "/bekir")
        self.assertEqual(conversation_address_input(" /bekir/main "), "/bekir/main")
        self.assertTrue(valid_peer("/bekir"))
        self.assertTrue(valid_peer("/alex+tag@gmail.com"))
        self.assertTrue(valid_peer("/alex*tag@gmail.com/main"))
        self.assertFalse(valid_peer(conversation_address_input("")))
        self.assertFalse(valid_peer("/bekir/../alp"))
        self.assertTrue(valid_peer("/alice/main"))
        self.assertTrue(valid_peer("/k/abc123"))
        self.assertFalse(valid_peer("/alice/*"))
        self.assertTrue(valid_peer("/alice/*", wildcard=True))
        self.assertFalse(valid_peer("/alice/main\n"))
        self.assertFalse(valid_peer("https://evil.example"))


class FakeStreamResponse:
    def __init__(self, text, content_type="text/event-stream; charset=utf-8"):
        self.lines = io.BytesIO(text.encode() if isinstance(text, str) else text)
        self.headers = {"Content-Type": content_type}
        self.closed = False
    def readline(self, limit=-1):
        return self.lines.readline(limit)
    def read(self, limit=-1):
        return b"{}"
    def close(self):
        self.closed = True
    def __enter__(self):
        return self
    def __exit__(self, *exc):
        self.close()


class FakeOpener:
    """Scripted urllib opener: each entry is a response, an HTTP status to raise, or an exception."""
    def __init__(self, script, done=None):
        self.script, self.requests, self.done = list(script), [], done
    def open(self, request, timeout=None):
        self.requests.append(request)
        if not self.script:
            if self.done:
                self.done.set()
            raise urllib.error.URLError("script finished")
        step = self.script.pop(0)
        if isinstance(step, int):
            raise urllib.error.HTTPError(request.full_url, step, "status", {}, io.BytesIO(b'{"error":"x"}'))
        if isinstance(step, Exception):
            raise step
        return step


class FakeCallSession:
    def __init__(self, fail=None):
        self.calls, self.fail = [], fail
    def access_token(self, rejected=None):
        self.calls.append(rejected)
        if self.fail:
            raise self.fail
        return "renewed" if rejected else "member"


class FakeNotifier:
    def __init__(self):
        self.shown, self.withdrawn = [], []
    def show(self, room, title, body):
        self.shown.append((room, title, body))
    def withdraw(self, room):
        self.withdrawn.append(room)


class FakeRingtone:
    def __init__(self):
        self.events = []
    def start(self):
        self.events.append("start")
    def stop(self):
        self.events.append("stop")


ROOM_ID = "AbCdEfGhIjKlMnOpQrStUv"
SESSION_ID = "SeSsIoN0123456789abcd"


def sse(event, data):
    return f"event: {event}\ndata: {json.dumps(data)}\n\n"


def incoming(room=ROOM_ID, kind="video", address="/k/bob00000000000000000000000b", handle="/bob", to="/alice"):
    return sse("incoming", {"room": room, "code": "ABC234", "kind": kind,
                            "from": {"address": address, "handle": handle}, "to": to})


class CallTests(unittest.TestCase):
    def client(self, script, session=None, **kwargs):
        done = threading.Event()
        opener = FakeOpener(script, done)
        notifier, ringtone, timers, opened = FakeNotifier(), FakeRingtone(), [], []
        client = CallAlerts(session or FakeCallSession(), notifier, opened.append, opener=opener,
                            ringtone=ringtone, later=lambda seconds, fn: timers.append((seconds, fn)), **kwargs)
        client._backoff = lambda stopped, failures: (stopped.wait(0.005), failures + 1)[1]
        return client, opener, notifier, ringtone, timers, opened, done

    def run_until_done(self, client, done):
        client.start()
        self.assertTrue(done.wait(5))
        client.stop()
        client.thread.join(5)

    def test_sse_parser_frames_and_ignores_noise(self):
        parser = SSEParser()
        frames = [parser.feed(line) for line in
                  [b": keepalive\n", b"\n", b"id: 7\n", b"retry: 10\n", b"event: ready\r\n", b"data: {\"a\":\n",
                   b"data: 1}\n", b"\n", b"data:x\n", b"\n"]]
        self.assertEqual([f for f in frames if f], [("ready", '{"a":\n1}'), ("message", "x")])

    def test_sse_parser_drops_oversized_and_invalid_events(self):
        parser = SSEParser()
        chunk = b"data: " + b"x" * 10000 + b"\n"
        for _ in range(7):
            self.assertIsNone(parser.feed(chunk))
        self.assertIsNone(parser.feed(b"\n"))  # over 64 KiB in total: dropped whole
        self.assertIsNone(parser.feed(b"data: \xff\xfe\n"))
        self.assertIsNone(parser.feed(b"\n"))  # invalid UTF-8 poisons only that event
        parser.feed(b"event: ring_ended\n")
        parser.feed(b"data: {}\n")
        self.assertEqual(parser.feed(b"\n"), ("ring_ended", "{}"))
        with self.assertRaises(ProtocolError):
            parser.feed(b"data: " + b"y" * 20000 + b"\n")

    def test_decode_keeps_only_known_json_objects(self):
        self.assertEqual(decode("ready", '{"session":"s"}'), ("ready", {"session": "s"}))
        self.assertIsNone(decode("signal", "{}"))
        self.assertIsNone(decode("incoming", "[1]"))
        self.assertIsNone(decode("incoming", "{not json"))

    def test_ring_validation(self):
        ring = Ring.parse(json.loads(incoming().split("data: ")[1]))
        self.assertEqual((ring.room, ring.kind, ring.handle, ring.to, ring.code), (ROOM_ID, "video", "/bob", "/alice", "ABC234"))
        payload = lambda **kw: json.loads(incoming(**kw).split("data: ")[1])
        self.assertIsNone(Ring.parse(payload(room="short")))
        self.assertIsNone(Ring.parse(payload(room="../../etc/passwd/xx")))
        self.assertIsNone(Ring.parse(payload(kind="screen")))
        self.assertIsNone(Ring.parse(payload(address="/<b>bob</b>")))
        self.assertIsNone(Ring.parse(payload(address="https://evil.example")))
        self.assertIsNone(Ring.parse(payload(to="/alice\n")))
        self.assertIsNone(Ring.parse(payload(handle="/a&b")).handle)  # bad handle falls back to the address
        self.assertIsNone(Ring.parse({"room": ROOM_ID, "kind": "audio", "from": "/bob", "to": "/alice"}))
        self.assertTrue(valid_room("a" * 64) and not valid_room("a" * 65) and not valid_room(None))
        self.assertTrue(valid_address("/k/abc") and not valid_address("/bekir/../alp") and not valid_address("/"))

    def test_answer_url_and_plain_text(self):
        self.assertEqual(answer_url(ROOM_ID), "https://inbox.pigeonpost.dev/?answer=" + ROOM_ID)
        with self.assertRaises(ValueError):
            answer_url("x&next=https://evil.example")
        self.assertEqual(plain_text("<b>Alp</b> & ‮co‬\n  new"), "bAlp/b co new")
        self.assertEqual(len(plain_text("a" * 500)), 60)
        self.assertEqual(plain_text("‏"), "Unknown caller")

    def test_https_is_required(self):
        with self.assertRaises(ValueError):
            CallAlerts(FakeCallSession(), FakeNotifier(), print, base="http://inbox.pigeonpost.dev/calls")
        with self.assertRaises(ValueError):
            CallAlerts(FakeCallSession(), FakeNotifier(), print, base="http://example.com/calls", insecure_loopback=True)
        CallAlerts(FakeCallSession(), FakeNotifier(), print, base="http://127.0.0.1:9/calls", insecure_loopback=True)

    def test_ring_notifies_and_ring_end_withdraws(self):
        stream = FakeStreamResponse(incoming(handle="/bob") + sse("ready", {"session": SESSION_ID, "mailboxes": []})
                                    + ": keepalive\n\n" + sse("signal", {"x": 1}) + incoming() + incoming()
                                    + sse("ring_ended", {"room": ROOM_ID, "reason": "answered_elsewhere"}))
        client, opener, notifier, ringtone, timers, _, done = self.client([stream])
        self.run_until_done(client, done)
        request = opener.requests[0]
        self.assertEqual(request.full_url, "https://inbox.pigeonpost.dev/calls/v1/events")
        self.assertEqual(request.get_header("Authorization"), "Bearer member")
        self.assertIsNone(request.get_header("Origin"))
        # The ring before `ready` is ignored, and a repeated ring does not notify twice.
        self.assertEqual(notifier.shown, [(ROOM_ID, "Incoming video call", "/bob")])
        self.assertIn(ROOM_ID, notifier.withdrawn)
        self.assertEqual(ringtone.events[0], "start")
        self.assertEqual(ringtone.events[-1], "stop")
        self.assertEqual(timers[0][0], 60)
        self.assertTrue(stream.closed)

    def test_name_helper_is_used_and_sanitized(self):
        stream = FakeStreamResponse(sse("ready", {"session": SESSION_ID}) + incoming(kind="audio", handle="/alp/main"))
        client, _, notifier, _, _, _, done = self.client(
            [stream], name_for=lambda address, handle: "<i>Alp</i> (" + display_name(handle) + ")")
        client.start()
        self.assertTrue(done.wait(5))
        self.assertEqual(notifier.shown, [(ROOM_ID, "Incoming voice call", "iAlp/i (/alp)")])
        client.stop()

    def test_expiry_and_stream_loss_withdraw(self):
        stream = FakeStreamResponse(sse("ready", {"session": SESSION_ID}) + incoming())
        client, _, notifier, _, timers, _, done = self.client([stream])
        client.start()
        self.assertTrue(done.wait(5))
        # The stream ended without a ring end; the alert is withdrawn because that session is gone.
        self.assertEqual(notifier.withdrawn, [ROOM_ID])
        timers[0][1]()  # a late expiry for an already withdrawn ring is harmless
        self.assertEqual(notifier.withdrawn, [ROOM_ID])
        client.stop()

    def test_expiry_withdraws_a_ring_without_an_end(self):
        client = CallAlerts(FakeCallSession(), FakeNotifier(), print, ringtone=FakeRingtone(),
                            later=lambda seconds, fn: fn())
        ring = Ring.parse(json.loads(incoming().split("data: ")[1]))
        ring.generation = client.generation
        client._incoming(ring)
        self.assertEqual(client.notifier.withdrawn, [ROOM_ID])
        self.assertEqual(client.rings, {})

    def test_unauthorized_renews_once_then_continues(self):
        stream = FakeStreamResponse(sse("ready", {"session": SESSION_ID}))
        session = FakeCallSession()
        client, opener, _, _, _, _, done = self.client([401, stream], session=session)
        self.run_until_done(client, done)
        self.assertEqual(session.calls[:2], [None, "member"])
        self.assertEqual(opener.requests[1].get_header("Authorization"), "Bearer renewed")

    def test_repeated_unauthorized_stops(self):
        client, opener, _, _, _, _, done = self.client([401, 401, FakeStreamResponse("")])
        client.start()
        client.thread.join(5)
        self.assertFalse(client.running)
        self.assertEqual(len(opener.requests), 2)
        self.assertEqual(client.state, "signed_out")

    def test_signed_out_session_and_no_mailbox_stop(self):
        client, opener, *_ = self.client([], session=FakeCallSession(APIError(401, "invalid_grant")))
        client.start()
        client.thread.join(5)
        self.assertEqual(opener.requests, [])
        client, opener, *_ = self.client([403])
        client.start()
        client.thread.join(5)
        self.assertEqual(len(opener.requests), 1)
        self.assertFalse(client.running)

    def test_non_stream_response_and_errors_reconnect(self):
        stream = FakeStreamResponse(sse("ready", {"session": SESSION_ID}))
        client, opener, _, _, _, _, done = self.client(
            [FakeStreamResponse("{}", "application/json"), 502, urllib.error.URLError("down"), stream])
        self.run_until_done(client, done)
        self.assertGreaterEqual(len(opener.requests), 5)  # four scripted failures, the stream, then retries

    def ringing(self):
        client = CallAlerts(FakeCallSession(), FakeNotifier(), [].append, ringtone=FakeRingtone(),
                            later=lambda *a: None, opener=FakeOpener([FakeStreamResponse("{}")]))
        client.opened = []
        client.open_url = client.opened.append
        client.session_id = SESSION_ID
        ring = Ring.parse(json.loads(incoming().split("data: ")[1]))
        ring.session, ring.generation = SESSION_ID, client.generation
        client._incoming(ring)
        return client

    def test_answer_opens_the_web_inbox(self):
        client = self.ringing()
        self.assertFalse(client.answer("bad room"))
        self.assertTrue(client.answer(ROOM_ID))
        self.assertEqual(client.opened, ["https://inbox.pigeonpost.dev/?answer=" + ROOM_ID])
        self.assertEqual(client.notifier.withdrawn, [ROOM_ID])
        self.assertEqual(client.ringtone.events, ["start", "stop"])

    def test_decline_posts_this_session(self):
        client = self.ringing()
        self.assertTrue(client.decline(ROOM_ID, background=False))
        request = client.opener.requests[0]
        self.assertEqual(request.full_url, f"https://inbox.pigeonpost.dev/calls/v1/rooms/{ROOM_ID}/decline")
        self.assertEqual(request.get_method(), "POST")
        self.assertEqual(json.loads(request.data), {"session": SESSION_ID})
        self.assertEqual(request.get_header("Authorization"), "Bearer member")
        self.assertIsNone(request.get_header("Origin"))
        self.assertEqual(client.notifier.withdrawn, [ROOM_ID])
        # Unknown or already-ended rings send nothing.
        self.assertFalse(client.decline(ROOM_ID, background=False))
        self.assertFalse(client.decline("../../x", background=False))
        self.assertEqual(len(client.opener.requests), 1)

    def test_decline_renews_once_on_401(self):
        client = self.ringing()
        client.opener = FakeOpener([401, 401])
        self.assertFalse(client.decline(ROOM_ID, background=False))
        self.assertEqual([r.get_header("Authorization") for r in client.opener.requests], ["Bearer member", "Bearer renewed"])
        client = self.ringing()
        client.opener = FakeOpener([401, FakeStreamResponse("{}")])
        self.assertTrue(client.decline(ROOM_ID, background=False))

    def test_decline_after_reconnect_sends_nothing(self):
        client = self.ringing()
        client.session_id = "AnOtHeRsEsSiOn0123456"
        self.assertFalse(client.decline(ROOM_ID, background=False))
        self.assertEqual(client.opener.requests, [])

    def test_stop_withdraws_and_ignores_late_events(self):
        client = self.ringing()
        old = client.generation
        client.stop()
        self.assertEqual(client.notifier.withdrawn, [ROOM_ID])
        ring = Ring.parse(json.loads(incoming().split("data: ")[1]))
        ring.generation = old
        client._incoming(ring)
        self.assertEqual(len(client.notifier.shown), 1)

    def test_ringtone_is_optional(self):
        spawned = []
        class Process:
            def terminate(self):
                spawned.append("terminated")
        Ringtone(which=lambda name: None, spawn=lambda *a, **k: spawned.append(a)).start()
        self.assertEqual(spawned, [])
        tone = Ringtone(which=lambda name: "/usr/bin/" + name, spawn=lambda args, **k: spawned.append(args) or Process())
        tone.start()
        tone.start()
        tone.stop()
        self.assertEqual(spawned, [["/usr/bin/canberra-gtk-play", "--id=phone-incoming-call", "--loop=15",
                                    "--description=Incoming call"], "terminated"])
        failing = Ringtone(which=lambda name: "/x", spawn=lambda *a, **k: (_ for _ in ()).throw(OSError()))
        failing.start()
        failing.stop()


if __name__ == "__main__":
    unittest.main()
