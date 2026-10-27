"""Device approval and strong sign-in: pure rules, the approval state machine and the loopback
PKCE flow against a fake issuer. Synthetic only; no network beyond 127.0.0.1."""
import base64
import hashlib
import http.server
import json
import threading
import time
import unittest
import urllib.error
import urllib.parse
import urllib.request

from pigeonpost.api import APIError, Transport
from pigeonpost.binding import Lease, Retired, owner_key
from pigeonpost.pairing import (APPROVED, DENIED, EXPIRED, REVIEW, UNAVAILABLE, UNCONFIRMED, ApprovalStore,
                                PairingAPI, browser_link, check_preview, parse_code, parse_input, parse_link)
from pigeonpost.strong_signin import (LoopbackReceiver, StrongSignIn, StrongSignInError, StrongSignInRequirement,
                                      bearer_parameters, supported)

NOW = 1_800_000_000
ADDRESS = "/k/" + "a" * 26


def jwt(claims):
    enc = lambda d: base64.urlsafe_b64encode(json.dumps(d).encode()).rstrip(b"=").decode()
    return enc({"alg": "none"}) + "." + enc(claims) + ".sig"


class FakeSession:
    def __init__(self, owner="owner-1"):
        self.lock = threading.RLock()
        self.epoch, self.owner = 1, owner
        self.accepted = []

    def _accept(self, result):
        self.accepted.append(result)
        self.owner = owner_key(result["access_token"])


def preview(**overrides):
    raw = {"pairing_id": "pair_abc", "org": {"id": "org_1", "name": "Acme"}, "handle": "/acme/agent1",
           "address": ADDRESS, "creates_agent": False, "label": "Build box", "confirmation": "123 456",
           "capabilities": ["mailbox"], "ip_country": None, "requested_at": NOW - 10, "expires_at": NOW + 600,
           "state": "pending", "can_approve": True, "reason": None}
    raw.update(overrides)
    return raw


class FakePairingAPI:
    def __init__(self):
        self.calls = []
        self.resolve_results = []
        self.approve_result = None
        self.deny_result = None
        self.gate = None

    def resolve(self, code):
        self.calls.append(("resolve", code.display))
        if self.gate:
            self.gate.wait(5)
        result = self.resolve_results.pop(0) if len(self.resolve_results) > 1 else self.resolve_results[0]
        if isinstance(result, Exception):
            raise result
        return check_preview(result)

    def approve(self, p, code):
        self.calls.append(("approve", p["pairing_id"], code.display))
        if isinstance(self.approve_result, Exception):
            raise self.approve_result
        return {"approved": True}

    def deny(self, p, code):
        self.calls.append(("deny", p["pairing_id"], code.display))
        if isinstance(self.deny_result, Exception):
            raise self.deny_result
        return {"denied": True}


class CodeTests(unittest.TestCase):
    def test_codes_and_links(self):
        self.assertEqual(parse_code(" abcd-efgh ").display, "ABCD-EFGH")
        self.assertEqual(parse_code("ABCDEFGH").display, "ABCD-EFGH")
        for bad in ("ABCD-EFG", "ABCD_EFGH", "ABCD-EFGHI", "ÄBCD-EFGH", "", "x" * 200):
            self.assertIsNone(parse_code(bad), bad)
        self.assertEqual(parse_link("https://inbox.pigeonpost.dev/pair.html#ABCD-EFGH").display, "ABCD-EFGH")
        for bad in ("https://inbox.pigeonpost.dev/pair.html?c=ABCD-EFGH", "http://inbox.pigeonpost.dev/pair.html#ABCD-EFGH",
                    "https://evil.example/pair.html#ABCD-EFGH", "https://inbox.pigeonpost.dev:8443/pair.html#ABCD-EFGH",
                    "https://user@inbox.pigeonpost.dev/pair.html#ABCD-EFGH", "https://inbox.pigeonpost.dev/other#ABCD-EFGH",
                    "https://inbox.pigeonpost.dev/pair.html#ABCD-EFGH ", "https://inbox.pigeonpost.dev/pair.html?#ABCD-EFGH"):
            self.assertIsNone(parse_link(bad), bad)
        self.assertEqual(parse_input("https://inbox.pigeonpost.dev/pair.html#wxyz-2345").display, "WXYZ-2345")
        code = parse_code("ABCD-EFGH")
        self.assertNotIn("ABCD", repr(code) + str(code))
        self.assertEqual(browser_link(code), "https://inbox.pigeonpost.dev/pair.html#ABCD-EFGH")
        self.assertNotIn("?", browser_link(code))

    def test_preview_validation(self):
        p = check_preview(preview(confirmation="123456"))
        self.assertEqual(p["confirmation"], "123 456")
        self.assertFalse(p["creates_agent"])
        self.assertTrue(check_preview(preview(address=None, creates_agent=True))["creates_agent"])
        for bad in (dict(address=None, creates_agent=False), dict(address=ADDRESS, creates_agent=True),
                    dict(handle="/k/abc/x"), dict(handle="/acme"), dict(confirmation="12345"),
                    dict(capabilities=[]), dict(capabilities=["mailbox", "mailbox"]), dict(state="weird"),
                    dict(expires_at=NOW - 20), dict(org={"id": "o", "name": "A‮B"}), dict(label="a\nb"),
                    dict(pairing_id="../x"), dict(can_approve="yes")):
            with self.assertRaises(ValueError, msg=str(bad)):
                check_preview(preview(**bad))

    def test_api_routes_and_acknowledgments(self):
        class Raw:
            def __init__(self):
                self.calls = []
                self.result = {}

            def call(self, method, path, identity=None, data=None, **kw):
                self.calls.append((method, path, identity, data))
                return self.result
        raw = Raw()
        api = PairingAPI(raw)
        code = parse_code("ABCD-EFGH")
        raw.result = preview()
        api.resolve(code)
        self.assertEqual(raw.calls[-1], ("POST", "/v1/device-pairings/resolve", None, {"user_code": "ABCD-EFGH"}))
        p = check_preview(preview())
        raw.result = {"approved": True, "address": ADDRESS, "handle": "/acme/agent1"}
        api.approve(p, code)
        self.assertEqual(raw.calls[-1][1], "/v1/device-pairings/pair_abc/approve")
        raw.result = {"approved": True, "address": "/k/" + "b" * 26, "handle": "/acme/agent1"}
        with self.assertRaises(APIError):
            api.approve(p, code)  # an acknowledgment for another agent is not this approval
        raw.result = {"ok": True}
        with self.assertRaises(APIError):
            api.deny(p, code)


class StoreTests(unittest.TestCase):
    def setUp(self):
        self.session = FakeSession()
        self.lease = Lease(self.session)
        self.api = FakePairingAPI()
        self.clock = [NOW]

    def store(self, strong=True):
        return ApprovalStore(self.lease, self.api, now=lambda: self.clock[0], strong_available=strong)

    def test_review_then_hold_to_approve(self):
        self.api.resolve_results = [preview()]
        s = self.store()
        self.assertTrue(s.resolve("abcd-efgh"))
        snap = s.snapshot()
        self.assertEqual(snap["phase"], REVIEW)
        self.assertTrue(snap["can_approve"])
        self.assertFalse(s.approve("stale-proof"))
        self.assertTrue(s.approve(snap["proof"]))
        self.assertEqual(s.snapshot()["phase"], APPROVED)
        self.assertFalse(s.approve(s.snapshot()["proof"]))  # a decided request offers no other approval
        self.assertEqual([c[0] for c in self.api.calls], ["resolve", "approve"])

    def test_production_gate_sends_to_browser(self):
        self.api.resolve_results = [preview()]
        s = self.store(strong=False)
        s.resolve("ABCD-EFGH")
        snap = s.snapshot()
        self.assertFalse(snap["can_approve"])
        self.assertTrue(snap["needs_browser"])
        self.assertTrue(snap["can_deny"])
        self.assertFalse(s.approve(snap["proof"]))

    def test_lost_approval_is_unconfirmed_and_never_resent(self):
        self.api.resolve_results = [preview()]
        self.api.approve_result = APIError(0, "network_error")
        s = self.store()
        s.resolve("ABCD-EFGH")
        s.approve(s.snapshot()["proof"])
        snap = s.snapshot()
        self.assertEqual(snap["phase"], UNCONFIRMED)
        self.assertFalse(snap["can_approve"])
        self.assertEqual([c[0] for c in self.api.calls].count("approve"), 1)
        self.api.resolve_results = [preview(state="approved", can_approve=False)]
        s.check_status()
        self.assertEqual(s.snapshot()["phase"], APPROVED)
        self.assertEqual([c[0] for c in self.api.calls].count("approve"), 1)

    def test_deny_resolves_again(self):
        self.api.resolve_results = [preview()]
        s = self.store(strong=False)
        s.resolve("ABCD-EFGH")
        self.api.resolve_results = [preview(state="approved", can_approve=False)]
        s.deny(s.snapshot()["proof"])
        self.assertEqual(s.snapshot()["phase"], APPROVED)  # a concurrent approval won; say so
        self.api.resolve_results = [preview()]
        s.resolve("ABCD-EFGH")
        self.api.resolve_results = [preview(state="denied", can_approve=False)]
        s.deny(s.snapshot()["proof"])
        self.assertEqual(s.snapshot()["phase"], DENIED)
        self.api.resolve_results = [preview()]
        s.resolve("ABCD-EFGH")
        self.api.resolve_results = [preview()]
        s.deny(s.snapshot()["proof"])
        self.assertEqual(s.snapshot()["phase"], UNCONFIRMED)

    def test_cannot_approve_reason_expiry_and_errors(self):
        self.api.resolve_results = [preview(can_approve=False, reason="delegation_required")]
        s = self.store()
        s.resolve("ABCD-EFGH")
        snap = s.snapshot()
        self.assertFalse(snap["can_approve"] or snap["can_deny"])
        self.assertIn("operators", snap["message"])
        self.api.resolve_results = [preview()]
        s.resolve("ABCD-EFGH")
        self.clock[0] = NOW + 601
        self.assertFalse(s.approve(s.snapshot()["proof"]))
        self.assertEqual(s.snapshot()["phase"], EXPIRED)
        self.clock[0] = NOW
        self.api.resolve_results = [APIError(404, "pairing_not_found")]
        s.resolve("ABCD-EFGH")
        self.assertEqual(s.snapshot()["phase"], UNAVAILABLE)
        self.api.resolve_results = [APIError(429, "rate_limited", headers={"retry-after": "20"})]
        s.resolve("ABCD-EFGH")
        self.assertEqual(s.snapshot()["retry_at"], NOW + 20)
        self.assertFalse(s.resolve("ABCD-EFGH"))  # waits for Retry-After
        self.clock[0] = NOW + 21
        self.api.resolve_results = [preview(expires_at=NOW + 900)]
        self.assertTrue(s.resolve("ABCD-EFGH"))

    def test_step_up_challenge_needs_another_decision(self):
        self.api.resolve_results = [preview()]
        challenge = APIError(401, "step_up_required", body={"error": "step_up_required"},
                             headers={"www-authenticate": 'Bearer error="insufficient_user_authentication", acr_values="mfa", max_age=600'})
        self.api.approve_result = challenge
        s = self.store()
        s.resolve("ABCD-EFGH")
        s.approve(s.snapshot()["proof"])
        snap = s.snapshot()
        self.assertEqual(snap["phase"], REVIEW)
        self.assertEqual(snap["challenge"], StrongSignInRequirement(["mfa"], 600))
        self.assertFalse(snap["can_approve"])
        self.api.approve_result = None
        signed = []
        self.assertTrue(s.verify(snap["proof"], lambda requirement: signed.append(requirement)))
        self.assertEqual(signed, [StrongSignInRequirement(["mfa"], 600)])
        self.assertEqual([c[0] for c in self.api.calls], ["resolve", "approve", "resolve"])  # no automatic approval
        self.assertEqual(s.snapshot()["phase"], REVIEW)
        self.assertTrue(s.snapshot()["can_approve"])
        # Without strong sign-in in this client, the same challenge points at the browser.
        gated = self.store(strong=False)
        gated.resolve("ABCD-EFGH")
        with gated.lock:
            gated._failed(challenge, True)
        snap = gated.snapshot()
        self.assertIsNone(snap["challenge"])
        self.assertIn("browser", snap["message"])
        self.assertFalse(gated.verify(snap["proof"], lambda r: self.fail("no sign-in without the capability")))

    def test_retired_login_drops_late_answers(self):
        self.api.resolve_results = [preview()]
        self.api.gate = threading.Event()
        s = self.store()
        worker = threading.Thread(target=s.resolve, args=("ABCD-EFGH",))
        worker.start()
        time.sleep(0.05)
        self.session.epoch += 1  # another sign-in
        self.api.gate.set()
        worker.join(5)
        snap = s.snapshot()
        self.assertIsNone(snap["preview"])
        self.assertFalse(snap["can_approve"])
        self.assertFalse(s.resolve("ABCD-EFGH"))

    def test_abandoned_code_answer_is_dropped(self):
        self.api.resolve_results = [preview()]
        self.api.gate = threading.Event()
        s = self.store()
        worker = threading.Thread(target=s.resolve, args=("ABCD-EFGH",))
        worker.start()
        time.sleep(0.05)
        s.clear()
        self.api.gate.set()
        worker.join(5)
        self.assertIsNone(s.snapshot()["preview"])
        self.assertNotIn("ABCD", repr(s))


class RequirementTests(unittest.TestCase):
    def test_parsing(self):
        self.assertEqual(StrongSignInRequirement.parse(None, 'Bearer acr_values="a b", max_age=300'),
                         StrongSignInRequirement(["a", "b"], 300))
        self.assertEqual(StrongSignInRequirement.parse({"acr_values": ["x"], "max_age": 600}, None),
                         StrongSignInRequirement(["x"], 600))
        self.assertEqual(StrongSignInRequirement.parse({"acr_values": "x y", "max_age": 0}, "garbage"),
                         StrongSignInRequirement(["x", "y"], 0))
        for header in ('Bearer acr_values="a", acr_values="b", max_age=1', 'Bearer acr_values="a\\x", max_age=1',
                       'Bearer acr_values="a, max_age=1', 'Basic acr_values="a", max_age=1', "Bearer " + "a=b," * 600):
            self.assertIsNone(StrongSignInRequirement.parse(None, header), header)
        self.assertEqual(bearer_parameters('Bearer error_description="say \\"max_age=1\\", ok", realm=x'),
                         {"error_description": 'say "max_age=1", ok', "realm": "x"})
        for body in ({"acr_values": "a", "max_age": 601}, {"acr_values": "a a", "max_age": 1}, {"acr_values": [], "max_age": 1},
                     {"acr_values": "a<b", "max_age": 1}, {"acr_values": "a", "max_age": True}):
            self.assertIsNone(StrongSignInRequirement.parse(body, None), body)

    def test_only_the_configured_linux_client_is_enabled(self):
        self.assertTrue(supported("pigeonpost-linux"))
        self.assertFalse(supported("pigeonpost-cli"))
        self.assertFalse(supported("pigeonpost-windows"))


class FakeIssuer:
    """Token endpoint on 127.0.0.1 that issues synthetic tokens for one authorization code."""

    def __init__(self):
        issuer_self = self
        self.exchanges = []
        self.subject = "person-1"
        self.acr = "mfa"
        self.nonce = None

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_POST(self):
                form = dict(urllib.parse.parse_qsl(self.rfile.read(int(self.headers["Content-Length"])).decode()))
                issuer_self.exchanges.append(form)
                now = int(time.time())
                base = {"iss": issuer_self.url, "sub": issuer_self.subject, "exp": now + 300, "iat": now}
                body = json.dumps({"access_token": jwt(base), "refresh_token": "r2", "expires_in": 300,
                                   "id_token": jwt({**base, "aud": "test-client", "azp": "test-client", "nonce": issuer_self.nonce,
                                                    "acr": issuer_self.acr, "auth_time": now})}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(body)
        self.server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_port}/realms/test"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()


class LoopbackTests(unittest.TestCase):
    def setUp(self):
        self.issuer = FakeIssuer()
        self.addCleanup(self.issuer.close)
        self.owner = owner_key(jwt({"iss": self.issuer.url, "sub": "person-1"}))
        self.session = FakeSession(self.owner)
        self.lease = Lease(self.session)

    def browser(self, extra="", state=None):
        """Plays the browser: reads the authorize URL and returns to the redirect."""
        def open_url(url):
            query = dict(urllib.parse.parse_qsl(urllib.parse.urlsplit(url).query))
            self.issuer.nonce = query["nonce"]
            self.seen = query

            def go():
                redirect = query["redirect_uri"]
                assert redirect.startswith("http://127.0.0.1:") and redirect.endswith("/")
                # Junk first: must not consume the pending return.
                for junk in ("?code=x&state=wrong", "other?code=x&state=" + query["state"], "?code=a&code=b&state=" + query["state"]):
                    try:
                        urllib.request.urlopen(redirect + junk, timeout=5).read()
                    except urllib.error.HTTPError as error:
                        error.close()
                    except Exception:
                        pass
                with urllib.request.urlopen(redirect + "?" + urllib.parse.urlencode(
                        {"code": "auth-code", "state": state or query["state"]}) + extra, timeout=5) as response:
                    self.page = (response.read(), response.headers)
            threading.Thread(target=go, daemon=True).start()
        return open_url

    def flow(self, **kw):
        return StrongSignIn(self.session, Transport(allow_loopback_http=True), issuer=self.issuer.url,
                            client_id="test-client", clients={"test-client"}, overall=10, **kw)

    def test_verified_same_person_is_adopted(self):
        flow = self.flow(open_url=self.browser())
        self.assertTrue(flow.available)
        self.assertTrue(flow.run(StrongSignInRequirement(["mfa"], 600), self.lease))
        self.assertEqual(len(self.session.accepted), 1)
        self.assertEqual(self.seen["code_challenge_method"], "S256")
        self.assertEqual(self.seen["acr_values"], "mfa")
        exchange = self.issuer.exchanges[0]
        self.assertEqual(exchange["grant_type"], "authorization_code")
        self.assertEqual(base64.urlsafe_b64encode(hashlib.sha256(exchange["code_verifier"].encode()).digest()).rstrip(b"=").decode(),
                         self.seen["code_challenge"])
        self.assertEqual(exchange["redirect_uri"], self.seen["redirect_uri"])
        body, headers = self.page
        self.assertNotIn(b"auth-code", body)
        self.assertEqual(headers["Cache-Control"], "no-store")
        self.assertEqual(headers["Referrer-Policy"], "no-referrer")

    def test_other_person_or_weak_method_is_refused(self):
        self.issuer.subject = "someone-else"
        with self.assertRaises(StrongSignInError):
            self.flow(open_url=self.browser()).run(StrongSignInRequirement(["mfa"], 600), self.lease)
        self.issuer.subject = "person-1"
        self.issuer.acr = "pwd"
        with self.assertRaises(StrongSignInError):
            self.flow(open_url=self.browser()).run(StrongSignInRequirement(["mfa"], 600), self.lease)
        self.assertEqual(self.session.accepted, [])

    def test_retired_login_adopts_nothing(self):
        def open_url(url):
            self.session.epoch += 1  # signed out / another account while the browser was open
            self.browser()(url)
        with self.assertRaises(Retired):
            self.flow(open_url=open_url).run(StrongSignInRequirement(["mfa"], 600), self.lease)
        self.assertEqual(self.session.accepted, [])
        self.assertEqual(self.issuer.exchanges, [])

    def test_cancel_and_timeout_release_listener(self):
        receiver = LoopbackReceiver("s", overall=0.5)
        port = receiver.port
        with self.assertRaises(StrongSignInError):
            receiver.wait()
        with self.assertRaises(OSError):
            urllib.request.urlopen(f"http://127.0.0.1:{port}/?state=s&code=c", timeout=1)
        receiver = LoopbackReceiver("s")
        threading.Timer(0.2, receiver.cancel).start()
        with self.assertRaises(StrongSignInError):
            receiver.wait()

    def test_unavailable_client_never_opens_browser(self):
        opened = []
        # A client that is not configured for the loopback flow (an empty allow-list here).
        flow = StrongSignIn(self.session, Transport(allow_loopback_http=True), issuer=self.issuer.url, open_url=opened.append, clients=())
        self.assertFalse(flow.available)
        with self.assertRaises(StrongSignInError):
            flow.run(StrongSignInRequirement(["mfa"], 600), self.lease)
        self.assertEqual(opened, [])


if __name__ == "__main__":
    unittest.main()
