"""Contacts, phone numbers and names in the real GTK window (SDS §14D): display names, local
aliases, phone verification, Find people I know and SMS invitations. The postbox is faked. Run
under Xvfb."""
import copy
import os
import tempfile
import unittest

from gi.repository import Gio

from pigeonpost import contacts_core as C
from pigeonpost.api import APIError
from pigeonpost.widgets import SettingsNavigation
from test_ui import pump
from test_ui_features import ALL, FeatureAPI, WindowCase, find_button, texts, walk

NAMES = frozenset({"display_names", "phone_numbers", "contact_discovery", "sms_invites"})
AVERY = "+15555550100"
BLAKE = "+447700900123"


class ContactsAPI(FeatureAPI):
    def __init__(self, features):
        super().__init__(features)
        self.names = {"/studio/reviewer": "Studio Rev", "/alex/main": "Alexandra"}
        self.own = None
        self.numbers = [{"id": "ph_1", "identity": "/k/two", "masked": "+44 ••• ••• ••23", "discoverable": False}]
        self.challenge = None

    def call(self, method, path, identity=None, data=None, **kwargs):
        query = kwargs.get("query") or {}
        record = (method, path, identity, copy.deepcopy(data), dict(query))
        if path == "/v1/profiles":
            self.calls.append(record)
            asked = query["addresses"].split(",")
            return {"profiles": {a: ({"address": a, "handle": a, "display_name": self.names[a]} if a in self.names else None) for a in asked}}
        if path == "/v1/profile/name":
            self.calls.append(record)
            if method == "PUT":
                self.own = dict(data)
                return {"name": dict(data, updated_at=1)}
            if method == "DELETE":
                self.own = None
                return {"removed": True}
            return {"name": self.own}
        if path == "/v1/phone":
            self.calls.append(record)
            return {"numbers": copy.deepcopy(self.numbers), "max_numbers": 3}
        if path == "/v1/phone/verify":
            self.calls.append(record)
            self.challenge = "chl_1"
            return {"challenge_id": "chl_1", "expires_at": 9999999999, "resend_after": 0, "code_length": 6}
        if path.startswith("/v1/phone/verify/"):
            self.calls.append(record)
            if data["code"] != "123456":
                raise APIError(400, "invalid_code", body={"attempts_left": 4})
            self.numbers.append({"id": "ph_2", "identity": "/k/one", "masked": "+90 ••• ••• ••67", "discoverable": data["discoverable"]})
            return {"number": self.numbers[-1]}
        if path.startswith("/v1/phone/"):
            self.calls.append(record)
            number = next(n for n in self.numbers if n["id"] == path.rsplit("/", 1)[1])
            if method == "DELETE":
                self.numbers.remove(number)
                return {"removed": True}
            number.update(data)
            return {"number": dict(number)}
        if path == "/v1/contacts/discover":
            self.calls.append(record)
            matches = [{"hash": h, "address": "/k/averykey", "handle": "/avery/main", "display_name": "Avery"}
                       for h in data["hashes"] if h == C.hash_phone(AVERY)]
            return {"matches": matches, "checked": len(data["hashes"]), "new": len(data["hashes"]), "remaining_today": 150}
        if path == "/v1/contacts/invites":
            self.calls.append(record)
            return {"status": "sent", "invite_id": "inv_1"}
        return super().call(method, path, identity, data, **kwargs)


class ContactsCase(WindowCase):
    features = ALL | NAMES

    def make_api(self):
        return ContactsAPI(self.features)

    def setUp(self):
        self.data = tempfile.TemporaryDirectory(prefix="pigeonpost-data-")
        self.previous_data = os.environ.get("XDG_DATA_HOME")
        os.environ["XDG_DATA_HOME"] = self.data.name
        super().setUp()

    def tearDown(self):
        super().tearDown()
        if self.previous_data is None:
            os.environ.pop("XDG_DATA_HOME", None)
        else:
            os.environ["XDG_DATA_HOME"] = self.previous_data
        self.data.cleanup()

    def peer_texts(self):
        return texts(self.window.peer_list)

    def settings_page(self):
        navigation = SettingsNavigation(self.window)
        return navigation


class OlderPostboxTests(ContactsCase):
    features = ALL

    def test_nothing_new_without_the_flags_but_aliases_work(self):
        self.window.settings()
        pump(timeout=0.1)
        settings = self.window.dialogs[-1]
        shown = texts(settings)
        for title in ("Display name", "Phone number", "Find people I know"):
            self.assertNotIn(title, shown)
        self.assertFalse(any(c[1] == "/v1/profiles" for c in self.api.calls))
        self.assertIsNone(self.window.find_people())
        self.assertIsNone(self.window.phone_verify_dialog())
        # Local aliases need no flag.
        self.select("/studio/reviewer")
        dialog = self.window.edit_alias()
        entry = next(w for w in walk(dialog) if w.__class__.__name__ == "Entry")
        entry.set_text("  My   reviewer ")
        find_button(dialog, "Save name").emit("clicked")
        pump(timeout=0.1)
        self.assertEqual(self.window.aliases.get("/studio/reviewer"), "My reviewer")
        self.assertIn("My reviewer", " ".join(self.peer_texts()))
        self.assertEqual(self.window.peer_title.get_text(), "My reviewer")
        self.assertEqual(self.window.peer_subtitle.get_text(), "/studio/reviewer")
        self.assertTrue(self.window.peer_subtitle.get_visible())
        self.assertFalse(any("aliases" in str(c) or "My reviewer" in str(c) for c in self.api.calls), "an alias never leaves the computer")


class NameTests(ContactsCase):
    def test_display_names_follow_the_precedence(self):
        pump(lambda: any(c[1] == "/v1/profiles" for c in self.api.calls) and "~Studio Rev" in " ".join(self.peer_texts()), timeout=3)
        joined = " ".join(self.peer_texts())
        self.assertIn("~Studio Rev", joined)
        self.assertIn("Alex", joined)  # the server contact alias beats the self-set name
        self.assertNotIn("~Alexandra", joined)
        lookups = [c for c in self.api.calls if c[1] == "/v1/profiles"]
        asked = [a for c in lookups for a in c[4]["addresses"].split(",")]
        self.assertTrue({"/alex/main", "/studio/reviewer"} <= set(asked))
        self.assertTrue(all(c[2] is None and len(c[4]["addresses"].split(",")) <= 100 for c in lookups),
                        "lookups are batched and carry no mailbox selector")
        self.window.aliases.set("/studio/reviewer", "Rev")
        self.window.render_peers()
        self.assertTrue(any(t.startswith("Rev") for t in self.peer_texts()))
        self.assertNotIn("~Studio Rev", " ".join(self.peer_texts()))

    def test_own_display_name_is_saved_for_the_acting_mailbox(self):
        navigation = self.settings_page()
        content = self.window.name_settings(navigation)
        pump(lambda: any(c[1] == "/v1/profile/name" for c in self.api.calls), timeout=2)
        entry = next(w for w in walk(content) if w.__class__.__name__ == "Entry")
        entry.set_text("Admin")
        find_button(content, "Save name").emit("clicked")
        pump(timeout=0.1)
        self.assertIn("That name is reserved. Choose another.", texts(content))
        self.assertFalse(any(c[0] == "PUT" for c in self.api.calls))
        entry.set_text("  Bekir   Dağ ")
        find_button(content, "Save name").emit("clicked")
        pump(lambda: self.api.own is not None, timeout=2)
        put = next(c for c in self.api.calls if c[0] == "PUT" and c[1] == "/v1/profile/name")
        self.assertEqual(put[3], {"display_name": "Bekir Dağ", "visibility": "known"}, "no identity in the body (deny_unknown_fields)")
        self.assertEqual(put[4], {"identity": "/k/one"})
        navigation.window.close()


class PhoneTests(ContactsCase):
    def test_add_verify_and_unlink(self):
        navigation = self.settings_page()
        content = self.window.phone_settings(navigation)
        pump(lambda: "+44 ••• ••• ••23" in texts(content), timeout=2)
        self.assertIn("On /demo/agent", texts(content))
        toggle = next(w for w in walk(content) if w.__class__.__name__ == "CheckButton")
        self.assertFalse(toggle.get_active())
        toggle.set_active(True)
        pump(lambda: any(c[0] == "PATCH" for c in self.api.calls), timeout=2)
        self.assertEqual(next(c for c in self.api.calls if c[0] == "PATCH")[3], {"discoverable": True})

        dialog = self.window.phone_verify_dialog(lambda: None)
        picker = next(w for w in walk(dialog) if w.__class__.__name__ == "DropDown")
        picker.set_selected(0)  # Türkiye
        entries = [w for w in walk(dialog) if w.__class__.__name__ == "Entry"]
        number = next(e for e in entries if e.get_placeholder_text() == "Phone number")
        number.set_text("0532 123 45 67")
        find_button(dialog, "Send code").emit("clicked")
        pump(lambda: self.api.challenge, timeout=2)
        start = next(c for c in self.api.calls if c[1] == "/v1/phone/verify")
        self.assertEqual(start[3], {"phone": "+905321234567", "identity": "/k/one"})
        pump(timeout=0.1)
        discoverable = next(w for w in walk(dialog) if w.__class__.__name__ == "CheckButton")
        self.assertFalse(discoverable.get_active(), "discoverability is off unless the person turns it on")
        code = next(e for e in entries if e.get_placeholder_text() == "6-digit code")
        code.set_text("000000")
        find_button(dialog, "Verify").emit("clicked")
        pump(lambda: "That code isn’t right. 4 tries left." in texts(dialog), timeout=2)
        code.set_text("123456")
        find_button(dialog, "Verify").emit("clicked")
        pump(lambda: dialog not in self.window.dialogs, timeout=2)
        check = [c for c in self.api.calls if c[1] == "/v1/phone/verify/chl_1"][-1]
        self.assertEqual(check[3], {"code": "123456", "discoverable": False})
        navigation.window.close()


class DiscoveryTests(ContactsCase):
    def test_file_hashes_only_then_start_chat_and_one_invite(self):
        navigation = self.settings_page()
        content = self.window.find_people(navigation)
        path = os.path.join(self.data.name, "contacts.vcf")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write("BEGIN:VCARD\nFN:Avery Example\nTEL:+1 555 555 0100\nEND:VCARD\n"
                         "BEGIN:VCARD\nFN:Blake Sample\nTEL:+44 7700 900123\nTEL:112\nEND:VCARD\n")
        content.read_contacts(Gio.File.new_for_path(path))
        pump(lambda: "On Pigeonpost" in texts(content) and "No match found" in texts(content), timeout=3)
        sent = [c for c in self.api.calls if c[1] == "/v1/contacts/discover"]
        self.assertEqual(len(sent), 1)
        self.assertEqual(sorted(sent[0][3]["hashes"]), sorted([C.hash_phone(AVERY), C.hash_phone(BLAKE)]))
        self.assertNotIn("Avery", str(sent), "names never leave the computer")
        shown = texts(content)
        self.assertIn("Avery Example", shown)
        self.assertIn("/avery/main", shown)
        self.assertIn("Blake Sample", shown)
        self.assertIn("Checked 2 of 2 numbers. 1 person is on Pigeonpost. 1 couldn’t be read. 150 new checks left today.", shown)
        self.assertFalse(any(c[1] == "/v1/contacts/invites" for c in self.api.calls), "nothing is sent without a tap")

        find_button(content, "Invite Blake Sample by text message").emit("clicked")
        pump(timeout=0.1)
        dialog = self.window.dialogs[-1]
        self.assertTrue(any(t.startswith("Pigeonpost: ") and "invited you to chat" in t for t in texts(dialog)))
        dialog.invite_send()
        pump(lambda: any(c[1] == "/v1/contacts/invites" for c in self.api.calls), timeout=2)
        invite = next(c for c in self.api.calls if c[1] == "/v1/contacts/invites")
        self.assertEqual(invite[3], {"phone": BLAKE, "confirm": True, "identity": "/k/one"})
        pump(lambda: "Invited" in texts(content), timeout=2)

        find_button(content, "Start chat").emit("clicked")
        pump(timeout=0.1)
        self.assertEqual(self.window.peer, "/avery/main")


if __name__ == "__main__":
    unittest.main()
