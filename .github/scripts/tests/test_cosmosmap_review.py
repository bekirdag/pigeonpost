"""Exercise public-release guards without credentials or network access."""
import ast
import contextlib
import copy
import io
import json
from pathlib import Path
from types import SimpleNamespace
import unittest

SOURCE = Path(__file__).resolve().parents[1] / 'cosmosmap_review.py'
TREE = ast.parse(SOURCE.read_text())
MAIN = next(n for n in TREE.body if isinstance(n, ast.FunctionDef) and n.name == 'main')
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


if __name__ == '__main__':
    unittest.main()
