#!/usr/bin/env python3
"""Parse the OpenAPI specs as YAML, validate them as OpenAPI 3.1, and check their JSON mirrors.

The route-drift tests in crates/pigeonpost-postbox only scan the specs as text, so a bad merge that
interleaves two path blocks still passes them. This check loads each file with a strict YAML loader
(duplicate mapping keys are an error, not a silent overwrite) and runs the pinned OpenAPI 3.1
validator from requirements.txt over the result.

Each spec has a JSON mirror next to it (`pigeonpost.v1.yaml` -> `pigeonpost.v1.json`) so the
stdlib-only developer-site builder needs no YAML library. The mirror must be exactly what
export_json.py writes from the YAML; regenerate it with that script after editing a spec.

Usage: check_openapi.py [path ...]
       (default: openapi/pigeonpost.v1.yaml openapi/pigeonpost-org.v1.yaml)
"""

import json
import pathlib
import sys

import yaml
from openapi_spec_validator import OpenAPIV31SpecValidator

DEFAULT_SPECS = ["openapi/pigeonpost.v1.yaml", "openapi/pigeonpost-org.v1.yaml"]


class StrictLoader(yaml.SafeLoader):
    """SafeLoader that refuses duplicate keys in a mapping."""


def _construct_mapping(loader, node, deep=False):
    seen = {}
    for key_node, _ in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in seen:
            raise yaml.constructor.ConstructorError(
                "while constructing a mapping",
                node.start_mark,
                f"duplicate key {key!r} (first at line {seen[key] + 1})",
                key_node.start_mark,
            )
        seen[key] = key_node.start_mark.line
    return loader.construct_mapping(node, deep=deep)


StrictLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _construct_mapping
)


def load_spec(path):
    """The YAML document at `path`, parsed strictly. Raises yaml.YAMLError."""
    with open(path, encoding="utf-8") as handle:
        return yaml.load(handle, Loader=StrictLoader)  # noqa: S506 - SafeLoader subclass


def mirror_path(path):
    return pathlib.Path(path).with_suffix(".json")


def render_json(spec):
    """The exact text of a spec's JSON mirror: document order, two-space indent, UTF-8."""
    return json.dumps(spec, indent=2, ensure_ascii=False) + "\n"


def check(path):
    try:
        spec = load_spec(path)
    except (OSError, yaml.YAMLError) as error:
        print(f"::error file={path}::not valid YAML: {error}")
        return False
    if not isinstance(spec, dict) or not str(spec.get("openapi", "")).startswith("3.1."):
        print(f"::error file={path}::expected an OpenAPI 3.1.x document")
        return False
    errors = list(OpenAPIV31SpecValidator(spec).iter_errors())
    for error in errors:
        where = "/".join(str(part) for part in error.absolute_path) or "(root)"
        print(f"::error file={path}::{where}: {error.message}")
    if errors:
        return False
    mirror = mirror_path(path)
    try:
        expected = render_json(spec)
    except (TypeError, ValueError) as error:
        print(f"::error file={path}::cannot be represented as JSON: {error}")
        return False
    try:
        actual = mirror.read_text(encoding="utf-8")
    except OSError:
        actual = None
    if actual != expected:
        print(
            f"::error file={mirror}::JSON mirror is missing or differs from {path}; "
            "run python .github/scripts/openapi/export_json.py"
        )
        return False
    print(f"{path}: valid OpenAPI {spec['openapi']} ({len(spec.get('paths', {}))} paths), {mirror} in sync")
    return True


def main(argv):
    paths = argv[1:] or DEFAULT_SPECS
    results = [check(path) for path in paths]
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
