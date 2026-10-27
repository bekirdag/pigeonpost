"""Organization rooms: catalog, directory, policy hints and room-creation custody (pure)."""
import base64
import json
import threading
import unittest

from pigeonpost import orgs_core as oc
from pigeonpost.api import APIError
from pigeonpost.binding import Lease, Retired
from pigeonpost.protected import MemoryProtectedStore

MINE = "/k/" + "m" * 26
PERSONAL = "/k/" + "p" * 26
AGENT = "/k/" + "g" * 26


def catalog(**org):
    base = {"id": "org_1", "name": "Acme", "namespace": "acme", "status": "active", "my_member_id": "mem_1",
            "my_roles": [], "settings": {"rooms_creation": "everyone"}, "my_mailbox": {"address": MINE, "handle": "/acme/john"}}
    base.update(org)
    return {"orgs": [base], "operated": [{"address": AGENT, "handle": "/acme/agent1", "org_id": "org_1",
                                          "capabilities": ["read_inbox", "enroll"]}]}


class Session:
    def __init__(self):
        self.lock = threading.RLock()
        self.epoch, self.owner = 1, "owner-a"


class CatalogTests(unittest.TestCase):
    def test_owned_and_operated_stay_apart(self):
        c = oc.parse_catalog(catalog())
        self.assertEqual(oc.owned_org_mailboxes(c, [{"address": PERSONAL}, {"address": MINE}]), {"org_1": MINE})
        self.assertEqual(oc.owned_org_mailboxes(c, [{"address": PERSONAL}]), {})
        agents = oc.operated_agents(c)
        self.assertEqual([(a["address"], a["org_name"]) for a in agents], [(AGENT, "Acme")])
        # Even if an operated agent's address appeared in the account's list, it is not an org inbox.
        self.assertNotIn(AGENT, oc.owned_org_mailboxes(c, [{"address": AGENT}]).values())

    def test_strict_parsing(self):
        for bad in (dict(id="../x"), dict(name=""), dict(name="A‮B"), dict(my_roles=["admin", "admin"]),
                    dict(my_mailbox={"address": "/acme/john"}), dict(my_member_id=None)):
            with self.assertRaises(oc.OrgError, msg=str(bad)):
                oc.parse_catalog(catalog(**bad))
        raw = catalog()
        raw["operated"][0]["org_id"] = "org_other"
        with self.assertRaises(oc.OrgError):
            oc.parse_catalog(raw)
        raw = catalog()
        raw["orgs"].append(dict(raw["orgs"][0]))
        with self.assertRaises(oc.OrgError):
            oc.parse_catalog(raw)

    def test_creation_hint(self):
        org = oc.parse_catalog(catalog())["orgs"][0]
        self.assertTrue(oc.may_create(org, MINE))
        self.assertFalse(oc.may_create(org, PERSONAL))  # acting as another inbox
        org = oc.parse_catalog(catalog(settings={"rooms_creation": "admins"}))["orgs"][0]
        self.assertFalse(oc.may_create(org, MINE))
        org = oc.parse_catalog(catalog(settings={"rooms_creation": "admins"}, my_roles=["people_admin"]))["orgs"][0]
        self.assertTrue(oc.may_create(org, MINE))
        org = oc.parse_catalog(catalog(status="read_only"))["orgs"][0]
        self.assertFalse(oc.may_create(org, MINE))
        self.assertTrue(oc.readable(org))

    def test_rooms_and_actions(self):
        rooms = oc.parse_rooms({"rooms": [
            {"id": "grp_a", "name": "General", "description": None, "member_count": 3, "join_policy": "public", "my_state": "active"},
            {"id": "grp_b", "name": "Design", "member_count": 1, "join_policy": "public", "my_state": None},
            {"id": "grp_c", "name": "Board", "member_count": 1, "join_policy": "private", "my_state": "invited"},
            {"id": "grp_d", "name": "Secret", "member_count": 1, "join_policy": "private", "my_state": None}]})
        self.assertEqual([oc.room_action(r) for r in rooms], ["open", "join", "accept", "unavailable"])
        org = oc.parse_catalog(catalog(status="read_only"))["orgs"][0]
        self.assertEqual([oc.room_action(r, org) for r in rooms], ["open", "unavailable", "unavailable", "unavailable"])
        for bad in ({"rooms": [{"id": "x y", "name": "a", "member_count": 1, "join_policy": "public"}]},
                    {"rooms": [{"id": "x", "name": "a", "member_count": -1, "join_policy": "public"}]},
                    {"rooms": [{"id": "x", "name": "a", "member_count": 1, "join_policy": "public"}] * 2},
                    {"rooms": [{"id": str(i), "name": "a", "member_count": 1, "join_policy": "public"} for i in range(51)]}):
            with self.assertRaises(oc.OrgError):
                oc.parse_rooms(bad)


class CustodyTests(unittest.TestCase):
    def setUp(self):
        self.session = Session()
        self.lease = Lease(self.session).child(MINE)
        self.store = MemoryProtectedStore()

    def test_exact_bytes_and_conflicts(self):
        wire = oc.room_request("org_1", " Design ", "Mockups", request_key="rk_" + "a" * 16)
        doc = json.loads(wire)
        self.assertEqual(doc["name"], "Design")
        self.assertEqual(doc["visibility"], "listed")
        self.assertEqual(oc.parse_room_request(wire)["request_key"], "rk_" + "a" * 16)
        with self.assertRaises(ValueError):
            oc.parse_room_request(wire.replace(b"Design", b"Desig2").replace(b'"name"', b'"name"', 1) + b" ")
        self.store.update(self.lease, MINE, lambda r: oc.save_pending(r, wire, now=10))
        self.store.update(self.lease, MINE, lambda r: oc.save_pending(r, wire, now=11))  # same bytes: no-op
        rows = oc.pending_rooms(self.store.read(self.lease, MINE))
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["wire"], wire)
        changed = oc.room_request("org_1", "Design 2", request_key="rk_" + "a" * 16)
        with self.assertRaises(ValueError):
            self.store.update(self.lease, MINE, lambda r: oc.save_pending(r, changed))
        other = oc.room_request("org_1", "Other")
        with self.assertRaises(ValueError):
            self.store.update(self.lease, MINE, lambda r: oc.save_pending(r, other))
        self.assertFalse(self.store.update(self.lease, MINE, lambda r: oc.remove_pending(r, "rk_other1234")))
        self.assertTrue(self.store.update(self.lease, MINE, lambda r: oc.remove_pending(r, "rk_" + "a" * 16)))
        self.assertEqual(oc.pending_rooms(self.store.read(self.lease, MINE)), [])

    def test_bound_to_login_and_mailbox(self):
        wire = oc.room_request("org_1", "Design")
        self.store.update(self.lease, MINE, lambda r: oc.save_pending(r, wire))
        other_mailbox = Lease(self.session).child(PERSONAL)
        self.assertEqual(oc.pending_rooms(self.store.read(other_mailbox, PERSONAL)), [])
        self.session.epoch += 1
        self.session.owner = "owner-b"
        with self.assertRaises(Retired):
            self.store.read(self.lease, MINE)
        self.assertEqual(oc.pending_rooms(self.store.read(Lease(self.session), MINE)), [])

    def test_corrupt_saved_request_is_refused(self):
        record = {"orgs": {"pending_rooms": [{"request_key": "rk_" + "a" * 16, "org_id": "org_1",
                                              "wire": base64.b64encode(b'{"name":"x"}').decode()}]}}
        with self.assertRaises(oc.OrgError):
            oc.pending_rooms(record)

    def test_uncertain_classification(self):
        self.assertTrue(oc.uncertain(APIError()))
        self.assertTrue(oc.uncertain(APIError(502, "bad_gateway")))
        self.assertTrue(oc.uncertain(Retired()))
        self.assertFalse(oc.uncertain(APIError(403, "forbidden")))
        self.assertFalse(oc.uncertain(APIError(429, "rate_limited")))
        self.assertFalse(oc.uncertain(oc.OrgError("x")))
        self.assertEqual(oc.retry_after(APIError(429, "rate_limited", headers={"retry-after": "99999"})), 3600)


class ApiTests(unittest.TestCase):
    class Raw:
        def __init__(self, result):
            self.result, self.calls = result, []

        def call(self, method, path, identity=None, data=None, **kw):
            self.calls.append((method, path, identity, data))
            return self.result

        def scoped(self, method, path, identity, data=None, **kw):
            self.calls.append((method, path, identity, data, kw))
            return self.result

    def test_routes(self):
        raw = self.Raw({"rooms": []})
        api = oc.OrgsAPI(raw)
        api.rooms(MINE, "org_1", "des ign")
        self.assertEqual(raw.calls[-1][1], "/v1/rooms?org_id=org_1&q=des+ign")
        self.assertEqual(raw.calls[-1][2], MINE)
        with self.assertRaises(oc.OrgError):
            api.rooms(MINE, "org/1")
        wire = oc.room_request("org_1", "Design")
        raw.result = {"group": {"id": "grp_new", "org_id": "org_1", "created_by": MINE, "my": {"role": "owner", "state": "keying"}}}
        self.assertEqual(api.create_exact(MINE, wire)["id"], "grp_new")
        self.assertEqual(raw.calls[-1][4]["raw"], wire)
        raw.result = {"group": {"id": "grp_new", "org_id": "org_2", "created_by": MINE, "my": {"role": "owner", "state": "keying"}}}
        with self.assertRaises(oc.OrgError):
            api.create_exact(MINE, wire)  # not the room this request created


if __name__ == "__main__":
    unittest.main()
