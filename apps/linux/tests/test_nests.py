"""Nests rules without a GUI: parsing, approval binding, upload header, delete confirmation, gating."""
import unittest

from pigeonpost import nests_core as nc
from pigeonpost.api import APIError, Transport

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

    def test_can_resume_paused_reasons(self):
        self.assertTrue(nc.can_resume({'state': 'paused', 'paused_reason': 'owner'}))
        self.assertTrue(nc.can_resume({'state': 'paused', 'paused_reason': None}))
        self.assertFalse(nc.can_resume({'state': 'paused', 'paused_reason': 'unfunded'}))
        self.assertFalse(nc.can_resume({'state': 'paused', 'paused_reason': 'over_capacity'}))
        self.assertFalse(nc.can_resume({'state': 'running', 'paused_reason': 'owner'}))

    def test_can_pause_states(self):
        for state in ['running', 'ready', 'stopped']:
            self.assertTrue(nc.can_pause({'state': state}))
        for state in ['paused', 'creating', 'failed']:
            self.assertFalse(nc.can_pause({'state': state}))

    def test_deletion_notice_none(self):
        self.assertIsNone(nc.deletion_notice({'delete_after': None}))

    def test_deletion_notice_export(self):
        result = nc.deletion_notice({'delete_after': 1893456000})
        self.assertIn('Export', result)

    def test_label_problem_invalid(self):
        self.assertTrue(nc.label_problem(''))
        self.assertTrue(nc.label_problem('x' * 65))
        self.assertTrue(nc.label_problem('a\nb'))

    def test_label_problem_valid(self):
        self.assertIsNone(nc.label_problem('Mira'))

    def test_can_add_nest_none_package(self):
        self.assertFalse(nc.can_add_nest(None))
        self.assertFalse(nc.can_add_nest({'package': None, 'used': 0, 'slots': 3}))

    def test_can_add_nest_with_slots(self):
        self.assertTrue(nc.can_add_nest({'package': 'nest.ai.plus.monthly', 'used': 1, 'slots': 3}))
        self.assertFalse(nc.can_add_nest({'package': 'nest.ai.plus.monthly', 'used': 3, 'slots': 3}))

    def test_org_can_manage_roles(self):
        self.assertTrue(nc.org_can_manage({'roles': ['agent_admin']}))
        self.assertFalse(nc.org_can_manage({'roles': ['member']}))
        self.assertFalse(nc.org_can_manage(None))

    def test_peer_addresses_matching(self):
        peers = [
            {'peer_handle': '/acme/hr', 'peer': '/k/abc'},
            {'peer_handle': '/other', 'peer': '/k/zzz'}
        ]
        result = nc.peer_addresses('/acme/hr', peers)
        self.assertEqual(result, {'/acme/hr', '/k/abc'})

    def test_peer_addresses_empty(self):
        self.assertEqual(nc.peer_addresses(None, []), set())

    def test_can_approve_valid(self):
        card = nc.parse_approvals({'approvals': [{
            'approval_id': 'nap_' + '0' * 24,
            'args_hash': H,
            'tool': 'send_message',
            'call': {'tool': 'send_message', 'args': {'to': '/bob', 'body': 'hi'}}
        }]})[0]
        self.assertTrue(nc.can_approve(card))

    def test_can_approve_invalid_to(self):
        card = nc.parse_approvals({'approvals': [{
            'approval_id': 'nap_' + '0' * 24,
            'args_hash': H,
            'tool': 'send_message',
            'call': {'tool': 'send_message', 'args': {'to': '/bo\u202eb', 'body': 'hi'}}
        }]})[0]
        self.assertFalse(nc.can_approve(card))

    def test_can_approve_spend_invalid(self):
        card = nc.parse_approvals({'approvals': [{
            'approval_id': 'nap_' + '0' * 24,
            'args_hash': H,
            'tool': 'spend',
            'call': {}
        }]})[0]
        self.assertFalse(nc.can_approve(card))
        self.assertIn('cannot show', nc.card_text(card))

    def test_pending_cards_filtering(self):
        cards = [
            {'status': 'pending', 'expires_at': None},
            {'status': 'pending', 'expires_at': 101},
            {'status': 'pending', 'expires_at': 100},
            {'status': 'approved', 'expires_at': None}
        ]
        result = nc.pending_cards(cards, now=100)
        self.assertEqual(len(result), 2)

    def test_expiry_text_none(self):
        self.assertIsNone(nc.expiry_text({'expires_at': None}))

    def test_expiry_text_hours(self):
        self.assertIn('Expires in 2 h', nc.expiry_text({'expires_at': 7200}, now=0))

    def test_expiry_text_minutes(self):
        self.assertIn('Expires in 1 min', nc.expiry_text({'expires_at': 90}, now=0))

    def test_expiry_text_expired(self):
        self.assertIn('Expired', nc.expiry_text({'expires_at': 0}, now=0))

    def test_usage_lines_no_ai(self):
        usage = {
            'input_tokens': {'used': 1, 'limit': 0},
            'output_tokens': {'used': 0, 'limit': 0},
            'running_secs': 0,
            'files_bytes': 0,
            'requests': 7
        }
        lines = nc.usage_lines(usage)
        self.assertIn('no AI is connected', lines[0])
        self.assertIn('7', lines[-1])

    def test_usage_lines_pooled_hours(self):
        usage = {
            'package': {'active_hours_pooled': 10},
            'input_tokens': {'used': 0, 'limit': 5},
            'output_tokens': {'used': 0, 'limit': 5},
            'running_secs': 0,
            'files_bytes': 0,
            'requests': 0
        }
        lines = nc.usage_lines(usage)
        self.assertIn('10 h', lines[1])

    def test_usage_lines_string_used(self):
        usage = {
            'input_tokens': {'used': 'x', 'limit': 5},
            'output_tokens': {'used': 0, 'limit': 5},
            'running_secs': 0,
            'files_bytes': 0,
            'requests': 0
        }
        nc.usage_lines(usage)

    def test_upload_outcome_rejected(self):
        result = nc.upload_outcome({
            'file_id': 'nfl_' + '1' * 24,
            'name': 'a.pdf',
            'status': 'rejected',
            'reason': 'encrypted'
        })
        self.assertIn('could not be read', result)
        self.assertIn('encrypted', result)

    def test_upload_outcome_partial(self):
        result = nc.upload_outcome({
            'file_id': 'nfl_' + '1' * 24,
            'name': 'a.pdf',
            'status': 'partial',
            'coverage': {'units_total': 4, 'units_text': 3}
        })
        self.assertIn('Read 3 of 4', result)

    def test_upload_outcome_parsed(self):
        result = nc.upload_outcome({
            'file_id': 'nfl_' + '1' * 24,
            'name': 'a.pdf',
            'status': 'parsed'
        })
        self.assertEqual(result, 'a.pdf added.')

    def test_parse_files_shortens_warning(self):
        result = nc.parse_files({'files': [{'file_id': 'nfl_' + '1' * 24, 'name': 'a.zip', 'status': 'partial', 'warnings': ['w' * 5000]}]})
        self.assertEqual(result['files'][0]['warnings'], ['w' * 300])

    def test_explain_value_error(self):
        result = nc.explain(ValueError('secret path'))
        self.assertNotIn('secret', result)

    def test_explain_api_error(self):
        result = nc.explain(APIError(502, 'tool_failed'))
        self.assertEqual(result, 'The action could not be carried out.')

    def test_recorder_export(self):
        r = Recorder()
        api = nc.NestsAPI(r)
        api.export('nst_1')
        call = r.calls[0]
        kwargs = call[3] if len(call) > 3 else {}
        self.assertTrue(kwargs.get('binary'))
        self.assertEqual(kwargs.get('max_bytes'), nc.MAX_EXPORT)

    def test_recorder_create(self):
        r = Recorder()
        api = nc.NestsAPI(r)
        api.create('Mira')
        self.assertIn(('POST', '/v1/nests', {'label': 'Mira'}), [(c[0], c[1], c[2]) for c in r.calls])

    def test_recorder_upload_empty(self):
        r = Recorder()
        api = nc.NestsAPI(r)
        with self.assertRaises(APIError) as caught:
            api.upload('nst_1', 'a', b'')
        self.assertEqual(caught.exception.code, 'empty_file')
        self.assertEqual(len(r.calls), 0)

    def test_reasons_and_card_shape_match_the_server(self):
        self.assertEqual(nc.parse_nest({"nest_id": "nst_1", "label": "x", "paused_reason": "unfunded"})["paused_reason"], "unfunded")
        self.assertIn("Renew", nc.REASON_TEXT["payment"])
        card = nc.parse_approvals({"approvals": [{"approval_id": "nap_" + "0" * 24, "tool": "send_message", "args_hash": H, "call": None}]})[0]
        self.assertFalse(nc.can_approve(card), "a corrupt call is never offered for approval")
        self.assertIn("cannot show", nc.card_text(card))

    def test_an_export_may_exceed_the_attachment_limit_but_not_its_own(self):
        size = 26 * 1024 * 1024

        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *args):
                return False

            def read(self, n):
                return b"x" * min(n, size)

        class Opener:
            def open(self, req, timeout=None):
                return Response()

        transport = Transport()
        transport.opener = Opener()
        self.assertEqual(len(transport.request("GET", "https://example.test/x", binary=True, max_bytes=nc.MAX_EXPORT)), size)
        with self.assertRaises(APIError) as refused:
            transport.request("GET", "https://example.test/x", binary=True)
        self.assertEqual(refused.exception.code, "response_too_large")


if __name__ == "__main__":
    unittest.main()
