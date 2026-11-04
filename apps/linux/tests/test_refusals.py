"""Edit/reply refusal messages (SDS §14C) and meeting-notes load errors: no GUI."""

import unittest

from pigeonpost.features import EDIT_ERRORS, GROUPS_OFF, REPLY_ERRORS, explain, refusal
from pigeonpost.meeting_notes import load_error


class E(Exception):
    def __init__(self, status, code, message=None):
        super().__init__()
        self.status = status
        self.code = code
        self.body = {"message": message} if message else {}


class TestRefusals(unittest.TestCase):

    def test_explain_feature_disabled_edit(self):
        result = explain(E(403, "feature_disabled"), "edit")
        self.assertEqual(result, EDIT_ERRORS["feature_disabled"])
        self.assertNotIn("Groups", result)

    def test_explain_feature_disabled_reply(self):
        result = explain(E(403, "feature_disabled"), "reply")
        self.assertEqual(result, REPLY_ERRORS["feature_disabled"])
        self.assertNotIn("Groups", result)

    def test_groups_off_is_404_whatever_the_request(self):
        self.assertEqual(explain(E(404, "feature_disabled"), "edit"), GROUPS_OFF)
        self.assertEqual(explain(E(404, "feature_disabled"), "reply"), GROUPS_OFF)
        self.assertEqual(explain(E(404, "feature_disabled")), GROUPS_OFF)

    def test_other_403_feature_disabled_uses_the_server_wording(self):
        self.assertEqual(explain(E(403, "feature_disabled", "live streams are not enabled for this account")),
                         "Live streams are not enabled for this account.")
        self.assertEqual(explain(E(403, "feature_disabled")), "This is not enabled for this account.")

    def test_edit_window(self):
        self.assertEqual(explain(E(403, "not_editable"), "edit"), EDIT_ERRORS["not_editable"])
        self.assertEqual(refusal(E(403, "edit_window_closed"), "edit"), EDIT_ERRORS["edit_window_closed"])
        self.assertEqual(refusal(E(400, "edit_has_attachments"), "edit"), EDIT_ERRORS["edit_has_attachments"])
        self.assertEqual(refusal(E(404, "reference_not_found"), "reply"), REPLY_ERRORS["reference_not_found"])

    def test_rate_limited(self):
        self.assertTrue(refusal(E(429, "x"), "edit").startswith("Too many requests"))

    def test_other_group_refusals_keep_their_meaning(self):
        self.assertIsNone(refusal(E(403, "posting_restricted"), "edit"))
        self.assertEqual(explain(E(403, "posting_restricted"), "edit"), "Only admins can send messages here.")
        self.assertEqual(explain(E(403, "org_read_only"), "reply"),
                         "Your organization is read-only until its payment is settled.")
        self.assertIn("no longer one of your devices", explain(E(403, "unknown_device"), "edit"))
        self.assertEqual(explain(E(403, "not_member"), "edit"), "You are not a member of this group.")
        self.assertEqual(explain(E(409, "quota_exceeded"), "reply"), "This group is out of storage.")

    def test_no_context_is_not_a_refusal(self):
        self.assertIsNone(refusal(E(403, "feature_disabled"), None))

    def test_load_error(self):
        self.assertEqual(load_error(E(404, "feature_disabled")), "Meeting notes aren’t available for this account.")
        self.assertTrue(load_error(E(404, "not_found"), opening=True).startswith("These notes are no longer available"))
        self.assertTrue(load_error(E(0, "network_error")).startswith("Could not reach"))
        self.assertTrue(load_error(E(500, "server_error")).startswith("Couldn’t load"))
        self.assertTrue(load_error(E(500, "server_error"), opening=True).startswith("Couldn’t open"))


if __name__ == "__main__":
    unittest.main()
