"""Tests for deploy/status/pp-status.py: merge, flapping and alert logic. Run: python3 deploy/status/test_pp_status.py"""
import importlib.util
import os
import re
import tempfile
import unittest


def load_module():
    spec = importlib.util.spec_from_file_location('pp_status', os.path.join(os.path.dirname(os.path.abspath(__file__)), 'pp-status.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class TestPPStatus(unittest.TestCase):

    def setUp(self):
        self.mod = load_module()
        self.fake_comp = ('x', 'X', 'G', 'd', [('a', 'both', 'health', 'u'), ('b', 'inside', 'health', 'u')])

    # 1) merge_sample tests
    def test_merge_sample_split(self):
        # 'both' check failing at one location only gives 'split'
        outside = {'a': {'ok': False, 'ms': 10}}
        inside = {'a': {'ok': True, 'ms': 5}, 'b': {'ok': True, 'ms': 2}}
        sample, seen, ms = self.mod.merge_sample(self.fake_comp, outside, inside)
        self.assertEqual(sample, 'split')

    def test_merge_sample_fail_both(self):
        # failing at both gives 'fail'
        outside = {'a': {'ok': False, 'ms': 10}}
        inside = {'a': {'ok': False, 'ms': 5}, 'b': {'ok': True, 'ms': 2}}
        sample, seen, ms = self.mod.merge_sample(self.fake_comp, outside, inside)
        self.assertEqual(sample, 'fail')

    def test_merge_sample_fail1_inside_none(self):
        # failing with inside=None gives 'fail1' (only one location reporting for 'both' check)
        outside = {'a': {'ok': False, 'ms': 10}}
        inside = None
        sample, seen, ms = self.mod.merge_sample(self.fake_comp, outside, inside)
        self.assertEqual(sample, 'fail1')

    def test_merge_sample_inside_only_fail(self):
        # an 'inside'-only check failing gives 'fail'
        outside = {}
        inside = {'a': {'ok': True, 'ms': 5}, 'b': {'ok': False, 'ms': 2}}
        sample, seen, ms = self.mod.merge_sample(self.fake_comp, outside, inside)
        self.assertEqual(sample, 'fail')

    # 2) state_of tests
    def test_state_of_down_three_fails(self):
        now = 1000.0
        samples = [
            [900.0, False, 100, 'fail'],
            [950.0, False, 100, 'fail'],
            [990.0, False, 100, 'fail']
        ]
        self.assertEqual(self.mod.state_of(samples, now), 'down')

    def test_state_of_not_down_three_fail1(self):
        now = 1000.0
        samples = [
            [900.0, False, 100, 'fail1'],
            [950.0, False, 100, 'fail1'],
            [990.0, False, 100, 'fail1']
        ]
        # last3 are all fail1, not all "fail", so not down by first rule.
        # codes is length 3 (not 5), so second rule doesn't apply.
        # sum of non-ok in last3 is 3 >= 2 -> degraded
        self.assertNotEqual(self.mod.state_of(samples, now), 'down')
        self.assertEqual(self.mod.state_of(samples, now), 'degraded')

    def test_state_of_down_five_fail1(self):
        now = 1000.0
        samples = [
            [800.0, False, 100, 'fail1'],
            [850.0, False, 100, 'fail1'],
            [900.0, False, 100, 'fail1'],
            [950.0, False, 100, 'fail1'],
            [990.0, False, 100, 'fail1']
        ]
        # codes (last 5) are all fail1 -> all in ("fail", "fail1") -> down
        self.assertEqual(self.mod.state_of(samples, now), 'down')

    def test_state_of_degraded_two_split_in_three(self):
        now = 1000.0
        # last3: ok, split, split -> two non-ok in last3 -> degraded
        samples = [
            [950.0, True, 100, 'ok'],
            [970.0, False, 100, 'split'],
            [990.0, False, 100, 'split']
        ]
        self.assertEqual(self.mod.state_of(samples, now), 'degraded')

    def test_state_of_unknown_all_old(self):
        now = 1000.0
        samples = [
            [500.0, True, 100, 'ok'],
            [600.0, False, 100, 'fail']
        ]
        # all older than 300s from now (700+) -> recent is empty -> unknown
        self.assertEqual(self.mod.state_of(samples, now), 'unknown')

    # 3) alert_events tests
    def test_alert_events_down_then_recovered(self):
        mod = self.mod
        astate = {}
        now = 1000.0
        
        # Down: K
        status_down = {
            'components': [{'key': 'k', 'name': 'K', 'state': 'down'}],
            'locations': [{'reporting': True}, {'reporting': True}]
        }
        lines = mod.alert_events(status_down, astate, now)
        self.assertIn('Down: K', '\n'.join(lines))

        # Recovered: K
        status_up = {
            'components': [{'key': 'k', 'name': 'K', 'state': 'operational'}],
            'locations': [{'reporting': True}, {'reporting': True}]
        }
        lines = mod.alert_events(status_up, astate, now + 10)
        self.assertIn('Recovered: K', '\n'.join(lines))

    def test_alert_events_degraded_no_lines(self):
        mod = self.mod
        astate = {}
        now = 1000.0
        status_deg = {
            'components': [{'key': 'k', 'name': 'K', 'state': 'degraded'}],
            'locations': [{'reporting': True}, {'reporting': True}]
        }
        lines = mod.alert_events(status_deg, astate, now)
        self.assertEqual(lines, [])

    def test_alert_events_flapping_unstable(self):
        mod = self.mod
        astate = {}
        # FLAP_EVENTS is 4. We need more than 4 announcements within FLAP_WINDOW (3600).
        # Announcement happens on transition down->up or up->down.
        
        status_down = {'components': [{'key': 'k', 'name': 'K', 'state': 'down'}], 'locations': [{'reporting': True}, {'reporting': True}]}
        status_up = {'components': [{'key': 'k', 'name': 'K', 'state': 'operational'}], 'locations': [{'reporting': True}, {'reporting': True}]}
        
        # Start operational (implicit via empty astate? No, astate.setdefault initializes down=False)
        # 1. Down at t=1000
        mod.alert_events(status_down, astate, 1000) 
        # 2. Up at t=1100
        mod.alert_events(status_up, astate, 1100)
        # 3. Down at t=1200
        mod.alert_events(status_down, astate, 1200)
        # 4. Up at t=1300
        mod.alert_events(status_up, astate, 1300)
        
        # Now events list has [1000, 1100, 1200, 1300]. len is 4. Not > 4 yet.
        # 5. Down at t=1400 -> len becomes 5 (>4) -> Unstable line appears
        lines = mod.alert_events(status_down, astate, 1400)
        self.assertTrue(any('Unstable' in l for l in lines))
        
        # Further changes are silent (flapping is True, so continue skips adding to down/up lists)
        lines2 = mod.alert_events(status_up, astate, 1500)
        self.assertEqual(lines2, [])

    def test_alert_events_steady_again(self):
        mod = self.mod
        astate = {}
        
        # Setup flapping state
        status_down = {'components': [{'key': 'k', 'name': 'K', 'state': 'down'}], 'locations': [{'reporting': True}, {'reporting': True}]}
        status_up = {'components': [{'key': 'k', 'name': 'K', 'state': 'operational'}], 'locations': [{'reporting': True}, {'reporting': True}]}
        
        # Trigger flapping
        mod.alert_events(status_down, astate, 1000)
        mod.alert_events(status_up, astate, 1100)
        mod.alert_events(status_down, astate, 1200)
        mod.alert_events(status_up, astate, 1300)
        mod.alert_events(status_down, astate, 1400) # Triggers Unstable
        
        # Now flapping is True. changed=1400.
        # FLAP_QUIET is 1800.
        # At t = 1400 + 1800 = 3200, if state hasn't changed (it's still down), steady line appears.
        lines = mod.alert_events(status_down, astate, 3200)
        self.assertTrue(any('Steady again' in l for l in lines))

    # 4) send_alert tests
    def test_send_alert_postbox_ok(self):
        mod = self.mod
        
        # Setup temp dir with credentials
        with tempfile.TemporaryDirectory() as tmpdir:
            creds_dir = os.path.join(tmpdir, 'creds')
            os.makedirs(creds_dir)
            
            with open(os.path.join(creds_dir, 'alerts.token'), 'w') as f:
                f.write('token123')
            with open(os.path.join(creds_dir, 'postmark.token'), 'w') as f:
                f.write('pm_token123')
                
            os.environ['CREDENTIALS_DIRECTORY'] = creds_dir
            os.environ['PP_STATUS_ALERT_TO'] = 'alert@example.com'
            os.environ['PP_STATUS_FALLBACK_TO'] = 'fallback@example.com'
            
            calls = []
            def fake_post(url, body, headers):
                calls.append((url, body, headers))
                return True # Success
                
            lines = ["Down: K"]
            sent = mod.send_alert(lines, False, post=fake_post)
            
            self.assertEqual(sent, ['pigeonpost'])
            # Only one call made (to postbox)
            self.assertEqual(len(calls), 1)
            self.assertIn('postbox.pigeonpost.dev', calls[0][0])

    def test_send_alert_postbox_failing(self):
        mod = self.mod
        
        with tempfile.TemporaryDirectory() as tmpdir:
            creds_dir = os.path.join(tmpdir, 'creds')
            os.makedirs(creds_dir)
            
            with open(os.path.join(creds_dir, 'alerts.token'), 'w') as f:
                f.write('token123')
            with open(os.path.join(creds_dir, 'postmark.token'), 'w') as f:
                f.write('pm_token123')
                
            os.environ['CREDENTIALS_DIRECTORY'] = creds_dir
            os.environ['PP_STATUS_ALERT_TO'] = 'alert@example.com'
            os.environ['PP_STATUS_FALLBACK_TO'] = 'fallback@example.com'
            
            calls = []
            def fake_post(url, body, headers):
                calls.append((url, body, headers))
                if 'postbox.pigeonpost.dev' in url:
                    return False # Postbox fails
                return True # Postmark succeeds
                
            lines = ["Down: K"]
            sent = mod.send_alert(lines, False, post=fake_post)
            
            self.assertEqual(sent, ['postmark'])
            self.assertEqual(len(calls), 2)
            self.assertIn('postbox.pigeonpost.dev', calls[0][0])
            self.assertIn('api.postmarkapp.com', calls[1][0])

    def test_send_alert_mention_postbox_true(self):
        mod = self.mod
        
        with tempfile.TemporaryDirectory() as tmpdir:
            creds_dir = os.path.join(tmpdir, 'creds')
            os.makedirs(creds_dir)
            
            with open(os.path.join(creds_dir, 'alerts.token'), 'w') as f:
                f.write('token123')
            with open(os.path.join(creds_dir, 'postmark.token'), 'w') as f:
                f.write('pm_token123')
                
            os.environ['CREDENTIALS_DIRECTORY'] = creds_dir
            os.environ['PP_STATUS_ALERT_TO'] = 'alert@example.com'
            os.environ['PP_STATUS_FALLBACK_TO'] = 'fallback@example.com'
            
            calls = []
            def fake_post(url, body, headers):
                calls.append((url, body, headers))
                return True # Both succeed
                
            lines = ["Down: K"]
            sent = mod.send_alert(lines, True, post=fake_post)
            
            self.assertEqual(sent, ['pigeonpost', 'postmark'])
            self.assertEqual(len(calls), 2)

    def tearDown(self):
        for key in ['CREDENTIALS_DIRECTORY', 'PP_STATUS_ALERT_TO', 'PP_STATUS_FALLBACK_TO']:
            if key in os.environ:
                del os.environ[key]


class TestEndToEnd(unittest.TestCase):
    """Whole outside passes against fake probes: a component pointed at a bad URL goes down,
    alerts once, recovers once; the public file stays backward compatible and leaks nothing."""

    BAD = "https://broken.invalid/health"

    def setUp(self):
        self.mod = load_module()
        self.tmp = tempfile.TemporaryDirectory()
        self.mod.STATE_DIR = self.tmp.name
        os.environ["PP_STATUS_OUT"] = os.path.join(self.tmp.name, "public", "status.json")
        creds = os.path.join(self.tmp.name, "creds")
        os.makedirs(creds)
        for name in ("alerts.token", "postmark.token"):
            with open(os.path.join(creds, name), "w") as f:
                f.write("t")
        os.environ.update(CREDENTIALS_DIRECTORY=creds, PP_STATUS_ALERT_TO="/staff",
                          PP_STATUS_FALLBACK_TO="ops@example.test")
        # Point the Messaging check at a URL that cannot answer.
        comps = []
        for key, name, group, desc, checks in self.mod.COMPONENTS:
            if key == "postbox":
                checks = [(cid, where, kind, self.BAD) for cid, where, kind, _ in checks]
            comps.append((key, name, group, desc, checks))
        self.mod.COMPONENTS = comps
        self.broken = True
        self.sent = []

    def tearDown(self):
        self.tmp.cleanup()
        for k in ("PP_STATUS_OUT", "CREDENTIALS_DIRECTORY", "PP_STATUS_ALERT_TO", "PP_STATUS_FALLBACK_TO"):
            os.environ.pop(k, None)

    def prober(self, kind, target):
        return (not (self.broken and target == self.BAD)), 12

    def post(self, url, body, headers):
        self.sent.append((url, body))
        return True

    def push_inside(self, now):
        results = {cid: {"ok": not (self.broken and target == self.BAD), "ms": 5}
                   for comp in self.mod.COMPONENTS for cid, where, kind, target in comp[4]
                   if where in ("both", "inside")}
        report = self.mod.validate_report({"vantage": "inside", "generated_at": now, "results": results}, now)
        self.mod.write_atomic(os.path.join(self.tmp.name, "inside.json"), report)

    def run_pass(self, now, inside=True):
        if inside:
            self.push_inside(now)
        return self.mod.cmd_run(now=now, prober=self.prober, post=self.post)

    def test_bad_url_goes_down_alerts_once_and_recovers(self):
        t = 1_800_000_000
        states, alerts = [], []
        for i in range(6):
            status, lines = self.run_pass(t + 60 * i)
            states.append(next(c["state"] for c in status["components"] if c["key"] == "postbox"))
            alerts.extend(lines)
        self.assertEqual(states[:3], ["operational", "degraded", "down"])
        self.assertEqual(alerts, ["Down: Messaging"])
        # Messaging is down, so the alert went both ways: Pigeonpost message and Postmark.
        self.assertEqual([u.split("/")[2] for u, _ in self.sent],
                         ["postbox.pigeonpost.dev", "api.postmarkapp.com"])
        self.assertEqual(self.sent[0][1]["to"], "/staff")
        self.broken = False
        recovered = []
        for i in range(6, 10):
            _, lines = self.run_pass(t + 60 * i)
            recovered.extend(lines)
        self.assertEqual(recovered, ["Recovered: Messaging"])

    def test_one_location_failing_is_never_down(self):
        t = 1_800_000_000
        for i in range(8):
            now = t + 60 * i
            self.push_inside(now)
            # Inside says Messaging is fine; only the outside probe fails.
            path = os.path.join(self.tmp.name, "inside.json")
            rep = self.mod.load(path, None)
            rep["results"]["postbox"]["ok"] = True
            self.mod.write_atomic(path, rep)
            status, lines = self.mod.cmd_run(now=now, prober=self.prober, post=self.post)
            c = next(c for c in status["components"] if c["key"] == "postbox")
            self.assertNotEqual(c["state"], "down")
            self.assertEqual(lines, [])
        self.assertEqual(c["state"], "degraded")
        self.assertEqual(c["checked_from"], 2)
        self.assertEqual(c["uptime_24h"], 100.0)

    def test_without_inside_reports_one_location_needs_five_runs(self):
        t = 1_800_000_000
        states = []
        for i in range(6):
            status, _ = self.run_pass(t + 60 * i, inside=False)
            states.append(next(c["state"] for c in status["components"] if c["key"] == "postbox"))
        self.assertEqual(status["checked_from"], 1)
        self.assertEqual(states[2:5], ["degraded", "degraded", "down"])
        notes = next(c for c in status["components"] if c["key"] == "notes")
        self.assertEqual(notes["state"], "unknown")  # only the inside location can check it

    def test_public_file_is_compatible_and_names_no_hosts(self):
        status, _ = self.run_pass(1_800_000_000)
        keys = {c["key"] for c in status["components"]}
        for old in ("web", "inbox", "account", "enterprise", "partner", "developers", "postbox",
                    "registry", "auth", "calls", "billing", "bridge"):
            self.assertIn(old, keys)
        for c in status["components"]:
            for field in ("key", "name", "group", "state", "latency_ms", "uptime_24h", "uptime_7d", "bars"):
                self.assertIn(field, c)
            self.assertEqual(len(c["bars"]), 90)
        with open(os.environ["PP_STATUS_OUT"]) as f:
            text = f.read()
        self.assertIsNone(re.search(r"\b\d{1,3}(\.\d{1,3}){3}\b", text))
        for leak in ("127.0.0", "http", ":8", "raijin", "nests-1", "wodomini"):
            self.assertNotIn(leak, text)

    def test_ingest_rejects_bad_reports(self):
        now = 1_800_000_000
        with self.assertRaises(ValueError):
            self.mod.validate_report({"vantage": "outside", "generated_at": now, "results": {}}, now)
        with self.assertRaises(ValueError):
            self.mod.validate_report({"vantage": "inside", "generated_at": now - 3600, "results": {}}, now)
        rep = self.mod.validate_report({"vantage": "inside", "generated_at": now, "results": {
            "postbox": {"ok": True, "ms": 3}, "made_up": {"ok": True}, "auth": {"ok": "yes"}}}, now)
        self.assertEqual(list(rep["results"]), ["postbox"])

    def test_v1_history_is_read(self):
        now = 1_800_000_000
        old = [[now - 120, False, None], [now - 60, False, None], [now, False, None]]
        self.assertEqual(self.mod.state_of(old, now), "down")


if __name__ == '__main__':
    unittest.main()
