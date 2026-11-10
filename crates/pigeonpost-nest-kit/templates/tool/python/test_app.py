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


class TestTool(unittest.TestCase):
    def test_split_line_quoted(self):
        self.assertEqual(app.split_line('a,"b, c",d', ","), ["a", "b, c", "d"])

    def test_split_line_escaped_quotes(self):
        self.assertEqual(app.split_line('x,"say ""hi"""', ","), ["x", 'say "hi"'])

    def test_summarize_basic(self):
        csv = "item,qty,price\nbolts,10,0.25\nnuts,4,0.10\nwashers,12,0.05"
        result = app.summarize(csv)
        self.assertEqual(result["rows"], 3)
        self.assertEqual(result["columns"], ["item", "qty", "price"])
        self.assertEqual(result["totals"]["qty"], 26)
        self.assertAlmostEqual(result["totals"]["price"], 0.4, places=1)

    def test_summarize_semicolon(self):
        csv = "a;b\n1;2"
        result = app.summarize(csv, ";")
        self.assertEqual(result["totals"], {"a": 1, "b": 2})

    def test_handler_summarize_decimal_places(self):
        job = FakeJob(payload={"csv": "n\n1.234\n2"}, config={"decimal_places": 1})
        handler = app.HANDLERS["csv.summarize@1.0"]
        result = handler(job)
        self.assertEqual(result["answer"]["rows"], 2)
        self.assertEqual(result["answer"]["columns"], ["n"])
        self.assertAlmostEqual(result["answer"]["totals"]["n"], 3.2, places=1)
        self.assertEqual(result["sources"], [])
        self.assertFalse(result["model_used"])

    def test_handler_invalid_payload_empty_csv(self):
        job = FakeJob(payload={"csv": "   "})
        handler = app.HANDLERS["csv.summarize@1.0"]
        with self.assertRaises(app.NestError) as cm:
            handler(job)
        self.assertEqual(cm.exception.code, "invalid_operation_payload")


if __name__ == "__main__":
    unittest.main()
