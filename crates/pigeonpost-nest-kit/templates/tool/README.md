# CSV summary

This template provides a deterministic tool that parses CSV text, counts rows and columns, and sums numeric values. It operates without an AI model or network access. The manifest explicitly declares no egress capabilities.

## Start

Initialize the application using the `tool` template. This copies the SDK and creates the directory structure.

    pigeonpost nest init tool my-app

Use `--lang python` to generate a Python project instead. The SDK has no external dependencies and is copied directly into your workspace.

## Files

- `nest-release.toml`: Defines the release version, title, and permissions.
- `operations/csv.summarize.json`: Describes the input fields and output structure.
- `[config_schema]` in `nest-release.toml`: Validates buyer configuration options.
- `jobs/example.json`: A sample job for local testing.
- `app.mjs` or `app.py`: The main application logic.
- `app.test.mjs` or `test_app.py`: Unit tests for the core functions.
- `Containerfile`: Defines the container image with a pinned base digest.

## Test

Run unit tests to verify parsing and summation logic.

    node --test app.test.mjs

For Python projects:

    python3 -m unittest test_app

## Run locally

Execute the example job against your local code. No model broker is started because this tool does not use AI.

    pigeonpost nest run .

The command prints progress, effects, and the final result object containing row counts, column names, and numeric totals.

## Make it yours

- Update `name` and `version` in `nest-release.toml`.
- Adjust `decimal_places` limits in `[config_schema]` if needed.
- Modify the parsing logic in `app.mjs` or `app.py` to handle specific CSV dialects.
- Change the operation name and version in both the manifest and the JSON descriptor if you rename the feature.

## Before you submit

- Bump the version number in the release manifest.
- Ensure egress remains empty since this tool requires no network access.
- Verify there are no secrets or credentials in the source tree.
- Confirm the licences table lists every dependency shipped with the app (currently none).
- An operation's fields are declared twice, in `nest-release.toml` and in its descriptor under `operations/`; keep the two the same.
- The build service builds Rust (Cargo) source trees today. Node and Python builds are coming with the publisher console; until then you can build, test and run this app locally. See https://developers.pigeonpost.dev/nest-review .
