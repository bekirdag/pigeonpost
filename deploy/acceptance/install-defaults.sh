#!/bin/sh
set -eu

umask 077

fail() {
  echo "install-defaults: $*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command is unavailable: $1"
}

program_name=${0##*/}

if [ "$program_name" = systemctl ]; then
  : "${HOME:?fake systemctl requires HOME}"
  : "${PP_INSTALL_CALLS:?fake systemctl requires PP_INSTALL_CALLS}"
  : "${PP_INSTALL_DIR:?fake systemctl requires PP_INSTALL_DIR}"
  : "${PP_INSTALL_LOG:?fake systemctl requires PP_INSTALL_LOG}"
  : "${PP_INSTALL_PID:?fake systemctl requires PP_INSTALL_PID}"
  : "${PP_INSTALL_PROGRAM:?fake systemctl requires PP_INSTALL_PROGRAM}"
  : "${PP_INSTALL_CMDLINE:?fake systemctl requires PP_INSTALL_CMDLINE}"
  : "${PP_INSTALL_SYSTEMD_LOG:?fake systemctl requires PP_INSTALL_SYSTEMD_LOG}"

  case "$*" in
    '--user daemon-reload'|'--user enable pigeonpost-loft.service')
      printf '%s\n' "$*" >>"$PP_INSTALL_CALLS"
      exit 0
      ;;
    '--user restart pigeonpost-loft.service')
      unit="$HOME/.config/systemd/user/pigeonpost-loft.service"
      [ -f "$unit" ] && [ ! -L "$unit" ] || fail "restart did not receive a regular service unit"
      case "$PP_INSTALL_PROGRAM$PP_INSTALL_DIR" in
        *\"*|*\\*) fail "fixture paths cannot be represented by the exact unit assertion" ;;
      esac
      expected="ExecStart=\"$PP_INSTALL_PROGRAM\" \"loft\" \"serve\" \"--dir\" \"$PP_INSTALL_DIR\""
      actual=$(sed -n '/^ExecStart=/p' "$unit")
      [ "$actual" = "$expected" ] || fail "generated ExecStart does not match the installed binary and canonical directory"
      systemd-analyze verify "$unit" >"$PP_INSTALL_SYSTEMD_LOG" 2>&1 \
        || fail "systemd rejected the generated service unit"

      printf '%s\n' "$*" >>"$PP_INSTALL_CALLS"
      printf '%s\0' \
        "$PP_INSTALL_PROGRAM" loft serve --dir "$PP_INSTALL_DIR" >"$PP_INSTALL_CMDLINE"
      nohup "$PP_INSTALL_PROGRAM" loft serve --dir "$PP_INSTALL_DIR" \
        >"$PP_INSTALL_LOG" 2>&1 &
      child=$!
      if ! printf '%s\n' "$child" >"$PP_INSTALL_PID"; then
        kill -TERM "$child" 2>/dev/null || true
        wait "$child" 2>/dev/null || true
        fail "could not record the managed-service PID"
      fi
      exit 0
      ;;
    *) fail "unexpected service-manager invocation: $*" ;;
  esac
fi

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
script_path="$script_dir/${0##*/}"

direct_root=''
direct_program=''
direct_pid_file=''
direct_cmdline_file=''
direct_actual_cmdline_file=''

service_is_ours() {
  pid=$1
  [ -r "/proc/$pid/exe" ] || return 1
  [ "$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)" = "$direct_program" ] || return 1
  [ -n "$direct_cmdline_file" ] && [ -n "$direct_actual_cmdline_file" ] || return 1
  cat "/proc/$pid/cmdline" >"$direct_actual_cmdline_file" 2>/dev/null || return 1
  if ! cmp -s "$direct_cmdline_file" "$direct_actual_cmdline_file"; then
    if [ "${PIGEONPOST_ACCEPTANCE_DEBUG:-0}" = 1 ]; then
      echo "install-defaults: expected managed argv:" >&2
      tr '\0' '\n' <"$direct_cmdline_file" >&2
      echo "install-defaults: actual managed argv:" >&2
      tr '\0' '\n' <"$direct_actual_cmdline_file" >&2
    fi
    return 1
  fi
  return 0
}

stop_direct_service() {
  [ -n "$direct_pid_file" ] && [ -f "$direct_pid_file" ] || return 0
  pid=$(sed -n '1p' "$direct_pid_file")
  case "$pid" in
    ''|*[!0-9]*) echo "install-defaults: invalid managed-service PID" >&2; return 1 ;;
  esac
  kill -0 "$pid" 2>/dev/null || return 0
  service_is_ours "$pid" || {
    echo "install-defaults: refusing to signal a process outside the isolated fixture" >&2
    return 1
  }

  kill -TERM "$pid" 2>/dev/null || true
  attempts=0
  while kill -0 "$pid" 2>/dev/null && [ "$attempts" -lt 50 ]; do
    state=$(sed -n 's/^State:[[:space:]]*\([^[:space:]]*\).*/\1/p' "/proc/$pid/status" 2>/dev/null || true)
    [ "$state" = Z ] && return 0
    sleep 0.1
    attempts=$((attempts + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    service_is_ours "$pid" || {
      echo "install-defaults: managed-service identity changed during shutdown" >&2
      return 1
    }
    kill -KILL "$pid" 2>/dev/null || true
  fi
}

cleanup_direct() {
  status=$?
  trap - 0 HUP INT TERM
  if ! stop_direct_service; then
    status=1
  fi
  if [ -n "$direct_root" ] && [ -d "$direct_root" ]; then
    case "${direct_root##*/}" in
      pigeonpost-install-defaults.*) rm -R -- "$direct_root" ;;
      *)
        echo "install-defaults: refusing to remove unexpected path: $direct_root" >&2
        status=1
        ;;
    esac
  fi
  exit "$status"
}

assert_mode_600() {
  file=$1
  [ -f "$file" ] && [ ! -L "$file" ] || fail "expected a regular file: $file"
  [ "$(stat -c '%a' "$file")" = 600 ] || fail "expected mode 0600: $file"
}

run_direct() {
  [ "$(uname -s)" = Linux ] || fail "direct zero-option installation requires Linux; use --container elsewhere"
  : "${PIGEONPOST_BIN:?Set PIGEONPOST_BIN to the exact Linux binary}"
  require_command cmp
  require_command curl
  require_command env
  require_command mktemp
  require_command readlink
  require_command sed
  require_command sqlite3
  require_command stat
  require_command systemd-analyze

  direct_program=$(readlink -f "$PIGEONPOST_BIN")
  [ -f "$direct_program" ] && [ -x "$direct_program" ] || fail "PIGEONPOST_BIN is not an executable regular file"

  require_command awk
  require_command df
  direct_root=$(mktemp -d "${TMPDIR:-/tmp}/pigeonpost-install-defaults.XXXXXX")
  direct_pid_file="$direct_root/service.pid"
  direct_cmdline_file="$direct_root/service.cmdline"
  direct_actual_cmdline_file="$direct_root/service.actual.cmdline"
  trap cleanup_direct 0
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM

  home="$direct_root/home"
  loft="$direct_root/loft"
  fixture_bin="$direct_root/bin"
  calls="$direct_root/systemctl.calls"
  expected_calls="$direct_root/systemctl.expected"
  service_log="$direct_root/service.log"
  systemd_log="$direct_root/systemd-analyze.log"
  install_stdout="$direct_root/install.stdout"
  install_stderr="$direct_root/install.stderr"
  mkdir -m 700 "$home" "$loft" "$fixture_bin"
  : >"$calls"
  ln -s "$script_path" "$fixture_bin/systemctl"

  available_kib=$(df -Pk "$loft" | awk 'NR == 2 { print $4; exit }')
  case "$available_kib" in
    ''|*[!0-9]*) fail "could not read available filesystem capacity" ;;
  esac
  free_gib=$((available_kib / 1048576))
  expected_capacity=$((free_gib / 5))
  [ "$expected_capacity" -ge 1 ] || expected_capacity=1
  [ "$expected_capacity" -le 20 ] || expected_capacity=20

  if curl --proto '=http' --noproxy '*' --max-time 1 --fail --silent \
      http://127.0.0.1:7717/ready >/dev/null 2>&1; then
    fail "default port 7717 is already serving a readiness response"
  fi

  if ! (
    umask 022
    cd "$loft"
    env -i \
      HOME="$home" \
      PATH="$fixture_bin:$PATH" \
      PIGEONPOST_LOG=warn \
      PP_INSTALL_CALLS="$calls" \
      PP_INSTALL_DIR="$loft" \
      PP_INSTALL_LOG="$service_log" \
      PP_INSTALL_PID="$direct_pid_file" \
      PP_INSTALL_PROGRAM="$direct_program" \
      PP_INSTALL_CMDLINE="$direct_cmdline_file" \
      PP_INSTALL_SYSTEMD_LOG="$systemd_log" \
      "$direct_program" install >"$install_stdout" 2>"$install_stderr"
  ); then
    if [ "${PIGEONPOST_ACCEPTANCE_DEBUG:-0}" = 1 ]; then
      echo "install-defaults: installer stdout:" >&2
      sed -n '1,160p' "$install_stdout" >&2
      echo "install-defaults: installer stderr:" >&2
      sed -n '1,160p' "$install_stderr" >&2
      if [ -s "$systemd_log" ]; then
        echo "install-defaults: systemd verification output:" >&2
        sed -n '1,160p' "$systemd_log" >&2
      fi
    fi
    fail "zero-option installer exited unsuccessfully"
  fi

  printf '%s\n' \
    '--user daemon-reload' \
    '--user enable pigeonpost-loft.service' \
    '--user restart pigeonpost-loft.service' >"$expected_calls"
  cmp -s "$expected_calls" "$calls" || fail "service activation calls were missing, reordered, or duplicated"

  key="$loft/loft.key"
  config="$loft/loft.toml"
  unit="$home/.config/systemd/user/pigeonpost-loft.service"
  assert_mode_600 "$key"
  assert_mode_600 "$config"
  assert_mode_600 "$unit"

  grep -Fqx 'bind = "127.0.0.1:7717"' "$config" || fail "default bind is not loopback port 7717"
  grep -Fqx "storage_path = \"$loft/data/loft.db\"" "$config" || fail "default storage path is not canonical"
  grep -Fqx 'retention_days = 30' "$config" || fail "default retention is not 30 days"
  grep -Fqx 'join = false' "$config" || fail "zero-option install unexpectedly joins the public pool"
  if grep -Eq '^domain[[:space:]]*=' "$config"; then
    fail "zero-option install unexpectedly records a public domain"
  fi

  capacity=$(sed -n 's/^capacity_gb = \([0-9][0-9]*\)$/\1/p' "$config")
  case "$capacity" in
    ''|*[!0-9]*) fail "default capacity is missing or malformed" ;;
  esac
  [ "$capacity" -eq "$expected_capacity" ] \
    || fail "default capacity does not match the clamped 20%-of-free-space formula"

  pid=$(sed -n '1p' "$direct_pid_file")
  case "$pid" in
    ''|*[!0-9]*) fail "service manager did not record a valid PID" ;;
  esac
  kill -0 "$pid" 2>/dev/null || fail "installed loft exited before acceptance checks"
  service_is_ours "$pid" || fail "installed service is not the exact tested binary"
  curl --proto '=http' --noproxy '*' --max-time 2 --fail --silent --show-error \
    http://127.0.0.1:7717/ready >/dev/null
  database="$loft/data/loft.db"
  assert_mode_600 "$database"
  [ "$(sqlite3 "$database" 'PRAGMA integrity_check;')" = ok ] \
    || fail "installed SQLite storage failed its integrity check"
  [ "$(sqlite3 "$database" 'PRAGMA user_version;')" = 6 ] \
    || fail "installed SQLite storage is not at schema version 6"
  for table in events recipient_policy agent_records storage_stats trace_segments rotation_records trace_admission; do
    [ "$(sqlite3 "$database" "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name='$table';")" = 1 ] \
      || fail "installed SQLite storage is missing required schema table: $table"
  done
  for sidecar in "$database-wal" "$database-shm"; do
    [ ! -e "$sidecar" ] || assert_mode_600 "$sidecar"
  done

  echo "install-defaults: exact zero-option Linux install, systemd parse, activation contract, schema, and readiness passed"
}

container_root=''
container_name=''
container_id=''
container_extract_id=''
container_volume=''
container_owns_volume=0
container_product_image=''
container_owns_product_image=0
container_acceptance_image=''
container_owns_acceptance_image=0
container_label_key='org.pigeonpost.acceptance.install-defaults'
container_token=''

container_label() {
  docker container inspect --format "{{ index .Config.Labels \"$container_label_key\" }}" "$1" 2>/dev/null
}

volume_label() {
  docker volume inspect --format "{{ index .Labels \"$container_label_key\" }}" "$1" 2>/dev/null
}

image_label() {
  docker image inspect --format "{{ index .Config.Labels \"$container_label_key\" }}" "$1" 2>/dev/null
}

cleanup_container() {
  status=$?
  trap - 0 HUP INT TERM
  if [ -n "$container_id" ] && docker container inspect "$container_id" >/dev/null 2>&1; then
    if [ "$(container_label "$container_id" || true)" = "$container_token-container" ]; then
      docker rm -f "$container_id" >/dev/null 2>&1 || status=1
    else
      echo "install-defaults: refusing to remove a container without this run's ownership label" >&2
      status=1
    fi
  fi
  if [ -n "$container_extract_id" ] \
      && docker container inspect "$container_extract_id" >/dev/null 2>&1; then
    if [ "$(container_label "$container_extract_id" || true)" = "$container_token-extract" ]; then
      docker rm -f "$container_extract_id" >/dev/null 2>&1 || status=1
    else
      echo "install-defaults: refusing to remove an extraction container without this run's ownership label" >&2
      status=1
    fi
  fi
  if [ "$container_owns_volume" -eq 1 ] && [ -n "$container_volume" ]; then
    if [ "$(volume_label "$container_volume" || true)" = "$container_token-volume" ]; then
      docker volume rm "$container_volume" >/dev/null 2>&1 || status=1
    else
      echo "install-defaults: refusing to remove a volume without this run's ownership label" >&2
      status=1
    fi
  fi
  if [ "$container_owns_acceptance_image" -eq 1 ] && [ -n "$container_acceptance_image" ]; then
    if [ "$(image_label "$container_acceptance_image" || true)" = "$container_token-acceptance" ]; then
      docker image rm "$container_acceptance_image" >/dev/null 2>&1 || status=1
    else
      echo "install-defaults: refusing to remove an acceptance image without this run's ownership label" >&2
      status=1
    fi
  fi
  if [ "$container_owns_product_image" -eq 1 ] && [ -n "$container_product_image" ]; then
    if [ "$(image_label "$container_product_image" || true)" = "$container_token-product" ]; then
      docker image rm "$container_product_image" >/dev/null 2>&1 || status=1
    else
      echo "install-defaults: refusing to remove a product image without this run's ownership label" >&2
      status=1
    fi
  fi
  if [ -n "$container_root" ] && [ -d "$container_root" ]; then
    case "${container_root##*/}" in
      pigeonpost-install-container.*) rm -R -- "$container_root" ;;
      *)
        echo "install-defaults: refusing to remove unexpected path: $container_root" >&2
        status=1
        ;;
    esac
  fi
  exit "$status"
}

run_container() {
  require_command docker
  require_command mktemp
  repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd -P)
  container_root=$(mktemp -d "${TMPDIR:-/tmp}/pigeonpost-install-container.XXXXXX")
  suffix=$(printf '%s' "${container_root##*/}" | tr '.:' '--')
  container_token=$suffix
  container_name=${PIGEONPOST_ACCEPTANCE_CONTAINER_NAME:-pp-install-defaults-$suffix}
  case "$container_name" in
    pp-install-defaults-*) ;;
    *) fail "acceptance container name must use the isolated fixture prefix" ;;
  esac
  trap cleanup_container 0
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM

  if [ -n "${PIGEONPOST_INSTALL_IMAGE:-}" ]; then
    container_product_image=$PIGEONPOST_INSTALL_IMAGE
    docker image inspect "$container_product_image" >/dev/null 2>&1 \
      || docker pull "$container_product_image" >/dev/null
  else
    product_iid="$container_root/product.iid"
    docker build \
      --label "$container_label_key=$container_token-product" \
      --iidfile "$product_iid" \
      --file "$repo_root/deploy/Dockerfile" \
      "$repo_root"
    container_product_image=$(sed -n '1p' "$product_iid")
    [ -n "$container_product_image" ] \
      && [ "$(image_label "$container_product_image" || true)" = "$container_token-product" ] \
      || fail "built product image is missing this run's ownership label"
    container_owns_product_image=1
  fi

  container_extract_id=$(docker create \
    --label "$container_label_key=$container_token-extract" \
    "$container_product_image")
  [ -n "$container_extract_id" ] \
    && [ "$(container_label "$container_extract_id" || true)" = "$container_token-extract" ] \
    || fail "created extraction container is missing this run's ownership label"
  docker cp "$container_extract_id:/usr/local/bin/pigeonpost" "$container_root/pigeonpost"
  docker rm "$container_extract_id" >/dev/null
  container_extract_id=''
  [ -f "$container_root/pigeonpost" ] && [ -x "$container_root/pigeonpost" ] \
    || fail "could not extract the exact product binary"

  acceptance_iid="$container_root/acceptance.iid"
  docker build \
    --label "$container_label_key=$container_token-acceptance" \
    --iidfile "$acceptance_iid" \
    --file "$repo_root/deploy/acceptance/Dockerfile.install-defaults" \
    "$container_root"
  container_acceptance_image=$(sed -n '1p' "$acceptance_iid")
  [ -n "$container_acceptance_image" ] \
    && [ "$(image_label "$container_acceptance_image" || true)" = "$container_token-acceptance" ] \
    || fail "built acceptance image is missing this run's ownership label"
  container_owns_acceptance_image=1

  product_hash=$(docker run --rm --entrypoint sha256sum "$container_product_image" /usr/local/bin/pigeonpost)
  acceptance_hash=$(docker run --rm --entrypoint sha256sum "$container_acceptance_image" /usr/local/bin/pigeonpost)
  [ "$product_hash" = "$acceptance_hash" ] \
    || fail "acceptance tooling image changed the exact product binary"

  container_volume=$(docker volume create \
    --label "$container_label_key=$container_token-volume")
  [ -n "$container_volume" ] \
    && [ "$(volume_label "$container_volume" || true)" = "$container_token-volume" ] \
    || fail "created volume is missing this run's ownership label"
  container_owns_volume=1
  docker run --rm \
    --entrypoint /bin/sh \
    --user 0:0 \
    --volume "$container_volume:/acceptance" \
    "$container_acceptance_image" \
    -c 'mkdir -p /acceptance/tmp && chown -R 10001:10001 /acceptance'

  inside_shell_flag=-e
  if [ "${PIGEONPOST_ACCEPTANCE_DEBUG:-0}" = 1 ]; then
    inside_shell_flag=-x
  fi
  container_id=$(docker create \
    --name "$container_name" \
    --label "$container_label_key=$container_token-container" \
    --entrypoint /bin/sh \
    --user 10001:10001 \
    --env PIGEONPOST_BIN=/usr/local/bin/pigeonpost \
    --env "PIGEONPOST_ACCEPTANCE_DEBUG=${PIGEONPOST_ACCEPTANCE_DEBUG:-0}" \
    --env TMPDIR=/acceptance/tmp \
    --volume "$container_volume:/acceptance" \
    --volume "$script_path:/harness/install-defaults.sh:ro" \
    --workdir /acceptance \
    "$container_acceptance_image" \
    "$inside_shell_flag" /harness/install-defaults.sh --inside
  )
  [ -n "$container_id" ] \
    && [ "$(container_label "$container_id" || true)" = "$container_token-container" ] \
    || fail "created container is missing this run's ownership label"
  if ! docker start --attach "$container_id"; then
    docker logs "$container_id" >&2 || true
    docker container inspect --format 'install-defaults: state={{.State.Status}} exit={{.State.ExitCode}} error={{.State.Error}}' \
      "$container_id" >&2 || true
    fail "isolated installer acceptance container failed"
  fi
}

case "${1:-}" in
  '') run_direct ;;
  --inside) run_direct ;;
  --container) run_container ;;
  *) fail "usage: install-defaults.sh [--container]" ;;
esac
