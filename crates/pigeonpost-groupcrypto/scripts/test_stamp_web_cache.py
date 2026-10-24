import tempfile
from pathlib import Path
import unittest
from stamp_web_cache import stamp


class CacheStampTests(unittest.TestCase):
    def fixture(self, root):
        vendor = root / "site-inbox/vendor/groupcrypto"
        vendor.mkdir(parents=True)
        (vendor / "loader.js").write_text("import init from './pigeonpost_groupcrypto.js';\nimport { WASM_SHA256 } from './integrity.js';\n")
        (root / "site-inbox/groups.js").write_text('const url = "vendor/groupcrypto/loader.js?v=old";\n')
        (root / "site-inbox/index.html").write_text('<script src="groups.js?v=old"></script>\n')

    def test_new_digest_updates_every_request_and_restamping_is_idempotent(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.fixture(repo)
            output = repo / "stamped"
            for digest in ("a" * 64, "b" * 64):
                stamp(repo, output, digest)
                loader = (output / "loader.js").read_text()
                groups = (output / "groups.js").read_text()
                index = (output / "index.html").read_text()
                self.assertEqual(loader.count("?v=" + digest), 2)
                self.assertIn("loader.js?v=" + digest, groups)
                self.assertIn("groups.js?v=" + digest, index)
                (repo / "site-inbox/vendor/groupcrypto/loader.js").write_text(loader)
                (repo / "site-inbox/groups.js").write_text(groups)
                (repo / "site-inbox/index.html").write_text(index)
                stamp(repo, output, digest)
                self.assertEqual((output / "loader.js").read_text(), loader)
                self.assertEqual((output / "groups.js").read_text(), groups)
                self.assertEqual((output / "index.html").read_text(), index)

    def test_invalid_digest_and_missing_or_duplicate_import_refuse_to_stamp(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.fixture(repo)
            output = repo / "stamped"
            with self.assertRaises(ValueError):
                stamp(repo, output, "not-a-digest")
            loader = repo / "site-inbox/vendor/groupcrypto/loader.js"
            for broken in ("", loader.read_text() * 2):
                loader.write_text(broken)
                with self.assertRaises(ValueError):
                    stamp(repo, output, "a" * 64)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
