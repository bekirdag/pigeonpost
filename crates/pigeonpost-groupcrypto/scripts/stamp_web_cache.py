"""Bind web module requests to the exact generated WASM digest."""
from pathlib import Path
import hashlib
import re
import sys


def replace_once(source, pattern, replacement):
    changed, count = re.subn(pattern, replacement, source)
    if count != 1:
        raise ValueError(f"Expected one cache version marker, found {count}")
    return changed


def stamp(repo, output, digest):
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise ValueError("Expected a lowercase SHA-256 digest")
    loader = (repo / "site-inbox/vendor/groupcrypto/loader.js").read_text()
    for module in ("pigeonpost_groupcrypto", "integrity"):
        loader = replace_once(loader,
            rf"(from ['\"]\./{module}\.js)(?:\?v=[^'\"]+)?(['\"])",
            rf"\g<1>?v={digest}\g<2>")
    groups = replace_once((repo / "site-inbox/groups.js").read_text(),
        r"(vendor/groupcrypto/loader\.js\?v=)[^'\"]+", rf"\g<1>{digest}")
    group_digest = hashlib.sha256(groups.encode("utf-8")).hexdigest()
    index = replace_once((repo / "site-inbox/index.html").read_text(),
        r'(src="groups\.js)(?:\?v=[^"]+)?(")', rf'\g<1>?v={group_digest}\g<2>')
    output.mkdir(parents=True, exist_ok=True)
    (output / "loader.js").write_text(loader)
    (output / "groups.js").write_text(groups)
    (output / "index.html").write_text(index)


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("usage: stamp_web_cache.py REPO OUTPUT WASM_SHA256")
    stamp(Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3])
