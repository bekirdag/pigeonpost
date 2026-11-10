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


class TestApprovalWrite(unittest.TestCase):
    def test_effect_key(self):
        self.assertEqual(app.effect_key("job_x", "T 1/2"), "reply:T_1_2:job_x")

    def test_plain_reply(self):
        result = app.plain_reply(" Refund approved. ", "Team")
        self.assertEqual(result, "Hello,\n\nRefund approved.\n\nTeam")

    def test_handler_no_broker_effect_success(self):
        captured_kwargs = None
        def effect(**kwargs):
            nonlocal captured_kwargs
            captured_kwargs = kwargs
            return {"reference": "HD-1", "body": {"ticket_id": "T-1", "reply": "APPROVED TEXT"}}

        job = FakeJob(
            payload={"ticket_id": "T-1", "notes": "Refund approved."},
            config={"signature": "Team"},
            effect=effect
        )
        handler = app.HANDLERS["helpdesk.reply.send@1.0"]
        result = handler(job)

        self.assertEqual(result["answer"]["ticket_id"], "T-1")
        self.assertEqual(result["answer"]["reply"], "APPROVED TEXT")
        self.assertTrue(result["answer"]["posted"])
        self.assertEqual(result["answer"]["reference"], "HD-1")
        self.assertEqual(result["sources"], [])
        self.assertFalse(result["model_used"])

        self.assertEqual(captured_kwargs["kind"], "helpdesk.reply.post")
        self.assertTrue(captured_kwargs["requires_approval"])
        self.assertEqual(captured_kwargs["key"], "reply:T-1:job_aaaaaaaaaaaaaaaaaaaaaaaaaa")
        self.assertEqual(captured_kwargs["body"]["reply"], "Hello,\n\nRefund approved.\n\nTeam")

    def test_handler_effect_failure(self):
        def effect(**kwargs):
            raise app.NestError("approval_required")

        job = FakeJob(
            payload={"ticket_id": "T-1", "notes": "Refund approved."},
            config={"signature": "Team"},
            effect=effect
        )
        handler = app.HANDLERS["helpdesk.reply.send@1.0"]
        with self.assertRaises(app.NestError) as cm:
            handler(job)
        self.assertEqual(cm.exception.code, "approval_required")

    def test_handler_with_broker_success(self):
        def complete(**kwargs):
            return {"text": "Your refund is on its way.", "usage": {"input": 1, "output": 1}}

        def effect(**kwargs):
            return {"reference": "HD-2"}

        broker = FakeBroker(available=True, complete=complete)
        job = FakeJob(
            payload={"ticket_id": "T-1", "notes": "Refund approved."},
            config={},
            broker=broker,
            effect=effect
        )
        handler = app.HANDLERS["helpdesk.reply.send@1.0"]
        result = handler(job)

        self.assertEqual(result["answer"]["reply"], "Your refund is on its way.\n\nThe support team")
        self.assertTrue(result["model_used"])

    def test_handler_missing_notes(self):
        job = FakeJob(payload={"ticket_id": "T-1"})
        handler = app.HANDLERS["helpdesk.reply.send@1.0"]
        with self.assertRaises(app.NestError) as cm:
            handler(job)
        self.assertEqual(cm.exception.code, "invalid_operation_payload")


if __name__ == "__main__":
    unittest.main()
