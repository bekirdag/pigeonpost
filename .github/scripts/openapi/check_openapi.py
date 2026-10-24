#!/usr/bin/env python3
"""Parse openapi/pigeonpost.v1.yaml as YAML and validate it as an OpenAPI 3.1 document.

The route-drift test in crates/pigeonpost-postbox only scans the spec as text, so a bad merge that
interleaves two path blocks still passes it. This check loads the file with a strict YAML loader
(duplicate mapping keys are an error, not a silent overwrite) and runs the pinned OpenAPI 3.1
validator from requirements.txt over the result.

Usage: check_openapi.py [path ...]   (default: openapi/pigeonpost.v1.yaml)
"""

import sys

import yaml
from openapi_spec_validator import OpenAPIV31SpecValidator


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


def check(path):
    with open(path, encoding="utf-8") as handle:
        try:
            spec = yaml.load(handle, Loader=StrictLoader)  # noqa: S506 - SafeLoader subclass
        except yaml.YAMLError as error:
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
    print(f"{path}: valid OpenAPI {spec['openapi']} ({len(spec.get('paths', {}))} paths)")
    return True


def main(argv):
    paths = argv[1:] or ["openapi/pigeonpost.v1.yaml"]
    results = [check(path) for path in paths]
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
