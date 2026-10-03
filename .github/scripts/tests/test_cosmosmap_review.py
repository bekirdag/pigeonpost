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
WITHDRAW = next(n for n in TREE.body if isinstance(n, ast.FunctionDef) and n.name == 'withdraw_review')
OLD = '0e2928b8-8f0c-414c-bd8d-beeb86b90e63'
TARGET = 'valid-32'


class ReviewGuards(unittest.TestCase):
    def run_case(self, action='inspect', selected=OLD, state='WAITING_FOR_REVIEW', processing='VALID', expired=False, encrypted=False, prerelease='1.11', absent=False, patch_error=False):
        mutations = []
        current = {'id': selected} if selected else None
        version = {'id': 'current-version', 'attributes': {'appStoreState': state, 'releaseType': 'AFTER_APPROVAL'}, 'relationships': {'build': {'data': current}}}
        target = {'id': TARGET, 'attributes': {'version': '32', 'processingState': processing, 'expired': expired, 'usesNonExemptEncryption': encrypted}}
        def get(path, **params):
            if path.endswith('/appStoreVersions'):
                self.assertEqual(params['filter[versionString]'], '1.11')
                self.assertEqual(params['filter[platform]'], 'IOS')
                return {'data': [copy.deepcopy(version)], 'included': [{'id': selected, 'type': 'builds', 'attributes': {'version': '31'}}]}
            if path == '/builds':
                self.assertEqual(params['filter[app]'], '6815358482')
                self.assertEqual(params['filter[version]'], '32')
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
        namespace = {'os': SimpleNamespace(environ={'ACTION': action, 'BUILD_NUMBER': '32'}), 'json': json, 'get': get, 'call': call, 'APP_ID': '6815358482', 'VERSION': '1.11', 'PREVIOUS_BUILD_ID': OLD, 'REVIEW_ID': 'original-review'}
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

    def test_invalid_build_cannot_withdraw_the_pending_review(self):
        for kwargs in [{'processing': 'PROCESSING'}, {'expired': True}, {'encrypted': None}, {'prerelease': '1.12'}, {'absent': True}]:
            result, calls, _ = self.run_case(action='withdraw', **kwargs)
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


class ReviewWithdrawal(unittest.TestCase):
    def run_case(self, selected=OLD, state='WAITING_FOR_REVIEW', app_state='WAITING_FOR_REVIEW', foreign=False, tamper=False, complete=False):
        review_id = 'observed-review'
        refs = [('appStoreVersion', 'appStoreVersions', 'current-version')] + [('inAppPurchaseVersion', 'inAppPurchaseVersions', f'purchase-{i}') for i in range(6)]
        items = [{'id': f'item-{i}', 'relationships': {key: {'data': {'type': kind, 'id': rid}}}} for i, (key, kind, rid) in enumerate(refs)]
        id_digest = hashlib.sha256('\n'.join(sorted(i['id'] for i in items)).encode()).hexdigest()
        resource_digest = hashlib.sha256('\n'.join('|'.join(ref) for ref in sorted(refs)).encode()).hexdigest()
        if tamper:
            items[0]['relationships']['appStoreVersion']['data']['id'] = 'other-version'
        live = {'state': 'COMPLETE' if complete else state}
        mutations = []
        def get(path, **params):
            if path == '/appStoreVersions/current-version/build': return {'data': {'id': selected}}
            if path == '/appStoreVersions/current-version': return {'data': {'attributes': {'appStoreState': app_state}}}
            if path.endswith('/items'): return {'data': copy.deepcopy(items)}
            if path == f'/reviewSubmissions/{review_id}': return {'data': {'attributes': {'state': live['state']}}}
            if path.endswith('/reviewSubmissions'):
                rows = [] if complete else [{'id': review_id, 'attributes': {'state': state, 'platform': 'IOS'}}]
                if foreign: rows.append({'id': 'concurrent-review', 'attributes': {'state': 'READY_FOR_REVIEW', 'platform': 'IOS'}})
                return {'data': rows}
            raise AssertionError(path)
        def call(method, path, body):
            mutations.append((method, path, body)); live['state'] = 'COMPLETE'
        ns = {'get': get, 'call': call, 'hashlib': hashlib, 'time': SimpleNamespace(sleep=lambda _: None), 'PREVIOUS_BUILD_ID': OLD, 'REVIEW_ID': review_id, 'APP_ID': '6815358482', 'ITEM_SET_SHA256': id_digest, 'RESOURCE_SET_SHA256': resource_digest}
        exec(compile(ast.Module(body=[WITHDRAW], type_ignores=[]), str(SOURCE), 'exec'), ns)
        try: result = ns['withdraw_review']('current-version')
        except RuntimeError as error: result = error
        return result, mutations

    def test_only_observed_pending_submission_is_canceled(self):
        result, calls = self.run_case()
        self.assertTrue(result['withdrawn'])
        self.assertEqual(calls, [('PATCH', '/reviewSubmissions/observed-review', {'data': {'type': 'reviewSubmissions', 'id': 'observed-review', 'attributes': {'canceled': True}}})])

    def test_concurrent_build_resources_or_review_are_preserved(self):
        for kwargs in [{'selected': 'other-build'}, {'foreign': True}, {'tamper': True}]:
            result, calls = self.run_case(**kwargs)
            self.assertIsInstance(result, RuntimeError)
            self.assertEqual(calls, [])

    def test_started_or_approved_review_is_preserved(self):
        for state in ['IN_REVIEW', 'COMPLETE', 'UNRESOLVED_ISSUES']:
            result, calls = self.run_case(state=state)
            self.assertIsInstance(result, RuntimeError)
            self.assertEqual(calls, [])

    def test_completed_withdrawal_is_idempotent(self):
        result, calls = self.run_case(complete=True, app_state='DEVELOPER_REJECTED')
        self.assertTrue(result['withdrawn'])
        self.assertEqual(calls, [])


class ReviewResubmission(unittest.TestCase):
    """Use Apple's observed COMPLETE/REMOVED withdrawal state and resumable drafts."""
    def run_case(self, selected=OLD, old_state='COMPLETE', app_state='DEVELOPER_REJECTED',
                 draft_state=None, partial=0, tamper=None, fail_item=False, retry=False):
        original_id, new_id = 'old-review', 'new-review'
        refs = [('appStoreVersion', 'appStoreVersions', 'current-version')] + [
            ('inAppPurchaseVersion', 'inAppPurchaseVersions', f'purchase-version-{i}') for i in range(6)]
        def item(ref, item_id):
            key, kind, resource_id = ref
            return {'id': item_id, 'attributes': {'state': 'REMOVED'}, 'relationships': {
                key: {'data': {'type': kind, 'id': resource_id}}}}
        original = [item(ref, f'original-item-{i}') for i, ref in enumerate(refs)]
        digest = hashlib.sha256('\n'.join(sorted(row['id'] for row in original)).encode()).hexdigest()
        if tamper == 'original_ids':
            original.pop()
        if tamper == 'original_version':
            original[0]['relationships']['appStoreVersion']['data']['id'] = 'another-version'
        live = {'build': selected, 'review': new_id if draft_state else None, 'state': draft_state,
                'items': [item(ref, f'new-item-{i}') for i, ref in enumerate(refs[:partial])], 'submit_pending': False}
        if tamper == 'draft_items':
            live['items'].append(item(('inAppPurchaseVersion', 'inAppPurchaseVersions', 'other-purchase'), 'foreign-item'))
        mutations, failures = [], set()
        def get(path, **params):
            if path == f'/reviewSubmissions/{original_id}/items':
                self.assertEqual(params['include'], 'appStoreVersion,inAppPurchaseVersion')
                return {'data': copy.deepcopy(original)}
            if path == f'/reviewSubmissions/{original_id}':
                return {'data': {'attributes': {'state': old_state}}}
            if path == '/appStoreVersions/current-version/build':
                return {'data': {'id': live['build']}}
            if path == '/appStoreVersions/current-version':
                return {'data': {'attributes': {'appStoreState': app_state}}}
            if path == '/apps/6815358482/reviewSubmissions':
                rows = [{'id': original_id, 'attributes': {'state': old_state, 'platform': 'IOS'}}]
                if live['review']:
                    rows.append({'id': live['review'], 'attributes': {'state': live['state'], 'platform': 'IOS'}})
                return {'data': rows}
            if path == f'/reviewSubmissions/{new_id}/items':
                return {'data': copy.deepcopy(live['items'])}
            if path == f'/reviewSubmissions/{new_id}':
                current = live['state']
                if live['submit_pending']:
                    live['state'], live['submit_pending'] = 'WAITING_FOR_REVIEW', False
                return {'data': {'attributes': {'state': current}}}
            raise AssertionError('Unexpected read: ' + path)
        def call(method, path, body):
            mutations.append((method, path, copy.deepcopy(body)))
            if path.endswith('/relationships/build'):
                self.assertEqual(method, 'PATCH')
                live['build'] = body['data']['id']
                return {}
            if path == '/reviewSubmissions':
                self.assertEqual(method, 'POST')
                self.assertIsNone(live['review'])
                self.assertEqual(body['data']['relationships']['app']['data'], {'type': 'apps', 'id': '6815358482'})
                self.assertEqual(body['data']['attributes'], {'platform': 'IOS'})
                live['review'], live['state'] = new_id, 'READY_FOR_REVIEW'
                return {'data': {'id': new_id}}
            if path == '/reviewSubmissionItems':
                self.assertEqual(method, 'POST')
                self.assertEqual(live['state'], 'READY_FOR_REVIEW')
                if fail_item and len(live['items']) == 2 and 'item' not in failures:
                    failures.add('item')
                    raise RuntimeError('Apple rejected one item addition')
                rels = body['data']['relationships']
                self.assertEqual(rels['reviewSubmission']['data'], {'type': 'reviewSubmissions', 'id': new_id})
                self.assertEqual(set(body['data']), {'type', 'relationships'})
                key = next(key for key in rels if key != 'reviewSubmission')
                live['items'].append(item((key, rels[key]['data']['type'], rels[key]['data']['id']), f'new-item-{len(live["items"])}'))
                return {'data': live['items'][-1]}
            if path == f'/reviewSubmissions/{new_id}':
                self.assertEqual(method, 'PATCH')
                self.assertEqual(body['data']['attributes'], {'submitted': True})
                self.assertEqual(len(live['items']), 7)
                live['submit_pending'] = True
                return {}
            raise AssertionError('Unexpected write: ' + path)
        namespace = {'get': get, 'call': call, 'hashlib': hashlib, 'APP_ID': '6815358482',
                     'time': SimpleNamespace(sleep=lambda seconds: None),
                     'REVIEW_ID': original_id, 'PREVIOUS_BUILD_ID': OLD, 'ITEM_SET_SHA256': digest}
        exec(compile(ast.Module(body=[RESUBMIT], type_ignores=[]), str(SOURCE), 'exec'), namespace)
        with contextlib.redirect_stdout(io.StringIO()):
            try:
                result = namespace['resubmit_review']('current-version', TARGET)
            except RuntimeError as error:
                result = error
                if retry:
                    result = namespace['resubmit_review']('current-version', TARGET)
        return result, mutations, live

    def test_completed_review_replaces_only_build_and_carries_exact_original_resources(self):
        result, calls, live = self.run_case()
        self.assertTrue(result['submitted'])
        self.assertEqual(result['reviewId'], 'new-review')
        self.assertEqual(result['replacesReviewId'], 'old-review')
        self.assertEqual(result['items'], 7)
        self.assertEqual(len(result['resourceSetSha256']), 64)
        self.assertEqual(live['build'], TARGET)
        self.assertEqual(live['state'], 'WAITING_FOR_REVIEW')
        self.assertEqual(len(calls), 10)
        resources = [c[2]['data']['relationships'] for c in calls if c[1] == '/reviewSubmissionItems']
        self.assertEqual(sum('appStoreVersion' in row for row in resources), 1)
        self.assertEqual(sum('inAppPurchaseVersion' in row for row in resources), 6)
        self.assertFalse(any(c[1].startswith('/inAppPurchase') for c in calls))
        self.assertFalse(any(c[1].endswith('/old-review') for c in calls))

    def test_existing_partial_draft_resumes_without_duplicate_items_or_submission(self):
        result, calls, live = self.run_case(selected=TARGET, draft_state='READY_FOR_REVIEW', partial=3)
        self.assertTrue(result['submitted'])
        self.assertEqual(len(live['items']), 7)
        self.assertEqual(sum(c[1] == '/reviewSubmissionItems' for c in calls), 4)
        self.assertFalse(any(c[1] == '/reviewSubmissions' for c in calls))

    def test_already_submitted_target_is_read_only(self):
        for state in ['WAITING_FOR_REVIEW', 'IN_REVIEW']:
            with self.subTest(state=state):
                result, calls, _ = self.run_case(selected=TARGET, draft_state=state, partial=7)
                self.assertTrue(result['submitted'])
                self.assertEqual(calls, [])

    def test_other_build_is_preserved(self):
        result, calls, live = self.run_case(selected='another-build')
        self.assertIsInstance(result, RuntimeError)
        self.assertEqual(calls, [])
        self.assertEqual(live['build'], 'another-build')

    def test_still_active_original_review_is_preserved(self):
        for state in ['WAITING_FOR_REVIEW', 'IN_REVIEW', 'CANCELING']:
            with self.subTest(state=state):
                result, calls, _ = self.run_case(old_state=state)
                self.assertIsInstance(result, RuntimeError)
                self.assertEqual(calls, [])

    def test_changed_original_items_or_app_version_prevent_any_write(self):
        for tamper in ['original_ids', 'original_version']:
            with self.subTest(tamper=tamper):
                result, calls, _ = self.run_case(tamper=tamper)
                self.assertIsInstance(result, RuntimeError)
                self.assertEqual(calls, [])

    def test_different_draft_items_are_preserved(self):
        result, calls, live = self.run_case(draft_state='READY_FOR_REVIEW', partial=2, tamper='draft_items')
        self.assertIsInstance(result, RuntimeError)
        self.assertEqual(calls, [])
        self.assertEqual(len(live['items']), 3)

    def test_active_replacement_with_wrong_build_or_missing_items_is_preserved(self):
        for kwargs in [{'selected': OLD, 'partial': 7}, {'selected': TARGET, 'partial': 6}]:
            with self.subTest(**kwargs):
                result, calls, _ = self.run_case(draft_state='WAITING_FOR_REVIEW', **kwargs)
                self.assertIsInstance(result, RuntimeError)
                self.assertEqual(calls, [])

    def test_version_that_started_review_again_is_not_edited(self):
        result, calls, _ = self.run_case(app_state='IN_REVIEW')
        self.assertIsInstance(result, RuntimeError)
        self.assertEqual(calls, [])

    def test_item_failure_keeps_valid_build_and_partial_draft_for_recovery(self):
        result, calls, live = self.run_case(fail_item=True)
        self.assertIsInstance(result, RuntimeError)
        self.assertEqual(live['build'], TARGET)
        self.assertEqual(live['state'], 'READY_FOR_REVIEW')
        self.assertEqual(len(live['items']), 2)
        self.assertFalse(any(c[0] == 'PATCH' and c[1].endswith('/new-review') for c in calls))

    def test_retry_after_item_failure_completes_same_draft_once(self):
        result, calls, live = self.run_case(fail_item=True, retry=True)
        self.assertTrue(result['submitted'])
        self.assertEqual(len(live['items']), 7)
        self.assertEqual(sum(c[1] == '/reviewSubmissions' for c in calls), 1)
        self.assertEqual(sum(c[0] == 'PATCH' and c[1].endswith('/new-review') for c in calls), 1)


if __name__ == '__main__':
    unittest.main()
