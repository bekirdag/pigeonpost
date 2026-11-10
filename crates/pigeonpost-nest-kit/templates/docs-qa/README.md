# Docs Q&A

This template answers questions using only the Markdown documents bundled in the image. It splits those files on `## ` headings, selects the sections with the most shared words with the question, and passes them to the model broker. If no broker is available or the call fails, it quotes the best section directly and reports `model_used` as false. The template ships synthetic test data in `docs/handbook.md`; replace it with your own content.

## Start

Create a new app from this template:

    pigeonpost nest init docs-qa my-app

Use Python instead of Node (the default):

    pigeonpost nest init docs-qa my-app --lang python

The command names the app after the directory unless you pass `--name`. It copies the SDK file (`pigeonpost-nest.mjs` for Node, `pigeonpost_nest.py` for Python) into your app. The SDK has no dependencies and is not installed from a registry.

## Files

- `nest-release.toml`: The release manifest defining what the release is and may do.
- `operations/docs.ask.json`: The operation descriptor with input fields, output shape, and effects.
- `[config_schema]` in `nest-release.toml`: Defines buyer configuration (`answer_style`, `sections_per_answer`) read as `job.config`.
- `jobs/example.json`: A sample job for local testing.
- `app.mjs` or `app.py`: The application logic.
- `app.test.mjs` or `test_app.py`: Unit tests.
- `Containerfile`: Defines the runner image with a pinned base digest.
- `docs/`: Contains the Markdown files the app searches.

## Test

Run unit tests for Node:

    node --test app.test.mjs

Run unit tests for Python:

    python3 -m unittest test_app

## Run locally

Execute the sample job against your app using a mock model broker:

    pigeonpost nest run .

The runner prints progress, effects, and the result. It uses deterministic replies and token accounting. Options include `--job FILE`, `--config FILE`, `--no-model` (tests fallback without AI), `--reply TEXT` (sets mock reply), `--redeliver` (delivers jobs twice), `--approve-all` or `--approve KEY` (approves effects), and `--json`.

For this template, expect an answer quoting a section from `docs/handbook.md` with `model_used` set to false if you use `--no-model`.

## Make it yours

- Replace `docs/handbook.md` with your actual documentation files.
- Adjust `answer_style` (short or detailed) and `sections_per_answer` (1-4) in the config schema defaults if needed.
- Modify the section selection logic in `app.mjs` or `app.py` if your document structure differs from `## ` headings.
- Update the `summary` and `title` in `nest-release.toml` to reflect your specific use case.

## Before you submit

- Bump the version in `nest-release.toml`.
- Keep egress restricted to what you actually use (currently only "broker").
- Ensure no secrets are present in the source tree.
- List every dependency you ship in the licences table in `nest-release.toml`.
- An operation's fields are declared twice, in `nest-release.toml` and in its descriptor under `operations/`; keep the two the same.
- The build service builds Rust (Cargo) source trees today. Node and Python builds are coming with the publisher console; until then you can build, test and run this app locally. See https://developers.pigeonpost.dev/nest-review .
