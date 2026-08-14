#!/usr/bin/env bash
set -Eeuo pipefail

umask 077

require_command() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "container acceptance: required command is unavailable: $1" >&2
    exit 1
  }
}

require_command docker
require_command node

# The release image is distroless, so readiness has to be probed from a separate container sharing
# its network namespace. Pinned by digest like every other image input here.
PROBE_IMAGE=${PIGEONPOST_PROBE_IMAGE:-docker.io/library/busybox@sha256:fc6dddc4c44b1bfe37f41cae8e67d1693828e8f42a91862816d7953e2c9d3f23}

: "${PIGEONPOST_IMAGE:?Set PIGEONPOST_IMAGE to one exact child-manifest digest}"
platform=${PIGEONPOST_PLATFORM:-linux/amd64}
mode=${1:---full}
case "$mode" in
  --full|--ready-only) ;;
  *)
    echo "container acceptance: usage: container-release.sh [--full|--ready-only]" >&2
    exit 1
    ;;
esac
case "$platform" in
  linux/amd64|linux/arm64) ;;
  *)
    echo "container acceptance: unsupported platform: $platform" >&2
    exit 1
    ;;
esac
if ! printf '%s\n' "$PIGEONPOST_IMAGE" \
    | grep -Eq '^ghcr\.io/bekirdag/pigeonpost@sha256:[0-9a-f]{64}$'; then
  echo "container acceptance: image must be the official exact GHCR digest" >&2
  exit 1
fi

run_root=$(mktemp -d "${TMPDIR:-/tmp}/pigeonpost-container-acceptance.XXXXXX")
suffix=$(basename -- "$run_root" | tr '.:' '--')
loft_name="pp-release-loft-$suffix"
loft_volume="pp-release-loft-data-$suffix"
sender_volume="pp-release-sender-$suffix"
recipient_volume="pp-release-recipient-$suffix"

cleanup() {
  local status=$?
  trap - EXIT
  docker rm -f "$loft_name" >/dev/null 2>&1 || true
  docker volume rm "$loft_volume" "$sender_volume" "$recipient_volume" \
    >/dev/null 2>&1 || true
  case "$(basename -- "$run_root")" in
    pigeonpost-container-acceptance.*) rm -R -- "$run_root" ;;
    *)
      echo "container acceptance: refusing to remove unexpected path: $run_root" >&2
      status=1
      ;;
  esac
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

docker volume create "$loft_volume" >/dev/null
docker volume create "$sender_volume" >/dev/null
docker volume create "$recipient_volume" >/dev/null

docker run --detach \
  --name "$loft_name" \
  --platform "$platform" \
  --user 10001:10001 \
  --read-only \
  --tmpfs /tmp:size=64m,mode=1777,noexec,nosuid,nodev \
  --security-opt no-new-privileges:true \
  --cap-drop ALL \
  --pids-limit 256 \
  --ulimit nofile=8192:8192 \
  --volume "$loft_volume:/var/lib/pigeonpost" \
  "$PIGEONPOST_IMAGE" \
  loft serve \
  --dir=/var/lib/pigeonpost \
  --bind=127.0.0.1:7717 \
  --capacity-gb=1 \
  --retention-days=1 >/dev/null

# Probed from a sidecar sharing the loft's network namespace, not by a docker healthcheck.
# `--health-cmd` always runs through /bin/sh inside the container and the release image is
# distroless: no shell, no curl, nothing to exec. The loft also refuses a non-loopback bind without
# pool.domain, so publishing a host port is not an option either — it must be probed from inside
# that namespace. This is the same `--network container:` trick the agent helper below already uses.
# Deliberately no `--platform`: the probe shares the loft's *network namespace*, not its
# architecture, so it runs natively and needs no emulation. Passing one is also impossible here —
# docker refuses `--platform` together with an `@sha256:` reference ("cannot overwrite digest").
readiness_probe() {
  docker run --rm \
    --network "container:$loft_name" \
    --read-only \
    --security-opt no-new-privileges:true \
    --cap-drop ALL \
    "$PROBE_IMAGE" \
    wget -q -O /dev/null -T 3 http://127.0.0.1:7717/ready
}

ready=false
for _ in {1..90}; do
  state=$(docker inspect --format '{{.State.Status}}' "$loft_name")
  case "$state" in
    exited*|dead*)
      echo "container acceptance: exact image exited before readiness: $state" >&2
      docker logs "$loft_name" >&2 || true
      exit 1
      ;;
  esac
  if probe_output=$(readiness_probe 2>&1); then
    ready=true
    break
  fi
  sleep 1
done
if [[ "$ready" != true ]]; then
  # Print why. Swallowing this cost a full release cycle to diagnose: the loft was up and logging
  # normally, and the only visible symptom was that the probe never succeeded.
  echo "container acceptance: exact image did not become healthy" >&2
  echo "container acceptance: last readiness probe said: ${probe_output:-<no output>}" >&2
  docker logs "$loft_name" >&2 || true
  exit 1
fi

# Asserted from the image and the running container rather than by executing inside it: a
# distroless image has no `id`, no `sh`, and nothing else to exec. What actually needs guarding is
# that the *image* declares an unprivileged user — the read-only rootfs and tmpfs are flags this
# script passes itself, and docker enforces them whether or not we can observe them from inside.
test "$(docker inspect --format '{{.Config.User}}' "$PIGEONPOST_IMAGE")" = '10001:10001'
test "$(docker inspect --format '{{.HostConfig.ReadonlyRootfs}}' "$loft_name")" = 'true'
test "$(docker inspect --format '{{.Config.Entrypoint}}' "$PIGEONPOST_IMAGE")" \
  = '[/usr/bin/tini -- /usr/local/bin/pigeonpost]'

if [[ "$mode" == --ready-only ]]; then
  echo "container acceptance: $platform exact image reached /ready under production constraints"
  exit 0
fi

agent() {
  local volume=$1
  shift
  docker run --rm \
    --platform "$platform" \
    --network "container:$loft_name" \
    --user 10001:10001 \
    --read-only \
    --tmpfs /tmp:size=64m,mode=1777,noexec,nosuid,nodev \
    --security-opt no-new-privileges:true \
    --cap-drop ALL \
    --pids-limit 128 \
    --env PIGEONPOST_HOME=/var/lib/pigeonpost/agent \
    --volume "$volume:/var/lib/pigeonpost" \
    "$PIGEONPOST_IMAGE" "$@"
}

json_field() {
  local field=$1
  # The single-quoted program is intentional: JavaScript, not the shell, owns `${...}`.
  # shellcheck disable=SC2016
  node -e '
    let input = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => { input += chunk; });
    process.stdin.on("end", () => {
      let value = JSON.parse(input);
      for (const part of process.argv[1].split(".")) value = value[part];
      if (typeof value !== "string" && typeof value !== "number") {
        throw new Error(`field ${process.argv[1]} is not a string or number`);
      }
      process.stdout.write(String(value));
    });
  ' "$field"
}

field_from() {
  local value=$1
  local field=$2
  printf '%s' "$value" | json_field "$field"
}

sender_identity=$(agent "$sender_volume" --json id)
recipient_identity=$(agent "$recipient_volume" --json id)
sender_address=$(field_from "$sender_identity" address)
recipient_address=$(field_from "$recipient_identity" address)
endpoint=http://127.0.0.1:7717

agent "$sender_volume" loft add "$endpoint" >/dev/null
agent "$recipient_volume" loft add "$endpoint" >/dev/null
agent "$recipient_volume" allow "$sender_address" >/dev/null

body='release-container-round-trip-untrusted'
send_result=$(agent "$sender_volume" --json send "$recipient_address" --body "$body")
# The single-quoted program is intentional: JavaScript, not the shell, owns `${...}`.
# shellcheck disable=SC2016
SEND_RESULT="$send_result" node -e '
  const result = JSON.parse(process.env.SEND_RESULT);
  if (result.delivered !== 1 || result.queued !== 0 || result.terminal !== 0
      || typeof result.id !== "string" || result.id.length === 0) {
    throw new Error(`unexpected send result: ${process.env.SEND_RESULT}`);
  }
'
message_id=$(field_from "$send_result" id)
inbox=$(agent "$recipient_volume" --json inbox --limit 10)
# The single-quoted program is intentional: JavaScript, not the shell, owns `${...}`.
# shellcheck disable=SC2016
INBOX="$inbox" MESSAGE_ID="$message_id" node -e '
  const inbox = JSON.parse(process.env.INBOX);
  if (!Array.isArray(inbox) || !inbox.some((item) => item.id === process.env.MESSAGE_ID)) {
    throw new Error(`inbox does not contain ${process.env.MESSAGE_ID}`);
  }
'
read_result=$(agent "$recipient_volume" --json read "$message_id")
# The single-quoted program is intentional: JavaScript, not the shell, owns `${...}`.
# shellcheck disable=SC2016
READ_RESULT="$read_result" EXPECTED_BODY="$body" node -e '
  const message = JSON.parse(process.env.READ_RESULT);
  const open = message.fence?.open;
  const close = message.fence?.close;
  if (message.body_format !== "pigeonpost_fenced_untrusted_text_v1"
      || typeof message.untrusted_body !== "string"
      || typeof open !== "string"
      || typeof close !== "string"
      || process.env.EXPECTED_BODY.includes(open)
      || process.env.EXPECTED_BODY.includes(close)
      || message.untrusted_body !== `${open}\n${process.env.EXPECTED_BODY}\n${close}`) {
    throw new Error("read result did not preserve the untrusted-body fence");
  }
'

docker logs "$loft_name" >"$run_root/loft.log" 2>&1
if grep -Fq "$body" "$run_root/loft.log"; then
  echo "container acceptance: message body appeared in service logs" >&2
  exit 1
fi

echo "container acceptance: exact $platform image passed non-root/read-only delivery"
