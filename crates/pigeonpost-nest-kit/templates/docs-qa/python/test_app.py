import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import unittest
import app


class FakeJob:
    def __init__(self, payload=None, config=None, broker=None, effect=None):
        self.id = "job_aaaaaaaaaaaaaaaaaaaaaaaaaa"
        self.payload = payload or {}
        self.config = config or {}
        self.progress_calls = []
        self.broker = broker or FakeBroker(available=False)
        self._effect = effect

    def progress(self, fraction, message=""):
        self.progress_calls.append((fraction, message))

    def effect(self, **kwargs):
        if self._effect is None:
            raise AssertionError("no effect expected")
        return self._effect(**kwargs)


class FakeBroker:
    def __init__(self, available=True, complete=None):
        self.available = available
        self._complete = complete

    def complete(self, **kwargs):
        if self._complete is None:
            raise app.NestError("no_broker")
        return self._complete(**kwargs)


class TestDocsQA(unittest.TestCase):
    def setUp(self):
        # Create a temporary docs directory with required content for load_docs test
        import tempfile
        import shutil
        self.docs_dir = tempfile.mkdtemp()
        with open(os.path.join(self.docs_dir, "policy.md"), "w") as f:
            f.write("## Annual leave\nEmployees get 25 days of paid annual leave.\n## Sick leave\nSick leave policy details.")
        # Patch load_docs to use this directory for tests that need it
        self._orig_load_docs = app.load_docs

    def tearDown(self):
        import shutil
        if hasattr(self, 'docs_dir'):
            shutil.rmtree(self.docs_dir)
        app.load_docs = self._orig_load_docs

    def test_parse_sections(self):
        md = "intro\n## One\nfirst line\nsecond\n## Two\nx"
        result = app.parse_sections(md)
        self.assertEqual(len(result), 2)
        self.assertEqual(result[0]["id"], "one")
        self.assertEqual(result[0]["title"], "One")
        self.assertEqual(result[0]["body"], "first line second")
        self.assertEqual(result[1]["id"], "two")
        self.assertEqual(result[1]["title"], "Two")
        self.assertEqual(result[1]["body"], "x")

    def test_best_sections_annual_leave(self):
        # Simulate load_docs result manually based on spec description
        sections = [
            {"id": "annual-leave", "title": "Annual leave", "body": "Employees get 25 days of paid annual leave."},
            {"id": "sick-leave", "title": "Sick leave", "body": "Sick leave policy details."}
        ]
        result = app.best_sections(sections, "annual leave days", 1)
        self.assertEqual(result[0]["id"], "annual-leave")

    def test_handler_no_broker(self):
        sections = [
            {"id": "annual-leave", "title": "Annual leave", "body": "Employees get 25 days of paid annual leave."},
        ]
        handlers = app.make_handlers(sections)
        job = FakeJob(payload={"question": "How many days of annual leave?"})
        handler = handlers["docs.ask@1.0"]
        result = handler(job)
        self.assertFalse(result["model_used"])
        self.assertEqual(result["sources"][0], "annual-leave")
        self.assertIn("25 days", result["answer"])

    def test_handler_with_broker_success(self):
        sections = [
            {"id": "annual-leave", "title": "Annual leave", "body": "Employees get 25 days of paid annual leave."},
        ]
        handlers = app.make_handlers(sections)
        
        captured_user = None
        def complete(system, user, max_tokens):
            nonlocal captured_user
            captured_user = user
            return {"text": " Twenty-five days. ", "usage": {"input": 1, "output": 1}}

        broker = FakeBroker(available=True, complete=complete)
        job = FakeJob(payload={"question": "How many days of annual leave?"}, broker=broker)
        handler = handlers["docs.ask@1.0"]
        result = handler(job)
        
        self.assertEqual(result["answer"], "Twenty-five days.")
        self.assertTrue(result["model_used"])
        self.assertIn("Annual leave", captured_user)

    def test_handler_with_broker_failure(self):
        sections = [
            {"id": "annual-leave", "title": "Annual leave", "body": "Employees get 25 days of paid annual leave."},
        ]
        handlers = app.make_handlers(sections)
        
        def complete(**kwargs):
            raise app.NestError("upstream_error")

        broker = FakeBroker(available=True, complete=complete)
        job = FakeJob(payload={"question": "How many days of annual leave?"}, broker=broker)
        handler = handlers["docs.ask@1.0"]
        result = handler(job)
        
        self.assertFalse(result["model_used"])
        self.assertIn("25 days", result["answer"])

    def test_handler_no_match(self):
        sections = [
            {"id": "annual-leave", "title": "Annual leave", "body": "Employees get 25 days of paid annual leave."},
        ]
        handlers = app.make_handlers(sections)
        job = FakeJob(payload={"question": "zzzz qqqq"})
        handler = handlers["docs.ask@1.0"]
        result = handler(job)
        self.assertEqual(result["answer"], "The documents do not cover this question.")
        self.assertEqual(result["sources"], [])
        self.assertFalse(result["model_used"])

    def test_handler_empty_question(self):
        sections = [
            {"id": "annual-leave", "title": "Annual leave", "body": "Employees get 25 days of paid annual leave."},
        ]
        handlers = app.make_handlers(sections)
        job = FakeJob(payload={"question": ""})
        handler = handlers["docs.ask@1.0"]
        with self.assertRaises(app.NestError) as cm:
            handler(job)
        self.assertEqual(cm.exception.code, "invalid_operation_payload")


if __name__ == "__main__":
    unittest.main()
