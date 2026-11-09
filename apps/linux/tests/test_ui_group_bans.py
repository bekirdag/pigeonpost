"""Unban in group info (SDS §4.4, audit P3-6): an admin sees banned mailboxes and can lift the ban
with `DELETE /v1/groups/{id}/bans/{address}`. Real GTK widgets, faked postbox. Run under Xvfb."""
import unittest

from pigeonpost.ui import Gtk
from test_ui import pump
from test_ui_features import ALL, WindowCase, find_button, walk

GID = "grp_" + "a" * 26
EVE = "/k/" + "e" * 26


class BanTests(WindowCase):
    features = ALL

    def setUp(self):
        super().setUp()
        self.scoped = []
        self.members = [{"address": "/k/one", "role": "owner", "state": "active", "revision": 1},
                        {"address": EVE, "role": "member", "state": "banned", "revision": 4}]

        def scoped(method, path, identity, data=None, **kwargs):
            self.scoped.append((method, path, identity))
            if method == "DELETE":
                self.members = [m for m in self.members if m["address"] != EVE]
                return {"unbanned": True}
            return {"members": [dict(m) for m in self.members]}
        self.api.scoped = scoped

    def row(self, role):
        g = {"id": GID, "name": "Garden", "my": {"role": role, "state": "active"}}
        return self.window.member_row(g, self.members[1], None)

    def test_admins_see_banned_members_and_can_unban(self):
        row = self.row("owner")
        self.assertEqual(row.get_subtitle(), "Banned")
        unban = find_button(row, "Unban")
        self.assertIsNotNone(unban)
        self.assertIsNone(find_button(row, "Ban"), "a banned member is not offered Ban again")
        unban.emit("clicked")
        pump(lambda: any(c[0] == "DELETE" for c in self.scoped), timeout=3)
        self.assertEqual(self.scoped[0], ("DELETE", f"/v1/groups/{GID}/bans/%2Fk%2F{'e' * 26}", "/k/one"))
        pump(lambda: len(self.scoped) >= 2, timeout=3)
        self.assertEqual(self.scoped[1][:2], ("GET", f"/v1/groups/{GID}/members"))
        self.assertEqual(self.window.group_members[GID], [self.members[0]])

    def test_members_without_admin_rights_get_no_unban(self):
        row = self.row("member")
        self.assertIsNone(find_button(row, "Unban"))
        self.assertFalse(any(isinstance(w, Gtk.Button) and w.get_tooltip_text() == "Unban" for w in walk(row)))


if __name__ == "__main__":
    unittest.main()
