#!/usr/bin/env bash
set -Eeuo pipefail

umask 077

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd -P)
run_root=''

cleanup() {
  status=$?
  trap - EXIT
  if [[ -n "$run_root" && -d "$run_root" ]]; then
    case "$(basename -- "$run_root")" in
      pigeonpost-rollback.*) rm -R -- "$run_root" ;;
      *)
        echo "rollback-drill: refusing to remove unexpected path: $run_root" >&2
        status=1
        ;;
    esac
  fi
  exit "$status"
}

trap cleanup EXIT
trap 'exit 130' INT TERM HUP

for command_name in cargo sqlite3; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "rollback-drill: required command is unavailable: $command_name" >&2
    exit 1
  }
done

run_root=$(mktemp -d "${TMPDIR:-/tmp}/pigeonpost-rollback.XXXXXX")
export TMPDIR="$run_root"

restore_fixture() {
  local name=$1
  local fixture=$2
  local sentinel_sql=$3
  local database="$run_root/$name.db"
  local backup="$run_root/$name.backup.db"
  local before="$run_root/$name.before.sql"
  local after="$run_root/$name.after.sql"

  sqlite3 "$database" <"$fixture"
  sqlite3 "$database" "PRAGMA journal_mode=WAL; $sentinel_sql" >/dev/null
  test "$(sqlite3 "$database" 'PRAGMA integrity_check;')" = ok
  sqlite3 "$database" .dump >"$before"

  # SQLite's online backup API includes committed WAL content; a raw database-file copy is not an
  # accepted substitute for this production drill.
  sqlite3 "$database" ".timeout 5000" ".backup '$backup'"
  test "$(sqlite3 "$backup" 'PRAGMA integrity_check;')" = ok

  sqlite3 "$database" \
    "CREATE TABLE acceptance_upgrade_marker(value TEXT NOT NULL);
     INSERT INTO acceptance_upgrade_marker VALUES('post-backup mutation');
     PRAGMA user_version=99;"
  test "$(sqlite3 "$database" 'PRAGMA user_version;')" = 99

  # This is an in-place rollback while no writer is running. Production uses the same stopped-role
  # constraint, restores into a staging path first, verifies it, and only then replaces state.
  sqlite3 "$database" ".timeout 5000" ".restore '$backup'"
  test "$(sqlite3 "$database" 'PRAGMA integrity_check;')" = ok
  test "$(sqlite3 "$database" 'PRAGMA user_version;')" = 0
  test "$(sqlite3 "$database" \
    "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name='acceptance_upgrade_marker';")" = 0
  sqlite3 "$database" .dump >"$after"
  cmp "$before" "$after"
  echo "rollback-drill: $name WAL-aware backup and exact logical restore passed"
}

restore_fixture \
  client \
  "$repo_root/crates/pigeonpost-client/tests/fixtures/v0_1_0_state.sql" \
  "INSERT INTO meta(key, value) VALUES('acceptance', 'client');"
restore_fixture \
  loft \
  "$repo_root/crates/pigeonpost-loft/tests/fixtures/v0_1_0_loft.sql" \
  "INSERT INTO agent_records(address, seq, record) VALUES('/k/rollback', 1, X'00');"
restore_fixture \
  directory \
  "$repo_root/crates/pigeonpost-directory/tests/fixtures/v0_1_0_directory.sql" \
  "INSERT INTO probes(endpoint, at, result) VALUES('https://rollback.invalid', 1, X'7B7D');"
restore_fixture \
  registry \
  "$repo_root/crates/pigeonpost-registry/tests/fixtures/v0_1_0_registry.sql" \
  "SELECT COUNT(*) FROM entries;"

cd "$repo_root"
export RUSTFLAGS=${RUSTFLAGS:--D warnings}

cargo test --locked -p pigeonpost-client legacy_schema_is_migrated_without_losing_outbox_rows
cargo test --locked -p pigeonpost-client failed_v5_data_validation_rolls_back_schema_and_version
cargo test --locked -p pigeonpost-loft deployed_unversioned_schema_migrates_transactionally
cargo test --locked -p pigeonpost-loft corrupt_deployed_policy_rolls_back_the_v2_migration
cargo test --locked -p pigeonpost-directory opening_a_pre_sequence_database_applies_the_additive_migration
cargo test --locked -p pigeonpost-directory failed_v0_1_0_probe_backfill_rolls_the_entire_schema_migration_back
cargo test --locked -p pigeonpost-directory schema_three_migrates_transactionally_and_partial_shape_is_refused
cargo test --locked -p pigeonpost-registry --features test-utilities --test registry \
  nonempty_legacy_schema_requires_a_matching_signed_checkpoint -- --exact
cargo test --locked -p pigeonpost-registry --features test-utilities --test registry \
  unknown_legacy_kind_fails_closed_before_any_migration -- --exact

echo "rollback-drill: exact v0.1.0 fixture migrations and rollback refusals passed"
