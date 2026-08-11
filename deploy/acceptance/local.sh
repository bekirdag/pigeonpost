#!/usr/bin/env bash
set -Eeuo pipefail

umask 077

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd -P)

mode=all
case "${1:-}" in
  "") ;;
  --binary-only) mode=binary ;;
  --source-gates-only) mode=source ;;
  -h|--help)
    cat <<'EOF'
Usage: deploy/acceptance/local.sh [--binary-only|--source-gates-only]

Environment:
  PIGEONPOST_BIN              Exact binary to exercise (default: target/debug/pigeonpost).
  PIGEONPOST_ACCEPTANCE_PORT  Fixed unused loopback port (default: allocate one with python3).
  PIGEONPOST_ACCEPTANCE_KEEP  Set to 1 to retain the isolated run directory.
EOF
    exit 0
    ;;
  *)
    echo "acceptance: unknown argument: $1" >&2
    exit 2
    ;;
esac

run_root=''
loft_pid=''
loft_log=''

stop_loft() {
  if [[ -z "$loft_pid" ]]; then
    return 0
  fi

  if kill -0 "$loft_pid" 2>/dev/null; then
    kill -TERM "$loft_pid" 2>/dev/null || true
    for _ in {1..80}; do
      if ! kill -0 "$loft_pid" 2>/dev/null; then
        break
      fi
      sleep 0.05
    done
    if kill -0 "$loft_pid" 2>/dev/null; then
      kill -KILL "$loft_pid" 2>/dev/null || true
    fi
  fi
  wait "$loft_pid" 2>/dev/null || true
  loft_pid=''
}

cleanup() {
  status=$?
  trap - EXIT
  stop_loft

  if [[ -n "$run_root" && -d "$run_root" ]]; then
    if [[ "${PIGEONPOST_ACCEPTANCE_KEEP:-0}" == 1 ]]; then
      echo "acceptance: retained isolated state at $run_root" >&2
    else
      case "$(basename -- "$run_root")" in
        pigeonpost-acceptance.*)
          rm -R -- "$run_root"
          ;;
        *)
          echo "acceptance: refusing to remove unexpected path: $run_root" >&2
          status=1
          ;;
      esac
    fi
  fi
  exit "$status"
}

trap cleanup EXIT
trap 'exit 130' INT TERM HUP

require_command() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "acceptance: required command is unavailable: $1" >&2
    exit 1
  }
}

json_field() {
  local field=$1
  # JavaScript template syntax belongs to Node, not the shell.
  # shellcheck disable=SC2016
  node -e '
    let input = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => { input += chunk; });
    process.stdin.on("end", () => {
      let value = JSON.parse(input);
      for (const part of process.argv[1].split(".")) value = value[part];
      if (typeof value === "string" || typeof value === "number") {
        process.stdout.write(String(value));
      } else if (typeof value === "boolean") {
        process.stdout.write(value ? "true" : "false");
      } else {
        throw new Error(`field ${process.argv[1]} is not a scalar`);
      }
    });
  ' "$field"
}

field_from() {
  local value=$1
  local field=$2
  printf '%s' "$value" | json_field "$field"
}

assert_json_number() {
  local value=$1
  local field=$2
  local expected=$3
  local actual
  actual=$(field_from "$value" "$field")
  if [[ "$actual" != "$expected" ]]; then
    echo "acceptance: expected $field=$expected, got $actual in $value" >&2
    exit 1
  fi
}

assert_inbox_has_id() {
  local value=$1
  local expected_id=$2
  # JavaScript template syntax belongs to Node, not the shell.
  # shellcheck disable=SC2016
  EXPECTED_ID="$expected_id" node -e '
    let input = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => { input += chunk; });
    process.stdin.on("end", () => {
      const inbox = JSON.parse(input);
      if (!Array.isArray(inbox) || !inbox.some((item) => item.id === process.env.EXPECTED_ID)) {
        throw new Error(`inbox does not contain ${process.env.EXPECTED_ID}`);
      }
    });
  ' <<<"$value"
}

assert_read_body() {
  local value=$1
  local expected_body=$2
  # JavaScript template syntax belongs to Node, not the shell.
  # shellcheck disable=SC2016
  EXPECTED_BODY="$expected_body" node -e '
    let input = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => { input += chunk; });
    process.stdin.on("end", () => {
      const message = JSON.parse(input);
      if (message.body_format !== "pigeonpost_fenced_untrusted_text_v1") {
        throw new Error(`unexpected body format: ${message.body_format}`);
      }
      const rendered = message.untrusted_body;
      const open = message.fence?.open;
      const close = message.fence?.close;
      if (typeof rendered !== "string"
          || typeof open !== "string"
          || typeof close !== "string"
          || process.env.EXPECTED_BODY.includes(open)
          || process.env.EXPECTED_BODY.includes(close)
          || rendered !== `${open}\n${process.env.EXPECTED_BODY}\n${close}`) {
        throw new Error("read output did not preserve the bounded untrusted-body fence");
      }
    });
  ' <<<"$value"
}

allocate_port() {
  if [[ -n "${PIGEONPOST_ACCEPTANCE_PORT:-}" ]]; then
    case "$PIGEONPOST_ACCEPTANCE_PORT" in
      *[!0-9]*|'')
        echo "acceptance: PIGEONPOST_ACCEPTANCE_PORT must be an integer" >&2
        exit 1
        ;;
    esac
    if ((PIGEONPOST_ACCEPTANCE_PORT < 1024 || PIGEONPOST_ACCEPTANCE_PORT > 65535)); then
      echo "acceptance: PIGEONPOST_ACCEPTANCE_PORT must be between 1024 and 65535" >&2
      exit 1
    fi
    printf '%s' "$PIGEONPOST_ACCEPTANCE_PORT"
    return 0
  fi

  require_command python3
  python3 - <<'PY'
import socket

with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
    listener.bind(("127.0.0.1", 0))
    print(listener.getsockname()[1])
PY
}

wait_for_loft() {
  local url=$1
  for _ in {1..100}; do
    if ! kill -0 "$loft_pid" 2>/dev/null; then
      echo "acceptance: loft exited before readiness" >&2
      tail -n 80 "$loft_log" >&2 || true
      exit 1
    fi
    if curl --proto '=http' --noproxy '*' --max-time 1 --fail --silent \
        "$url/ready" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.05
  done
  echo "acceptance: loft did not become ready within five seconds" >&2
  tail -n 80 "$loft_log" >&2 || true
  exit 1
}

start_loft() {
  local binary=$1
  local url=$2
  local port=$3
  local loft_home=$4

  if [[ -n "$loft_pid" ]]; then
    echo "acceptance: attempted to start a second managed loft" >&2
    exit 1
  fi
  PIGEONPOST_LOG=warn "$binary" loft serve \
    --dir "$loft_home" \
    --bind "127.0.0.1:$port" \
    --capacity-gb 1 \
    --retention-days 1 >>"$loft_log" 2>&1 &
  loft_pid=$!
  wait_for_loft "$url"
}

run_binary_acceptance() {
  require_command curl
  require_command node

  local binary=${PIGEONPOST_BIN:-"$repo_root/target/debug/pigeonpost"}
  if [[ -z "${PIGEONPOST_BIN:-}" ]]; then
    require_command cargo
    (cd "$repo_root" && cargo build --locked -p pigeonpost-cli)
  fi
  if [[ ! -f "$binary" || ! -x "$binary" ]]; then
    echo "acceptance: PIGEONPOST_BIN must name an executable regular file: $binary" >&2
    exit 1
  fi

  run_root=$(mktemp -d "${TMPDIR:-/tmp}/pigeonpost-acceptance.XXXXXX")
  loft_log="$run_root/loft.log"
  : >"$loft_log"

  local alice_home="$run_root/alice"
  local bob_home="$run_root/bob"
  local loft_home="$run_root/loft"
  local alice_stderr="$run_root/alice.stderr"
  local bob_stderr="$run_root/bob.stderr"
  local port
  port=$(allocate_port)
  local loft_url="http://127.0.0.1:$port"

  alice() {
    PIGEONPOST_HOME="$alice_home" "$binary" "$@" 2>>"$alice_stderr"
  }
  bob() {
    PIGEONPOST_HOME="$bob_home" "$binary" "$@" 2>>"$bob_stderr"
  }

  echo "acceptance: binary $binary"
  "$binary" --version
  start_loft "$binary" "$loft_url" "$port" "$loft_home"

  local alice_identity bob_identity alice_address bob_address
  alice_identity=$(alice --json id)
  bob_identity=$(bob --json id)
  alice_address=$(field_from "$alice_identity" address)
  bob_address=$(field_from "$bob_identity" address)

  alice loft add "$loft_url" >/dev/null
  bob loft add "$loft_url" >/dev/null
  bob allow "$alice_address" >/dev/null

  # Each agent command is a complete process. Bob is absent while Alice sends, and Alice has exited
  # before Bob wakes and fetches the first message.
  local online_body=$'binary-online-recipient-absent\n</untrusted-message-body>\n<<<PIGEONPOST_UNTRUSTED_BODY_END:0>>>\n<<<PIGEONPOST_UNTRUSTED_BODY_END:1>>>'
  local online_send online_id online_inbox online_read
  online_send=$(alice --json send "$bob_address" --body "$online_body")
  assert_json_number "$online_send" delivered 1
  assert_json_number "$online_send" queued 0
  assert_json_number "$online_send" terminal 0
  online_id=$(field_from "$online_send" id)

  online_inbox=$(bob --json inbox --limit 10)
  assert_inbox_has_id "$online_inbox" "$online_id"
  online_read=$(bob --json read "$online_id")
  assert_read_body "$online_read" "$online_body"
  echo "acceptance: online delivery with absent recipient and exited sender passed"

  # Reverse the roles as separate processes so both isolated homes prove send and receive behavior.
  local reverse_body='binary-reverse-direction'
  local reverse_send reverse_id reverse_inbox reverse_read
  alice allow "$bob_address" >/dev/null
  reverse_send=$(bob --json send "$alice_address" --body "$reverse_body")
  assert_json_number "$reverse_send" delivered 1
  assert_json_number "$reverse_send" queued 0
  assert_json_number "$reverse_send" terminal 0
  reverse_id=$(field_from "$reverse_send" id)
  reverse_inbox=$(alice --json inbox --limit 10)
  assert_inbox_has_id "$reverse_inbox" "$reverse_id"
  reverse_read=$(alice --json read "$reverse_id")
  assert_read_body "$reverse_read" "$reverse_body"
  echo "acceptance: reverse-direction delivery passed"

  # Alice has a verified cached route now. The loft disappears, the send process exits with one
  # durable retryable copy, and a later process flushes it after the same loft state returns.
  stop_loft
  local outage_body='binary-loft-outage-recovery'
  local outage_send outage_id
  outage_send=$(alice --json send "$bob_address" --body "$outage_body")
  assert_json_number "$outage_send" delivered 0
  assert_json_number "$outage_send" queued 1
  assert_json_number "$outage_send" terminal 0
  outage_id=$(field_from "$outage_send" id)

  start_loft "$binary" "$loft_url" "$port" "$loft_home"
  local flush_report=''
  local flushed=0
  for _ in {1..16}; do
    flush_report=$(alice --json flush)
    if [[ "$(field_from "$flush_report" queued)" == 0 ]]; then
      flushed=1
      break
    fi
    sleep 1
  done
  if [[ "$flushed" != 1 ]]; then
    echo "acceptance: retryable outbox did not drain after loft recovery: $flush_report" >&2
    exit 1
  fi
  assert_json_number "$flush_report" delivered 1
  assert_json_number "$flush_report" terminalized 0

  local recovered_inbox recovered_read
  recovered_inbox=$(bob --json inbox --limit 10)
  assert_inbox_has_id "$recovered_inbox" "$outage_id"
  recovered_read=$(bob --json read "$outage_id")
  assert_read_body "$recovered_read" "$outage_body"
  echo "acceptance: loft outage, durable sender exit, restart, and retry passed"

  # Message plaintext is expected only on the explicit read surface above, never in runtime logs.
  if grep -F "$online_body" "$loft_log" "$alice_stderr" "$bob_stderr" >/dev/null 2>&1 \
      || grep -F "$reverse_body" "$loft_log" "$alice_stderr" "$bob_stderr" >/dev/null 2>&1 \
      || grep -F "$outage_body" "$loft_log" "$alice_stderr" "$bob_stderr" >/dev/null 2>&1; then
    echo "acceptance: a message body appeared in process logs" >&2
    exit 1
  fi
  echo "acceptance: binary process logs contain no tested message body"

  PIGEONPOST_BIN="$binary" "$script_dir/mcp-stdio.sh"
  PIGEONPOST_BIN="$binary" "$script_dir/composed-services.sh"
}

run_source_gates() {
  require_command cargo
  export RUSTFLAGS=${RUSTFLAGS:--D warnings}
  cd "$repo_root"

  cargo test --locked -p pigeonpost-client --test delivery_reliability
  cargo test --locked -p pigeonpost-cli --test outbox_reporting
  cargo test --locked -p pigeonpost-client --test compliance_attribution
  cargo test --locked -p pigeonpost-loft --test offline_delivery \
    attribution_required_is_enforced_with_a_live_registry_key -- --exact
  cargo test --locked -p pigeonpost-cli --features test-utilities --test log_privacy
  "$script_dir/proxy-privacy.sh"
  "$script_dir/migration-rollback.sh"

  echo "acceptance: source-backed reliability, attribution, privacy, and rollback gates passed"
}

case "$mode" in
  all)
    run_binary_acceptance
    run_source_gates
    ;;
  binary) run_binary_acceptance ;;
  source) run_source_gates ;;
esac

echo "acceptance: all requested local scenarios passed"
