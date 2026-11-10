# Helpdesk reply

This template drafts a customer support reply using an AI model and posts it to the buyer's helpdesk only after the buyer approves the action. The application never connects to the helpdesk directly; it sends a request to the platform, which performs the external write once approved. Note that external writes are not available at launch, so review will block this release until they are enabled, but you can build and test the flow locally now.

## Start

Create the app using the template name `approval-write`. The default is Node.js; use `--lang python` for Python.

    pigeonpost nest init approval-write my-app
    pigeonpost nest init approval-write my-app --lang python

The `init` command names the app after the directory (override with `--name`) and copies the SDK file (`pigeonpost-nest.mjs` or `pigeonpost_nest.py`) into your project. The SDK has no dependencies and is not installed from a registry.

## Files

- `nest-release.toml`: The release manifest defining what the app is and what it may do.
- `operations/helpdesk.reply.send.json`: The operation descriptor for input fields, output shape, and effects.
- `[config_schema]` in `nest-release.toml`: Defines what the buyer configures at install time; the app reads this as `job.config`.
- `jobs/example.json`: A sample job to try locally.
- `app.mjs` or `app.py`: The main application logic.
- `app.test.mjs` or `test_app.py`: Unit tests for the application.
- `Containerfile`: Defines the runner image with a base pinned by digest.

## Test

Run unit tests to verify logic without external dependencies.

    node --test app.test.mjs
    python3 -m unittest test_app

## Run locally

Use `pigeonpost nest run` to execute jobs against your app as a plain process. It uses a mock model broker for deterministic replies and prints progress, effects, and results.

    pigeonpost nest run . --approve-all

For this template, expect the job to initially fail with error code `approval_required`. With `--approve-all`, the platform approves the effect and redelivers the job. The final result will show `posted: true` and a reference ID. Using `--redeliver` demonstrates that the second delivery is deduplicated via the idempotency key, ensuring no double post.

Other options include `--job FILE` (repeatable), `--config FILE`, `--no-model` to test fallback logic, `--reply TEXT` for mock model output, and `--json` for machine-readable output.

## Make it yours

- Change the default signature in `[config_schema]` if your support team uses a standard sign-off.
- Modify the system prompt in `app.mjs` or `app.py` to adjust tone or constraints for drafting replies.
- Update `nest-release.toml` version and listing address to match your publisher details.
- Adjust operation fields in `operations/helpdesk.reply.send.json` if you need additional context inputs.

## Before you submit

- Bump the version number in `nest-release.toml`.
- Ensure egress permissions are limited to what you actually use (here, only `broker`).
- Do not include secrets or credentials in the source tree.
- Update the `[licences]` table in `nest-release.toml` to list every dependency you ship; this template ships none.
- An operation's fields are declared twice, in `nest-release.toml` and in its descriptor under `operations/`; keep the two the same.
- The build service builds Rust (Cargo) source trees today. Node and Python builds are coming with the publisher console; until then you can build, test and run this app locally. See https://developers.pigeonpost.dev/nest-review .
