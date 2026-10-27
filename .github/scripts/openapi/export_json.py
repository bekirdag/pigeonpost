#!/usr/bin/env python3
"""Write the JSON mirror of each OpenAPI spec (`openapi/<name>.yaml` -> `openapi/<name>.json`).

The developer-site builder (site-developers/build.py) reads these mirrors with the standard
library only. check_openapi.py fails CI when a mirror differs from its YAML, so run this after
every spec edit. It uses the same strict loader (duplicate keys are an error) and preserves the
YAML's document order.

Usage: export_json.py [path ...]
       (default: openapi/pigeonpost.v1.yaml openapi/pigeonpost-org.v1.yaml)
"""

import sys

from check_openapi import DEFAULT_SPECS, load_spec, mirror_path, render_json


def main(argv):
    for path in argv[1:] or DEFAULT_SPECS:
        mirror = mirror_path(path)
        mirror.write_text(render_json(load_spec(path)), encoding="utf-8")
        print(f"wrote {mirror}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
