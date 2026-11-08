"""Contacts, phone numbers and names (SDS §14D): the same tables as site-inbox/test/contacts.test.mjs."""
import json
import os
import stat
import tempfile
import unittest

from pigeonpost import contacts_core as C
from pigeonpost.api import APIError


class HashAndNumbers(unittest.TestCase):
    def test_hash_vectors_from_the_contract(self):
        self.assertEqual(C.hash_phone("+15555550100"), "7c0dc19d3eae376a630cee358d0f8912")
        self.assertEqual(C.hash_phone("+447700900123"), "5c4fd5bb0f19345b15863f865cb76cee")
        self.assertEqual(C.hash_phone("+61491570156"), "511ebc743c8a1d1ea8be5541f2fee95d")

    def test_e164_normalisation(self):
        table = [
            ("0532 123 45 67", "90", "+905321234567"),
            ("(555) 555-0100", "1", "+15555550100"),
            ("0044 7700 900123", "44", "+447700900123"),
            ("0044 7700 900123", "90", "+447700900123"),
            ("+1 555 555 0100 x12", "90", "+15555550100"),
            ("+1 (555) 555-0100 ext. 7", "1", "+15555550100"),
            ("tel:+1-555-555-0100;ext=5", "", "+15555550100"),
            ("1-555-555-0100", "1", "+15555550100"),
            ("555.555.0100", "+1", "+15555550100"),
            ("011 44 7700 900123", "1", "+447700900123"),
            ("07700 900123", "44", "+447700900123"),
            ("0491 570 156", "61", "+61491570156"),
            ("+44 (0)7700 900123", "1", "+447700900123"),
            ("555 0100", "", None),
            ("112", "44", None),
            ("0800-FLOWERS", "1", None),
            ("garbage", "1", None),
            ("", "1", None),
            (None, "1", None),
            ("+0123456789", "1", None),
            ("+1234567890123456", "1", None),
        ]
        for raw, cc, want in table:
            self.assertEqual(C.normalize_e164(raw, cc), want, f"{raw!r} with {cc!r}")

    def test_default_region_comes_from_the_locale(self):
        self.assertEqual(C.guess_calling_code({"LC_TELEPHONE": "tr_TR.UTF-8", "LANG": "en_US.UTF-8"}), "90")
        self.assertEqual(C.guess_calling_code({"LANG": "en_GB.UTF-8"}), "44")
        self.assertEqual(C.guess_calling_code({"LANG": "fr_CA.UTF-8"}), "1")
        self.assertEqual(C.guess_calling_code({"LANG": "C.UTF-8"}), "1")
        self.assertEqual(C.guess_calling_code({}), "1")


class Files(unittest.TestCase):
    def test_vcard(self):
        vcf = "\r\n".join([
            "BEGIN:VCARD", "VERSION:3.0", "FN:Avery Ex", " ample", "N:Example;Avery;;;",
            "TEL;TYPE=cell:+1 555 555 0100", "TEL;TYPE=work,voice:(555) 555-0101", "END:VCARD",
            "BEGIN:VCARD", "VERSION:2.1", "N;ENCODING=QUOTED-PRINTABLE;CHARSET=UTF-8:M=C3=BCller;J=C3=B6rg;;;",
            "FN;ENCODING=QUOTED-PRINTABLE;CHARSET=UTF-8:J=C3=B6rg =", "M=C3=BCller",
            "TEL;CELL:+44 7700 900123", "END:VCARD",
            "BEGIN:VCARD", "VERSION:4.0", "N:Tester;Casey;;;", "item1.TEL;type=pref:+61 491 570 156",
            "item1.X-ABLabel:mobile", "TEL;VALUE=uri;TYPE=home:tel:+1-555-555-0102", "END:VCARD",
            "BEGIN:VCARD", "VERSION:3.0", "FN:Nobody\\, No Phone", "ORG:Example Org", "END:VCARD",
        ])
        cards = C.parse_vcard(vcf)
        self.assertEqual(cards, [
            {"name": "Avery Example", "phones": ["+1 555 555 0100", "(555) 555-0101"]},
            {"name": "Jörg Müller", "phones": ["+44 7700 900123"]},
            {"name": "Casey Tester", "phones": ["+61 491 570 156", "tel:+1-555-555-0102"]},
            {"name": "Nobody, No Phone", "phones": []},
        ])
        self.assertEqual(C.normalize_e164(cards[2]["phones"][1], "90"), "+15555550102")
        self.assertEqual(C.parse_vcard("BEGIN:VCARD\nFN:Dana\n\tSample\nTEL:+15555550103\nEND:VCARD\n")[0]["name"], "DanaSample")

    def test_csv(self):
        google = "\r\n".join([
            "Name,Given Name,Family Name,Phone 1 - Type,Phone 1 - Value,Phone 2 - Type,Phone 2 - Value",
            '"Example, Avery",Avery,Example,Mobile,+1 555 555 0100 ::: +1 555 555 0101,Work,(555) 555-0102',
            'Blake Sample,Blake,Sample,Home,"+44 7700 900123",,',
        ])
        self.assertEqual(C.parse_csv(google), [
            {"name": "Example, Avery", "phones": ["+1 555 555 0100", "+1 555 555 0101", "(555) 555-0102"]},
            {"name": "Blake Sample", "phones": ["+44 7700 900123"]},
        ])
        google2024 = "First Name,Middle Name,Last Name,Phone 1 - Label,Phone 1 - Value\nCasey,J,Tester,Mobile,+61 491 570 156\n"
        self.assertEqual(C.parse_csv(google2024), [{"name": "Casey J Tester", "phones": ["+61 491 570 156"]}])
        outlook = "\n".join([
            "First Name,Last Name,Business Phone,Home Phone,Mobile Phone,Primary Phone,E-mail Address",
            'Dana,Sample,(555) 555-0104,,"555-555-0105",,dana@example.invalid',
            '"Multi ""Line""",Person,,"",+44 7700 900124,,',
        ])
        self.assertEqual(C.parse_csv(outlook), [
            {"name": "Dana Sample", "phones": ["(555) 555-0104", "555-555-0105"]},
            {"name": 'Multi "Line" Person', "phones": ["+44 7700 900124"]},
        ])
        generic = '﻿contact,Cell,Tel (work),Phone Type\n"Eli\nSample",+1 555 555 0106,+1 555 555 0107,mobile\n'
        self.assertEqual(C.parse_csv(generic), [{"name": "", "phones": ["+1 555 555 0106", "+1 555 555 0107"]}])

    def test_numbers_are_unique_and_unreadable_ones_are_counted(self):
        people = C.parse_contacts_file("BEGIN:VCARD\nTEL:+1 555 555 0100\nEND:VCARD\nBEGIN:VCARD\nFN:Avery\n"
                                       "TEL:(555) 555-0100\nTEL:112\nEND:VCARD\n")
        entries, unreadable = C.numbers_from(people, "1")
        self.assertEqual(unreadable, 1)
        self.assertEqual([(e["e164"], e["name"], e["hash"]) for e in entries],
                         [("+15555550100", "Avery", "7c0dc19d3eae376a630cee358d0f8912")])

    def test_batches_respect_the_new_account_allowance(self):
        hashes = [f"{i:032x}" for i in range(450)]
        self.assertEqual(len(C.next_batch(hashes, 0, None)), 200)
        self.assertEqual(C.next_batch(hashes, 200, 30), hashes[200:230])
        self.assertEqual(C.next_batch(hashes, 230, 0), [])
        self.assertEqual(C.next_batch(hashes, 400, 900), hashes[400:450])


class Names(unittest.TestCase):
    def test_precedence(self):
        self.assertEqual(C.pick_name("Mum", "Ops", "Alp", "Alp Main"), ("Mum", "alias"))
        self.assertEqual(C.pick_name(None, "Ops", "Alp", "Alp"), ("Ops", "contact"))
        self.assertEqual(C.pick_name("  ", "", "Alp K", "Alp"), ("~Alp K", "display"))
        self.assertEqual(C.pick_name(None, None, None, "Bekir"), ("Bekir", "derived"))
        self.assertEqual(C.pick_name(derived="/k/abcdefgh…"), ("/k/abcdefgh…", "derived"))

    def test_validation_mirrors_the_server(self):
        self.assertEqual(C.validate_display_name("  Bekir   Dağ "), (True, "Bekir Dağ"))
        for bad in ["", "   ", "a/b", "me@home", "Bob​", "Bob‮", "x" * 41, "...", "Admin",
                    "Pigeon Post Support", "pigeonpost fan"]:
            self.assertFalse(C.validate_display_name(bad)[0], repr(bad))
        self.assertTrue(C.validate_alias("Admin")[0])
        self.assertTrue(C.validate_alias("x" * 64)[0])
        self.assertFalse(C.validate_alias("x" * 65)[0])
        self.assertFalse(C.validate_alias("a\u0000b")[0])

    def test_invite_text(self):
        self.assertEqual(C.invite_name("Bekir 🚀 <Dağ>"), "Bekir Dağ")
        self.assertEqual(C.invite_name("🚀"), "A friend")
        self.assertEqual(C.invite_name(None), "A friend")
        self.assertEqual(C.invite_text("Bekir"), "Pigeonpost: Bekir invited you to chat on Pigeonpost. "
                         "Join: https://pigeonpost.dev/invite?c=… Reply STOP to opt out.")


class Answers(unittest.TestCase):
    def test_discovery_accepts_only_hashes_it_sent(self):
        sent = ["7c0dc19d3eae376a630cee358d0f8912"]
        matches, remaining = C.parse_discovery({"matches": [
            {"hash": sent[0], "address": "/k/abc", "handle": "/bekir/main", "display_name": "Bekir"},
            {"hash": "5c4fd5bb0f19345b15863f865cb76cee", "address": "/k/other"},
            {"hash": sent[0], "address": "not-an-address"},
        ], "remaining_today": 186}, sent)
        self.assertEqual(matches, {sent[0]: {"address": "/k/abc", "handle": "/bekir/main", "display_name": "Bekir"}})
        self.assertEqual(remaining, 186)
        self.assertEqual(C.parse_discovery({"matches": "x", "remaining_today": True}, sent), ({}, None))

    def test_phones_profiles_and_own_name(self):
        numbers, maximum = C.parse_phones({"numbers": [
            {"id": "ph_1", "identity": "/k/one", "masked": "+90 ••• ••• ••12", "discoverable": True},
            {"id": "", "identity": "/k/one"}, "junk"], "max_numbers": 3})
        self.assertEqual(numbers, [{"id": "ph_1", "identity": "/k/one", "masked": "+90 ••• ••• ••12", "discoverable": True}])
        self.assertEqual(maximum, 3)
        self.assertEqual(C.parse_profiles({"profiles": {"/a/main": {"display_name": "Alp"}, "/b/main": None, "/x": {"display_name": "Not asked"}}},
                                          ["/a/main", "/b/main", "/c/main"]),
                         {"/a/main": "Alp", "/b/main": None, "/c/main": None})
        self.assertEqual(C.parse_own_name({"name": {"display_name": "Bekir", "visibility": "everyone"}}),
                         {"display_name": "Bekir", "visibility": "everyone"})
        self.assertIsNone(C.parse_own_name({"name": None}))

    def test_refusal_words(self):
        self.assertEqual(C.phone_error(APIError(400, "invalid_code", body={"attempts_left": 1})), "That code isn’t right. 1 try left.")
        self.assertIn("2 minutes", C.phone_error(APIError(429, "rate_limited", headers={"Retry-After": "120"})))
        self.assertEqual(C.phone_error(APIError(0, "network_error")), "Could not reach Pigeonpost. Please try again.")
        self.assertIn("today", C.discover_error(APIError(429, "rate_limited", body={"limit": "daily"})))
        self.assertIn("paused", C.discover_error(APIError(403, "discovery_locked", body={"until": 1893456000})))
        self.assertEqual(C.invite_error(APIError(403, "phone_required")), "Verify your own number first.")
        self.assertEqual(C.invite_outcome({"status": "sent", "invite_id": "inv_1"})[0], True)
        self.assertEqual(C.invite_outcome({"status": "not_sent", "reason": "already_invited"})[1], "Already invited")
        self.assertEqual(C.invite_outcome({"status": "not_sent", "reason": "unavailable"})[1], "Can’t be invited right now")
        self.assertEqual(C.name_error(APIError(429, "rate_limited")), "You can change your name 10 times a day. Try again tomorrow.")


class LocalAliases(unittest.TestCase):
    def test_aliases_are_per_account_private_and_cleared_on_sign_out(self):
        with tempfile.TemporaryDirectory() as root:
            path = os.path.join(root, "pigeonpost", "aliases.json")
            store = C.Aliases(path)
            store.set("/alp/main", "Mum")  # unbound: memory only, nothing written
            self.assertFalse(os.path.exists(path))
            store.bind("owner-a")
            self.assertEqual(store.set("/alp/main", "  Alp   from work "), "Alp from work")
            self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o600)
            with self.assertRaises(ValueError):
                store.set("/alp/main", "a​b")
            other = C.Aliases(path)
            other.bind("owner-b")
            self.assertIsNone(other.get("/alp/main"))
            other.set("/bo/main", "Bo")
            again = C.Aliases(path)
            again.bind("owner-a")
            self.assertEqual(again.all(), {"/alp/main": "Alp from work"})
            again.remove("/alp/main")
            self.assertEqual(json.loads(C.Path(path).read_text(encoding="utf-8")), {"owner-b": {"/bo/main": "Bo"}})
            again.clear_all()
            self.assertFalse(os.path.exists(path))
            self.assertEqual(again.all(), {})

    def test_unreadable_file_leaves_an_empty_store(self):
        with tempfile.TemporaryDirectory() as root:
            path = os.path.join(root, "aliases.json")
            C.Path(path).write_text("{not json")
            store = C.Aliases(path)
            store.bind("owner-a")
            self.assertEqual(store.all(), {})

    def test_default_path_follows_xdg(self):
        self.assertEqual(str(C.aliases_path({"XDG_DATA_HOME": "/data"})), "/data/pigeonpost/aliases.json")


if __name__ == "__main__":
    unittest.main()
