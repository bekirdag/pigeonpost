"""Nests rules without a GUI: parsing, approval binding, upload header, delete confirmation, gating."""
import unittest

from pigeonpost import nests_core as nc
from pigeonpost.api import APIError

H = "a" * 64


class Recorder:
    def __init__(self, answers=None):
        self.calls, self.answers = [], answers or {}

    def call(self, method, path, identity=None, data=None, **kwargs):
        self.calls.append((method, path, data, kwargs))
        return self.answers.get((method, path), {})


class CoreTests(unittest.TestCase):
    def test_flag_gates_everything(self):
        self.assertFalse(nc.nests_on(frozenset()))
        self.assertFalse(nc.nests_on(None))
        self.assertTrue(nc.nests_on(frozenset({"nests", "orgs"})))

    def test_packages_keep_only_sellable_and_reject_bad_shapes(self):
        raw = {"catalog_version": "nests-2026-11", "packages": [
            {"code": "nest.ai.starter.monthly", "name": "Pigeonpost AI", "slots": 1, "storage_bytes": 5 << 30, "sellable": True, "model": "managed"},
            {"code": "nest.ai.pro.monthly", "name": "Pro", "slots": 10, "storage_bytes": 100 << 30, "sellable": False}]}
        p = nc.parse_packages(raw)
        self.assertEqual([x["code"] for x in p["packages"]], ["nest.ai.starter.monthly"])
        self.assertEqual(p["packages"][0]["model"], "managed")
        with self.assertRaises(nc.NestError):
            nc.parse_packages({"packages": [{"code": 1}]})
        with self.assertRaises(nc.NestError):
            nc.parse_packages({"packages": "x"})

    def test_snapshot_status_and_setups(self):
        snap = nc.parse_snapshot({"allowance": {"slots": 3, "used": 1, "funding": {"package": "nest.ai.plus.monthly", "source": "iyzico"}, "duplicates": [{}]},
                                  "nests": [{"nest_id": "nst_ab12", "label": "Mira", "state": "paused", "status": "needs_you", "status_reason": "payment"}],
                                  "setups": [{"setup_id": "nsu_0123", "label": "Pigeonpost AI", "status": "failed", "last_error": "nestd_failed"},
                                             {"setup_id": "../x", "label": "bad"}]})
        self.assertEqual((snap["slots"], snap["used"], snap["duplicates"]), (3, 1, 1))
        self.assertEqual(len(snap["setups"]), 1)
        text, why = nc.status_line(snap["nests"][0])
        self.assertEqual(text, "Needs you")
        self.assertIn("Renew", why)
        self.assertEqual(nc.status_line(nc.parse_nest({"nest_id": "nst_1", "label": "x", "status": "weird"}))[0], "Unavailable")
        with self.assertRaises(nc.NestError):
            nc.parse_nest({"nest_id": "nst_../../x", "label": "x"})

    def test_upload_header_is_percent_encoded_utf8_without_controls(self):
        self.assertEqual(nc.upload_header("Q3 plan.pdf"), "Q3%20plan.pdf")
        self.assertEqual(nc.upload_header("çay/../x\n"), "%C3%A7ay%2F..%2Fx")
        with self.assertRaises(nc.NestError):
            nc.upload_header("\n\t")

    def test_upload_sends_raw_bytes_with_the_header_and_refuses_big_files(self):
        r = Recorder()
        nc.NestsAPI(r).upload("nst_1", "a b.txt", b"hello")
        method, path, data, kw = r.calls[0]
        self.assertEqual((method, path, data), ("POST", "/v1/nests/nst_1/files", None))
        self.assertEqual(kw["raw"], b"hello")
        self.assertEqual(kw["headers"]["x-file-name"], "a%20b.txt")
        with self.assertRaises(APIError):
            nc.NestsAPI(r).upload("nst_1", "big", b"x" * (nc.MAX_UPLOAD + 1))

    def test_a_decision_echoes_the_hash_that_was_shown(self):
        cards = nc.parse_approvals({"approvals": [{"approval_id": "nap_" + "0" * 24, "tool": "send_message", "args_hash": H,
                                                   "status": "pending", "call": {"tool": "send_message", "args": {"to": "/bob", "body": "hi"}}}]})
        self.assertEqual(nc.card_text(cards[0]), "Send a message to /bob:\nhi")
        r = Recorder()
        nc.NestsAPI(r).decide("nst_1", cards[0], True)
        self.assertEqual(r.calls[0][:3], ("POST", "/v1/nests/nst_1/approvals/nap_" + "0" * 24 + "/approve", {"args_hash": H}))
        nc.NestsAPI(r).decide("nst_1", cards[0], False)
        self.assertTrue(r.calls[1][1].endswith("/reject"))
        with self.assertRaises(nc.NestError):
            nc.parse_approvals({"approvals": [{"approval_id": "nap_" + "0" * 24, "tool": "x", "args_hash": "short"}]})

    def test_delete_needs_the_exact_name(self):
        self.assertTrue(nc.delete_confirmed("Research", "Research"))
        self.assertFalse(nc.delete_confirmed("Research", "research"))
        self.assertFalse(nc.delete_confirmed("Research", None))
        r = Recorder()
        with self.assertRaises(APIError):
            nc.NestsAPI(r).delete("nst_1", "Research", "Researc")
        self.assertEqual(r.calls, [])
        nc.NestsAPI(r).delete("nst_1", "Research", "Research")
        self.assertEqual(r.calls[0][:3], ("DELETE", "/v1/nests/nst_1", {"confirm": "Research"}))

    def test_files_usage_and_errors(self):
        f = nc.parse_files({"files": [{"file_id": "nfl_" + "1" * 24, "name": "a.pdf", "bytes": 10, "status": "partial",
                                       "coverage": {"units_total": 4, "units_text": 3}, "warnings": ["w"]}], "used_bytes": 10, "quota_bytes": 100})
        self.assertEqual(nc.coverage_text(f["files"][0]), "Read 3 of 4")
        lines = nc.usage_lines({"input_tokens": {"used": 1200, "limit": 1000000}, "output_tokens": {"used": 3, "limit": 100000},
                                "running_secs": 3720, "files_bytes": 1048576})
        self.assertIn("1,200 of 1,000,000", lines[0])
        self.assertIn("1 h 2 min", lines[1])
        self.assertEqual(nc.explain(APIError(409, "approval_changed")), "The action changed since you saw it. Review it again.")


if __name__ == "__main__":
    unittest.main()
