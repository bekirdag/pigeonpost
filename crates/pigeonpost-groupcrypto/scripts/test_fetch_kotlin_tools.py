"""Real curl, local HTTP429/retry, atomic cache verification and shell-safe exports."""
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
import subprocess
import tempfile
from threading import Thread
import unittest


SCRIPT = Path(__file__).with_name("fetch-kotlin-tools.sh").resolve()
BODY = b"pinned test dependency\n"
SHA = hashlib.sha256(BODY).hexdigest()


class FetchTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name)
        self.requests = []
        requests = self.requests

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                requests.append(self.path)
                retry = self.path == "/retry" and requests.count("/retry") == 1
                body = b"rate limited" if retry else b"wrong bytes" if self.path == "/wrong" else BODY
                self.send_response(429 if retry else 200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_args):
                pass

        self.http = HTTPServer(("127.0.0.1", 0), Handler)
        self.thread = Thread(target=self.http.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.stop)

    def stop(self):
        self.http.shutdown()
        self.thread.join(timeout=5)
        self.http.server_close()

    def fetch(self, route, destination):
        return subprocess.run(["bash", "-c", 'source "$1"; fetch "$2" "$3" "$4"', "fetch-test",
                               str(SCRIPT), f"http://127.0.0.1:{self.http.server_port}{route}",
                               str(destination), SHA], capture_output=True, timeout=30)

    def test_rate_limit_retries_and_publishes_only_verified_bytes(self):
        destination = self.directory / "dependency.jar"
        result = self.fetch("/retry", destination)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(destination.read_bytes(), BODY)
        self.assertEqual(self.requests, ["/retry", "/retry"])
        self.assertEqual(list(self.directory.glob("*.download-*")), [])

    def test_wrong_bytes_do_not_become_a_cache_hit(self):
        destination = self.directory / "dependency.jar"
        self.assertNotEqual(self.fetch("/wrong", destination).returncode, 0)
        self.assertFalse(destination.exists())
        self.assertEqual(self.fetch("/good", destination).returncode, 0)
        self.assertEqual(destination.read_bytes(), BODY)

    def test_existing_cache_is_verified_without_redownloading(self):
        destination = self.directory / "dependency.jar"
        destination.write_bytes(BODY)
        self.assertEqual(self.fetch("/good", destination).returncode, 0)
        destination.write_bytes(b"tampered cache")
        self.assertNotEqual(self.fetch("/good", destination).returncode, 0)
        self.assertEqual(self.requests, [])

    def test_exports_preserve_spaces_and_literal_shell_characters(self):
        tools = self.directory / "space $(touch injected)"
        (tools / "kotlinc").mkdir(parents=True)
        shell = 'source "$1"; fetch() { :; }; values="$(main "$2")"; eval "$values"; printf "%s\\0%s\\0" "$KOTLINC_HOME" "$JNA_JAR"'
        result = subprocess.run(["bash", "-c", shell, "export-test", str(SCRIPT), str(tools)],
                                cwd=self.directory, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.split(b"\0")[:-1],
                         [str(tools / "kotlinc").encode(), str(tools / "jna-5.17.0.jar").encode()])
        self.assertFalse((self.directory / "injected").exists())


if __name__ == "__main__":
    unittest.main()
