#!/usr/bin/env bash
set -Eeuo pipefail

umask 077

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd -P)
witness_helper="$script_dir/witness.js"

run_root=''
witness_pid=''
registry_pid=''
directory_pid=''
loft_one_pid=''
loft_two_pid=''

require_command() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "composed acceptance: required command is unavailable: $1" >&2
    exit 1
  }
}

stop_process() {
  local pid=${1:-}
  if [[ -z "$pid" ]]; then
    return 0
  fi
  if kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null || true
    for _ in {1..100}; do
      if ! kill -0 "$pid" 2>/dev/null; then
        break
      fi
      sleep 0.05
    done
    if kill -0 "$pid" 2>/dev/null; then
      kill -KILL "$pid" 2>/dev/null || true
    fi
  fi
  wait "$pid" 2>/dev/null || true
}

show_logs() {
  if [[ -z "$run_root" || ! -d "$run_root/logs" ]]; then
    return 0
  fi
  local log
  for log in "$run_root"/logs/*.log; do
    [[ -f "$log" ]] || continue
    echo "composed acceptance: tail of $(basename -- "$log")" >&2
    tail -n 60 "$log" >&2 || true
  done
}

cleanup() {
  local status=$?
  trap - EXIT
  stop_process "$loft_two_pid"
  stop_process "$loft_one_pid"
  stop_process "$directory_pid"
  stop_process "$registry_pid"
  stop_process "$witness_pid"

  if [[ "$status" -ne 0 ]]; then
    show_logs
  fi
  if [[ -n "$run_root" && -d "$run_root" ]]; then
    if [[ "${PIGEONPOST_ACCEPTANCE_KEEP:-0}" == 1 ]]; then
      echo "composed acceptance: retained isolated state at $run_root" >&2
    else
      case "$(basename -- "$run_root")" in
        pigeonpost-composed-acceptance.*) rm -R -- "$run_root" ;;
        *)
          echo "composed acceptance: refusing to remove unexpected path: $run_root" >&2
          status=1
          ;;
      esac
    fi
  fi
  exit "$status"
}

trap cleanup EXIT
trap 'exit 130' INT TERM HUP

start_process() {
  local pid_variable=$1
  local log=$2
  shift 2
  "$@" >>"$log" 2>&1 &
  local child=$!
  printf -v "$pid_variable" '%s' "$child"
}

wait_http() {
  local name=$1
  local pid=$2
  local url=$3
  local log=$4
  for _ in {1..200}; do
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "composed acceptance: $name exited before readiness" >&2
      tail -n 80 "$log" >&2 || true
      exit 1
    fi
    if curl --proto '=http' --noproxy '*' --max-time 1 --fail --silent "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
  echo "composed acceptance: $name did not become ready within 20 seconds" >&2
  tail -n 80 "$log" >&2 || true
  exit 1
}

allocate_ports() {
  node - <<'NODE'
const net = require("node:net");
const listeners = Array.from({ length: 5 }, () => net.createServer());
Promise.all(
  listeners.map(
    (listener) => new Promise((resolve, reject) => {
      listener.once("error", reject);
      listener.listen(0, "127.0.0.1", resolve);
    }),
  ),
).then(() => {
  process.stdout.write(`${listeners.map((listener) => listener.address().port).join(" ")}\n`);
  return Promise.all(listeners.map((listener) => new Promise((resolve) => listener.close(resolve))));
}).catch((error) => {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
});
NODE
}

json_field() {
  local field=$1
  node -e '
    let input = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => { input += chunk; });
    process.stdin.on("end", () => {
      let value = JSON.parse(input);
      for (const part of process.argv[1].split(".")) value = value[part];
      if (value === null || value === undefined || typeof value === "object") process.exit(2);
      process.stdout.write(String(value));
    });
  ' "$field"
}

field_from() {
  local value=$1
  local field=$2
  printf '%s' "$value" | json_field "$field"
}

assert_field() {
  local value=$1
  local field=$2
  local expected=$3
  local actual
  actual=$(field_from "$value" "$field")
  if [[ "$actual" != "$expected" ]]; then
    echo "composed acceptance: expected $field=$expected, got $actual" >&2
    exit 1
  fi
}

assert_probe_failure() {
  local document=$1
  local endpoint=$2
  EXPECTED_ENDPOINT="$endpoint" node -e '
    let input = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => { input += chunk; });
    process.stdin.on("end", () => {
      const document = JSON.parse(input);
      if (document.version !== 2 || document.endpoint !== process.env.EXPECTED_ENDPOINT) {
        throw new Error("probe document identity is invalid");
      }
      if (!Array.isArray(document.probes) || !document.probes.some((probe) =>
        probe.endpoint === process.env.EXPECTED_ENDPOINT &&
        probe.reachable === false &&
        probe.stored_and_returned === false &&
        probe.detail === "loft endpoint must use HTTPS")) {
        throw new Error("production prober did not reject the loopback HTTP loft as expected");
      }
    });
  ' <<<"$document"
}

assert_empty_directory_document() {
  local document=$1
  node -e '
    let input = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => { input += chunk; });
    process.stdin.on("end", () => {
      const document = JSON.parse(input);
      if (document.version !== 1 || !Array.isArray(document.lofts) || document.lofts.length !== 0) {
        throw new Error("pending locally probed lofts leaked into the selectable directory");
      }
      if (!/^[0-9a-f]{64}$/.test(document.signing_key)) {
        throw new Error("directory signing key is malformed");
      }
    });
  ' <<<"$document"
}

assert_inbox_has_id() {
  local inbox=$1
  local expected_id=$2
  EXPECTED_ID="$expected_id" node -e '
    let input = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => { input += chunk; });
    process.stdin.on("end", () => {
      const inbox = JSON.parse(input);
      if (!Array.isArray(inbox) || !inbox.some((item) => item.id === process.env.EXPECTED_ID)) {
        throw new Error("surviving loft did not deliver the expected message");
      }
    });
  ' <<<"$inbox"
}

assert_read_body() {
  local message=$1
  local body=$2
  # JavaScript template syntax belongs to Node, not the shell.
  # shellcheck disable=SC2016
  EXPECTED_BODY="$body" node -e '
    let input = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => { input += chunk; });
    process.stdin.on("end", () => {
      const message = JSON.parse(input);
      const open = message.fence?.open;
      const close = message.fence?.close;
      if (message.body_format !== "pigeonpost_fenced_untrusted_text_v1" ||
          typeof open !== "string" ||
          typeof close !== "string" ||
          process.env.EXPECTED_BODY.includes(open) ||
          process.env.EXPECTED_BODY.includes(close) ||
          message.untrusted_body !== `${open}\n${process.env.EXPECTED_BODY}\n${close}`) {
        throw new Error("read output did not contain the expected fenced body");
      }
    });
  ' <<<"$message"
}

require_command curl
require_command node

binary=${PIGEONPOST_BIN:-"$repo_root/target/debug/pigeonpost"}
if [[ -z "${PIGEONPOST_BIN:-}" ]]; then
  require_command cargo
  (cd "$repo_root" && cargo build --locked -p pigeonpost-cli)
fi
if [[ ! -f "$binary" || ! -x "$binary" ]]; then
  echo "composed acceptance: PIGEONPOST_BIN must name an executable regular file: $binary" >&2
  exit 1
fi
if [[ ! -f "$witness_helper" ]]; then
  echo "composed acceptance: test witness helper is missing: $witness_helper" >&2
  exit 1
fi

run_root=$(mktemp -d "${TMPDIR:-/tmp}/pigeonpost-composed-acceptance.XXXXXX")
mkdir -p "$run_root/logs" "$run_root/registry" "$run_root/directory" \
  "$run_root/loft-one" "$run_root/loft-two" "$run_root/discovery" \
  "$run_root/alice" "$run_root/bob"

read -r witness_port registry_port directory_port loft_one_port loft_two_port \
  <<<"$(allocate_ports)"
for port in "$witness_port" "$registry_port" "$directory_port" "$loft_one_port" "$loft_two_port"; do
  if [[ ! "$port" =~ ^[0-9]+$ ]]; then
    echo "composed acceptance: failed to allocate five loopback ports" >&2
    exit 1
  fi
done

origin='acceptance.pigeonpost/registry'
witness_name='acceptance.pigeonpost/witness'
witness_url="http://127.0.0.1:$witness_port"
registry_url="http://127.0.0.1:$registry_port"
directory_url="http://127.0.0.1:$directory_port"
loft_one_url="http://127.0.0.1:$loft_one_port"
loft_two_url="http://127.0.0.1:$loft_two_port"

node "$witness_helper" keygen "$run_root/witness.key"
node "$witness_helper" keygen "$run_root/registry/checkpoint.key"
node "$witness_helper" keygen "$run_root/directory/directory-signing.key"
witness_key=$(node "$witness_helper" public-key "$run_root/witness.key")
registry_key=$(node "$witness_helper" public-key "$run_root/registry/checkpoint.key")
directory_key=$(node "$witness_helper" public-key "$run_root/directory/directory-signing.key")

cat >"$run_root/registry/registry.toml" <<EOF
[server]
directory_publisher_keys = ["$directory_key"]

[witnessing]
threshold = 1
max_cosignature_age_seconds = 600
future_clock_skew_seconds = 30
max_lag_entries = 0
poll_interval_seconds = 1
connect_timeout_seconds = 1
request_timeout_seconds = 2
retry_initial_ms = 10
retry_max_ms = 50
retry_deadline_seconds = 2

[[witnessing.witnesses]]
name = "$witness_name"
public_key = "$witness_key"
submission_prefix = "$witness_url/submission/"
monitoring_prefix = "$witness_url/monitoring/"
EOF

cat >"$run_root/directory/directory.toml" <<EOF
witness_wait_seconds = 10
signing_key_file = "directory-signing.key"

[registry]
registry_url = "$registry_url"
expected_origin = "$origin"
registry_checkpoint_key = "$registry_key"
witness_threshold = 1
minimum_checkpoint_size = 0
minimum_checkpoint_root = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
max_staleness_seconds = 600
refresh_interval_seconds = 1
state_path = "registry-state.json"

[[registry.witnesses]]
name = "$witness_name"
public_key = "$witness_key"
EOF

start_process witness_pid "$run_root/logs/witness.log" \
  node "$witness_helper" serve \
    --seed "$run_root/witness.key" \
    --name "$witness_name" \
    --origin "$origin" \
    --operator-key "$registry_key" \
    --host 127.0.0.1 \
    --port "$witness_port"
wait_http witness "$witness_pid" "$witness_url/health" "$run_root/logs/witness.log"

start_process registry_pid "$run_root/logs/registry.log" \
  env PIGEONPOST_LOG=warn "$binary" registry serve \
    --dir "$run_root/registry" \
    --bind "127.0.0.1:$registry_port" \
    --origin "$origin"
wait_http registry "$registry_pid" "$registry_url/health" "$run_root/logs/registry.log"

initial_registry=$(curl --proto '=http' --noproxy '*' --max-time 2 --fail --silent \
  "$registry_url/v1/log/status")
assert_field "$initial_registry" ready true
assert_field "$initial_registry" committed_size 0
assert_field "$initial_registry" published_size 0

start_process loft_one_pid "$run_root/logs/loft-one.log" \
  env PIGEONPOST_LOG=warn "$binary" loft serve \
    --dir "$run_root/loft-one" \
    --bind "127.0.0.1:$loft_one_port" \
    --capacity-gb 1 \
    --retention-days 1
wait_http loft-one "$loft_one_pid" "$loft_one_url/ready" "$run_root/logs/loft-one.log"

start_process loft_two_pid "$run_root/logs/loft-two.log" \
  env PIGEONPOST_LOG=warn "$binary" loft serve \
    --dir "$run_root/loft-two" \
    --bind "127.0.0.1:$loft_two_port" \
    --capacity-gb 1 \
    --retention-days 1
wait_http loft-two "$loft_two_pid" "$loft_two_url/ready" "$run_root/logs/loft-two.log"

start_process directory_pid "$run_root/logs/directory.log" \
  env PIGEONPOST_LOG=warn "$binary" directory serve \
    --dir "$run_root/directory" \
    --bind "127.0.0.1:$directory_port"
wait_http directory "$directory_pid" "$directory_url/ready" "$run_root/logs/directory.log"
echo "composed acceptance: witness, registry, directory, and two loft roles started"

submission_one=$("$binary" --json loft submit \
  --directory "$directory_url" --endpoint "$loft_one_url" --dir "$run_root/loft-one")
submission_two=$("$binary" --json loft submit \
  --directory "$directory_url" --endpoint "$loft_two_url" --dir "$run_root/loft-two")
assert_field "$submission_one" state pending
assert_field "$submission_one" sequence 1
assert_field "$submission_two" state pending
assert_field "$submission_two" sequence 1

registry_status=$(curl --proto '=http' --noproxy '*' --max-time 2 --fail --silent \
  "$registry_url/v1/log/status")
assert_field "$registry_status" ready true
assert_field "$registry_status" committed_size 2
assert_field "$registry_status" published_size 2
assert_field "$registry_status" lag_entries 0
echo "composed acceptance: both loft submissions reached a fresh witnessed registry head"

# The supervised prober ticks immediately at process start. Restarting the disposable directory
# after enrollment forces a bounded sweep now instead of sleeping for its five-minute cadence.
stop_process "$directory_pid"
directory_pid=''
start_process directory_pid "$run_root/logs/directory-restarted.log" \
  env PIGEONPOST_LOG=warn "$binary" directory serve \
    --dir "$run_root/directory" \
    --bind "127.0.0.1:$directory_port"
wait_http directory "$directory_pid" "$directory_url/health" "$run_root/logs/directory-restarted.log"

probe_one=''
probe_two=''
for _ in {1..100}; do
  probe_one=$(curl --proto '=http' --noproxy '*' --max-time 2 --fail --silent --get \
    --data-urlencode "endpoint=$loft_one_url" "$directory_url/v1/probe")
  probe_two=$(curl --proto '=http' --noproxy '*' --max-time 2 --fail --silent --get \
    --data-urlencode "endpoint=$loft_two_url" "$directory_url/v1/probe")
  if [[ "$(node -e 'const d=JSON.parse(process.argv[1]); process.stdout.write(String(d.probes.length))' "$probe_one")" -gt 0 \
      && "$(node -e 'const d=JSON.parse(process.argv[1]); process.stdout.write(String(d.probes.length))' "$probe_two")" -gt 0 ]]; then
    break
  fi
  sleep 0.1
done
assert_probe_failure "$probe_one" "$loft_one_url"
assert_probe_failure "$probe_two" "$loft_two_url"
wait_http directory-ready "$directory_pid" "$directory_url/ready" "$run_root/logs/directory-restarted.log"
echo "composed acceptance: production prober examined both lofts and rejected local HTTP fail closed"

directory_document=$(curl --proto '=http' --noproxy '*' --max-time 2 --fail --silent \
  "$directory_url/directory.json")
assert_empty_directory_document "$directory_document"
directory_key=$(field_from "$directory_document" signing_key)

discovery_output=$(PIGEONPOST_HOME="$run_root/discovery" "$binary" directory add \
  "$directory_url" --key "$directory_key")
if [[ "$discovery_output" != *"with 0 loft(s)"* ]]; then
  echo "composed acceptance: signed directory import did not report the expected empty safe set" >&2
  exit 1
fi
refresh_output=$(PIGEONPOST_HOME="$run_root/discovery" "$binary" directory refresh)
bootstrap_stdout="$run_root/logs/discovery-bootstrap.stdout"
bootstrap_stderr="$run_root/logs/discovery-bootstrap.stderr"
if PIGEONPOST_HOME="$run_root/discovery" "$binary" directory bootstrap \
    >"$bootstrap_stdout" 2>"$bootstrap_stderr"; then
  echo "composed acceptance: empty safe directory unexpectedly bootstrapped" >&2
  exit 1
fi
# Match either spelling of the refusal: `NoLofts` is the error variant's Debug name, "no lofts
# configured" its Display text. The CLI now reports failures with Display so users read the
# sentence rather than the enum, and this check is about *which* refusal happened, not about which
# formatter produced it.
if [[ "$refresh_output" != "refreshed 1 signed directory snapshot(s)" \
    || -s "$bootstrap_stdout" \
    || ! $(<"$bootstrap_stderr") =~ (NoLofts|no\ lofts\ configured) ]]; then
  echo "composed acceptance: directory refresh/bootstrap did not report the expected NoLofts refusal" >&2
  exit 1
fi
selected=$(PIGEONPOST_HOME="$run_root/discovery" "$binary" --json loft list)
if [[ "$selected" != "[]" ]]; then
  echo "composed acceptance: an unqualified local loft was selected: $selected" >&2
  exit 1
fi
echo "composed acceptance: signed snapshot refresh and client bootstrap refused NoLofts fail closed"

alice() {
  PIGEONPOST_HOME="$run_root/alice" "$binary" "$@" 2>>"$run_root/logs/alice.log"
}
bob() {
  PIGEONPOST_HOME="$run_root/bob" "$binary" "$@" 2>>"$run_root/logs/bob.log"
}

alice_address=$(field_from "$(alice --json id)" address)
bob_address=$(field_from "$(bob --json id)" address)
alice loft add "$loft_one_url" >/dev/null
alice loft add "$loft_two_url" >/dev/null
bob loft add "$loft_one_url" >/dev/null
bob loft add "$loft_two_url" >/dev/null
bob allow "$alice_address" >/dev/null

stop_process "$loft_two_pid"
loft_two_pid=''
survivor_body='composed-two-loft-survivor'
send_report=$(alice --json send "$bob_address" --body "$survivor_body")
assert_field "$send_report" delivered 1
assert_field "$send_report" queued 1
assert_field "$send_report" terminal 0
message_id=$(field_from "$send_report" id)
inbox=$(bob --json inbox --limit 10)
assert_inbox_has_id "$inbox" "$message_id"
assert_read_body "$(bob --json read "$message_id")" "$survivor_body"
echo "composed acceptance: real-client delivery survived one of two lofts being offline"

start_process loft_two_pid "$run_root/logs/loft-two-restarted.log" \
  env PIGEONPOST_LOG=warn "$binary" loft serve \
    --dir "$run_root/loft-two" \
    --bind "127.0.0.1:$loft_two_port" \
    --capacity-gb 1 \
    --retention-days 1
wait_http loft-two "$loft_two_pid" "$loft_two_url/ready" "$run_root/logs/loft-two-restarted.log"

flush_report=''
for _ in {1..16}; do
  flush_report=$(alice --json flush)
  if [[ "$(field_from "$flush_report" queued)" == 0 ]]; then
    break
  fi
  sleep 1
done
assert_field "$flush_report" delivered 1
assert_field "$flush_report" queued 0
assert_field "$flush_report" terminalized 0

if grep -F "$survivor_body" "$run_root"/logs/*.log >/dev/null 2>&1; then
  echo "composed acceptance: message plaintext appeared in a service or client log" >&2
  exit 1
fi
echo "composed acceptance: recovered loft received its durable retry and logs stayed body-free"
echo "composed acceptance: all composed release-binary scenarios passed"
