import concurrent.futures
import threading
import time
import unittest

from pigeonpost.api import APIError, NoRedirect, Postbox, Transport
from pigeonpost.auth import Session
from pigeonpost.model import (contact_for, conversations, message_text, safe_filename,
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
        for _, url, _ in transport.calls:
            self.assertIn("include_read=true", url)
            self.assertIn("include_sent=true", url)
            self.assertIn("identity=%2Fk%2Ftest", url)

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

    def test_peer_validation(self):
        self.assertTrue(valid_peer("/alice/main"))
        self.assertTrue(valid_peer("/k/abc123"))
        self.assertFalse(valid_peer("/alice/*"))
        self.assertTrue(valid_peer("/alice/*", wildcard=True))
        self.assertFalse(valid_peer("/alice/main\n"))
        self.assertFalse(valid_peer("https://evil.example"))


if __name__ == "__main__":
    unittest.main()
