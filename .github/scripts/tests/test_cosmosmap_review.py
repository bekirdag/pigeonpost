"""Exercise public-release guards without credentials or network access."""
import ast
import contextlib
import copy
import hashlib
import io
import json
from pathlib import Path
from types import SimpleNamespace
import unittest

SOURCE = Path(__file__).resolve().parents[1] / 'cosmosmap_review.py'
TREE = ast.parse(SOURCE.read_text())
MAIN = next(n for n in TREE.body if isinstance(n, ast.FunctionDef) and n.name == 'main')
RESUBMIT = next(n for n in TREE.body if isinstance(n, ast.FunctionDef) and n.name == 'resubmit_review')
OLD = '3189f64a-4e73-4928-ae77-800078d389b5'
TARGET = 'valid-31'


class ReviewGuards(unittest.TestCase):
    def run_case(self, action='inspect', selected=OLD, state='WAITING_FOR_REVIEW', processing='VALID', expired=False, encrypted=False, prerelease='1.11', absent=False, patch_error=False):
        mutations = []
        current = {'id': selected} if selected else None
        version = {'id': 'current-version', 'attributes': {'appStoreState': state, 'releaseType': 'AFTER_APPROVAL'}, 'relationships': {'build': {'data': current}}}
        target = {'id': TARGET, 'attributes': {'version': '31', 'processingState': processing, 'expired': expired, 'usesNonExemptEncryption': encrypted}}
        def get(path, **params):
            if path.endswith('/appStoreVersions'):
                self.assertEqual(params['filter[versionString]'], '1.11')
                self.assertEqual(params['filter[platform]'], 'IOS')
                return {'data': [copy.deepcopy(version)], 'included': [{'id': selected, 'type': 'builds', 'attributes': {'version': '30'}}]}
            if path == '/builds':
                self.assertEqual(params['filter[app]'], '6815358482')
                self.assertEqual(params['filter[version]'], '31')
                return {'data': [] if absent else [target]}
            if path.endswith('/reviewSubmissions'):
                return {'data': [{'id': 'pending-review', 'attributes': {'state': state, 'platform': 'IOS'}}]}
            if path.endswith('/items'):
                return {'data': []}
            if path.endswith('/preReleaseVersion'):
                return {'data': {'attributes': {'version': prerelease}}}
            if path.endswith('/current-version/build'):
                return {'data': current}
            if path.endswith('/current-version'):
                return {'data': version}
            raise AssertionError('Unexpected endpoint: ' + path)
        def call(method, path, body):
            nonlocal current
            mutations.append((method, path, body))
            if patch_error:
                raise RuntimeError('Apple 409: pending review cannot be edited')
            current = copy.deepcopy(body['data'])
        namespace = {'os': SimpleNamespace(environ={'ACTION': action, 'BUILD_NUMBER': '31'}), 'json': json, 'get': get, 'call': call, 'APP_ID': '6815358482', 'VERSION': '1.11', 'PREVIOUS_BUILD_ID': OLD}
        exec(compile(ast.Module(body=[MAIN], type_ignores=[]), str(SOURCE), 'exec'), namespace)
        with contextlib.redirect_stdout(io.StringIO()):
            try:
                result = namespace['main']()
            except RuntimeError as error:
                result = error
        return result, mutations, current

    def test_inspection_is_read_only_even_before_upload_processing(self):
        result, calls, current = self.run_case(absent=True)
        self.assertFalse(result['attached'])
        self.assertEqual(calls, [])
        self.assertEqual(current['id'], OLD)

    def test_valid_attachment_changes_only_the_exact_build_relationship(self):
        result, calls, current = self.run_case(action='attach')
        self.assertTrue(result['attached'])
        self.assertEqual(current['id'], TARGET)
        self.assertEqual(calls, [('PATCH', '/appStoreVersions/current-version/relationships/build', {'data': {'type': 'builds', 'id': TARGET}})])
        self.assertEqual(result['appStoreState'], 'WAITING_FOR_REVIEW')

    def test_already_attached_build_is_idempotent(self):
        result, calls, _ = self.run_case(action='attach', selected=TARGET)
        self.assertTrue(result['attached'])
        self.assertEqual(calls, [])

    def test_concurrent_selection_is_preserved(self):
        result, calls, current = self.run_case(action='attach', selected='another-build')
        self.assertIsInstance(result, RuntimeError)
        self.assertEqual(calls, [])
        self.assertEqual(current['id'], 'another-build')

    def test_invalid_or_unanswered_compliance_never_attaches(self):
        for kwargs in [{'processing': 'PROCESSING'}, {'expired': True}, {'encrypted': None}, {'encrypted': True}, {'prerelease': '1.12'}, {'absent': True}]:
            with self.subTest(**kwargs):
                result, calls, _ = self.run_case(action='attach', **kwargs)
                self.assertIsInstance(result, RuntimeError)
                self.assertEqual(calls, [])

    def test_active_or_finished_review_is_preserved(self):
        for state in ['IN_REVIEW', 'READY_FOR_DISTRIBUTION', 'PENDING_DEVELOPER_RELEASE']:
            result, calls, _ = self.run_case(action='attach', state=state)
            self.assertIsInstance(result, RuntimeError)
            self.assertEqual(calls, [])

    def test_vendor_rejection_does_not_cancel_or_resubmit(self):
        result, calls, current = self.run_case(action='attach', patch_error=True)
        self.assertIsInstance(result, RuntimeError)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][0], 'PATCH')
        self.assertEqual(current['id'], OLD)


class ReviewResubmission(unittest.TestCase):
    """Simulate Apple's asynchronous transitions and changes made by another actor."""
    def run_case(self, selected=OLD, state='WAITING_FOR_REVIEW', tamper_at=None,
                 concurrent_build=False, fail=None, cancel_timeout=False):
        review_id = 'existing-review'
        item_ids = [f'original-item-{i}' for i in range(7)]
        mutations = []
        live = {'build': selected, 'state': state, 'cancelled': False, 'submit_pending': False}
        failures = set()
        def get(path, **params):
            if path.endswith('/items'):
                ids = item_ids if not (tamper_at == 'initial' or tamper_at == 'after_cancel' and live['cancelled']) else item_ids[:-1]
                return {'data': [{'id': item_id} for item_id in ids]}
            if path.endswith('/current-version/build'):
                return {'data': {'id': live['build']}}
            if path == f'/reviewSubmissions/{review_id}':
                current = live['state']
                if current == 'CANCELING' and not cancel_timeout:
                    live['state'] = 'READY_FOR_REVIEW'
                    if concurrent_build:
                        live['build'] = 'another-build'
                elif live['submit_pending']:
                    live['state'] = 'WAITING_FOR_REVIEW'
                    live['submit_pending'] = False
                return {'data': {'attributes': {'state': current}}}
            raise AssertionError('Unexpected read: ' + path)
        def call(method, path, body):
            self.assertEqual(method, 'PATCH')
            mutations.append((path, copy.deepcopy(body)))
            if path.endswith('/relationships/build'):
                self.assertEqual(live['state'], 'READY_FOR_REVIEW')
                target = body['data']['id']
                if fail == 'attach' and target == TARGET:
                    raise RuntimeError('Apple rejected target attachment')
                live['build'] = target
            elif path == f'/reviewSubmissions/{review_id}':
                attrs = body['data']['attributes']
                self.assertEqual(body['data']['type'], 'reviewSubmissions')
                self.assertEqual(body['data']['id'], review_id)
                if attrs == {'canceled': True}:
                    if fail == 'cancel':
                        raise RuntimeError('Apple rejected cancellation')
                    self.assertEqual(live['state'], 'WAITING_FOR_REVIEW')
                    live['cancelled'] = True
                    live['state'] = 'CANCELING'
                elif attrs == {'submitted': True}:
                    if fail == 'submit_once' and 'submit' not in failures:
                        failures.add('submit')
                        raise RuntimeError('Apple rejected first submission')
                    self.assertEqual(live['state'], 'READY_FOR_REVIEW')
                    live['submit_pending'] = True
                else:
                    raise AssertionError('Unexpected review mutation: ' + repr(attrs))
            else:
                raise AssertionError('Unexpected write: ' + path)
            return {}
        namespace = {'get': get, 'call': call, 'hashlib': hashlib,
                     'time': SimpleNamespace(sleep=lambda seconds: None),
                     'REVIEW_ID': review_id, 'PREVIOUS_BUILD_ID': OLD,
                     'ITEM_SET_SHA256': hashlib.sha256('\n'.join(sorted(item_ids)).encode()).hexdigest()}
        exec(compile(ast.Module(body=[RESUBMIT], type_ignores=[]), str(SOURCE), 'exec'), namespace)
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            try:
                result = namespace['resubmit_review']('current-version', TARGET)
            except RuntimeError as error:
                result = error
        return result, mutations, live, output.getvalue()

    def test_pending_review_replaces_only_build_and_retains_all_seven_items(self):
        result, calls, live, _ = self.run_case()
        self.assertTrue(result['submitted'])
        self.assertEqual(result['items'], 7)
        self.assertEqual(live['build'], TARGET)
        self.assertEqual(live['state'], 'WAITING_FOR_REVIEW')
        self.assertEqual(calls, [
            ('/reviewSubmissions/existing-review', {'data': {'type': 'reviewSubmissions', 'id': 'existing-review', 'attributes': {'canceled': True}}}),
            ('/appStoreVersions/current-version/relationships/build', {'data': {'type': 'builds', 'id': TARGET}}),
            ('/reviewSubmissions/existing-review', {'data': {'type': 'reviewSubmissions', 'id': 'existing-review', 'attributes': {'submitted': True}}}),
        ])

    def test_existing_draft_does_not_cancel_again(self):
        result, calls, _, _ = self.run_case(state='READY_FOR_REVIEW')
        self.assertTrue(result['submitted'])
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0][0], '/appStoreVersions/current-version/relationships/build')

    def test_already_submitted_target_is_read_only(self):
        for state in ['WAITING_FOR_REVIEW', 'IN_REVIEW']:
            with self.subTest(state=state):
                result, calls, _, _ = self.run_case(selected=TARGET, state=state)
                self.assertTrue(result['submitted'])
                self.assertEqual(calls, [])

    def test_other_build_or_active_review_is_preserved(self):
        for kwargs in [{'selected': 'another-build'}, {'state': 'IN_REVIEW'}, {'state': 'COMPLETE'}]:
            with self.subTest(**kwargs):
                result, calls, _, _ = self.run_case(**kwargs)
                self.assertIsInstance(result, RuntimeError)
                self.assertEqual(calls, [])

    def test_changed_item_set_prevents_cancellation(self):
        result, calls, _, _ = self.run_case(tamper_at='initial')
        self.assertIsInstance(result, RuntimeError)
        self.assertEqual(calls, [])

    def test_changed_draft_items_prevent_attachment_and_rollback(self):
        result, calls, live, output = self.run_case(tamper_at='after_cancel')
        self.assertIsInstance(result, RuntimeError)
        self.assertEqual(len(calls), 1)
        self.assertEqual(live['build'], OLD)
        self.assertIn('REVIEW_ROLLBACK_NEEDS_INSPECTION', output)

    def test_concurrent_build_change_after_cancellation_is_preserved(self):
        result, calls, live, _ = self.run_case(concurrent_build=True)
        self.assertIsInstance(result, RuntimeError)
        self.assertEqual(len(calls), 1)
        self.assertEqual(live['build'], 'another-build')

    def test_attachment_failure_restores_previous_pending_submission(self):
        result, calls, live, output = self.run_case(fail='attach')
        self.assertIsInstance(result, RuntimeError)
        self.assertEqual(len(calls), 4)
        self.assertEqual(live['build'], OLD)
        self.assertEqual(live['state'], 'WAITING_FOR_REVIEW')
        self.assertIn('REVIEW_ROLLBACK WAITING_FOR_REVIEW', output)

    def test_submit_failure_restores_previous_build_before_resubmitting(self):
        result, calls, live, output = self.run_case(fail='submit_once')
        self.assertIsInstance(result, RuntimeError)
        self.assertEqual(len(calls), 5)
        self.assertEqual(live['build'], OLD)
        self.assertEqual(live['state'], 'WAITING_FOR_REVIEW')
        self.assertIn('REVIEW_ROLLBACK WAITING_FOR_REVIEW', output)

    def test_cancel_failure_keeps_previous_pending_build(self):
        result, calls, live, _ = self.run_case(fail='cancel')
        self.assertIsInstance(result, RuntimeError)
        self.assertEqual(len(calls), 1)
        self.assertEqual(live['build'], OLD)
        self.assertEqual(live['state'], 'WAITING_FOR_REVIEW')

    def test_cancel_timeout_never_attaches_or_resubmits_in_unknown_state(self):
        result, calls, live, _ = self.run_case(cancel_timeout=True)
        self.assertIsInstance(result, RuntimeError)
        self.assertEqual(len(calls), 1)
        self.assertEqual(live['build'], OLD)
        self.assertEqual(live['state'], 'CANCELING')


if __name__ == '__main__':
    unittest.main()
