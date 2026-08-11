#!/usr/bin/env bash
set -Eeuo pipefail

umask 077

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd -P)
caddy_image='caddy@sha256:4c6e91c6ed0e2fa03efd5b44747b625fec79bc9cd06ac5235a779726618e530d'
run_root=''
container_name=''

cleanup() {
  status=$?
  trap - EXIT

  if [[ -n "$container_name" ]]; then
    case "$container_name" in
      pigeonpost-proxy-privacy-*) docker rm --force -- "$container_name" >/dev/null 2>&1 || true ;;
      *)
        echo "proxy-privacy: refusing to remove unexpected container: $container_name" >&2
        status=1
        ;;
    esac
  fi

  if [[ -n "$run_root" && -d "$run_root" ]]; then
    case "$(basename -- "$run_root")" in
      pigeonpost-proxy-privacy.*) rm -R -- "$run_root" ;;
      *)
        echo "proxy-privacy: refusing to remove unexpected path: $run_root" >&2
        status=1
        ;;
    esac
  fi
  exit "$status"
}

trap cleanup EXIT
trap 'exit 130' INT TERM HUP

for command_name in curl docker node; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "proxy-privacy: required command is unavailable: $command_name" >&2
    exit 1
  }
done

binary=${PIGEONPOST_BIN:-"$repo_root/target/debug/pigeonpost"}
if [[ -z "${PIGEONPOST_BIN:-}" ]]; then
  command -v cargo >/dev/null 2>&1 || {
    echo "proxy-privacy: required command is unavailable: cargo" >&2
    exit 1
  }
  (cd "$repo_root" && cargo build --locked -p pigeonpost-cli)
fi
if [[ ! -f "$binary" || ! -x "$binary" ]]; then
  echo "proxy-privacy: PIGEONPOST_BIN must name an executable regular file: $binary" >&2
  exit 1
fi

docker info >/dev/null
docker image inspect "$caddy_image" >/dev/null 2>&1 || docker pull "$caddy_image" >/dev/null

run_root=$(mktemp -d "${TMPDIR:-/tmp}/pigeonpost-proxy-privacy.XXXXXX")
install_root="$run_root/install"
adapted_config="$run_root/adapted.json"
adapt_stderr="$run_root/adapt.stderr"
local_adapted_config="$run_root/local-adapted.json"
local_adapt_stderr="$run_root/local-adapt.stderr"
local_compose_config="$run_root/local-compose.json"
proxy_log="$run_root/proxy.log"
runtime_caddyfile="$run_root/runtime.Caddyfile"
proxy_domain='loft.example'
selector_canary="proxy-raw-selector-canary-$(basename -- "$run_root")"
container_name="pigeonpost-proxy-privacy-$(basename -- "$run_root" | tr -cd 'A-Za-z0-9_.-')"

"$binary" install \
  --dir "$install_root" \
  --domain "$proxy_domain" \
  --bind 127.0.0.1:7717 \
  --capacity-gb 1 \
  --retention-days 1 \
  --no-service >/dev/null

caddyfile="$install_root/loft.Caddyfile"
if [[ ! -f "$caddyfile" ]]; then
  echo "proxy-privacy: installer did not generate loft.Caddyfile" >&2
  exit 1
fi

docker run --rm \
  --mount "type=bind,src=$caddyfile,dst=/etc/caddy/Caddyfile,readonly" \
  "$caddy_image" \
  caddy adapt --config /etc/caddy/Caddyfile --adapter caddyfile --pretty \
  >"$adapted_config" 2>"$adapt_stderr"

node - "$adapted_config" <<'NODE'
const fs = require("node:fs");
const config = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));

if (config.admin?.disabled !== true) {
  throw new Error("generated Caddy config must disable the admin endpoint");
}

const configuredLogs = config.logging?.logs ?? {};
if (Object.keys(configuredLogs).length !== 1
    || configuredLogs.default?.writer?.output !== "discard") {
  throw new Error("generated Caddy config must discard only the global default runtime log");
}

const servers = Object.values(config.apps?.http?.servers ?? {});
if (servers.length === 0) throw new Error("generated Caddy config has no HTTP server");
for (const server of servers) {
  if (server.logs && Object.keys(server.logs).length !== 0) {
    throw new Error("generated Caddy config must not enable HTTP access logging");
  }
}
NODE
echo "proxy-privacy: generated Caddy config disables admin, access, and runtime logging"

# The generated file intentionally uses public ACME. This isolated acceptance fixture has no DNS,
# so preserve the exact proxy/logging directives and add only a local issuer for the live requests.
awk 'NR > 1 { print previous } { previous = $0 } END { print "\ttls internal"; print previous }' \
  "$caddyfile" >"$runtime_caddyfile"

local_caddyfile="$repo_root/deploy/Caddyfile.local"
if [[ ! -f "$local_caddyfile" ]]; then
  echo "proxy-privacy: local Compose Caddyfile is missing: $local_caddyfile" >&2
  exit 1
fi

PIGEONPOST_PORT=7717 \
PIGEONPOST_CAPACITY_GB=20 \
PIGEONPOST_RETENTION_DAYS=30 \
  docker compose -f "$repo_root/deploy/compose.loft.yml" config --format json \
    >"$local_compose_config"

LOCAL_CADDYFILE="$local_caddyfile" CADDY_IMAGE="$caddy_image" \
  node - "$local_compose_config" <<'NODE'
const fs = require("node:fs");
const config = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
const edge = config.services?.edge;
const loft = config.services?.loft;

if (!edge || !loft) throw new Error("local Compose config must define loft and edge services");
if (edge.image !== process.env.CADDY_IMAGE) {
  throw new Error("local Compose edge must use the Caddy digest exercised by this gate");
}
if (edge.network_mode !== "service:loft") {
  throw new Error("local Compose edge must share only the loft network namespace");
}
if (JSON.stringify(edge.command) !== JSON.stringify([
  "caddy", "run", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile",
])) {
  throw new Error("local Compose edge must run the adapted Caddyfile directly");
}
const caddyMounts = (edge.volumes ?? []).filter(
  (mount) => mount.target === "/etc/caddy/Caddyfile",
);
if (caddyMounts.length !== 1
    || caddyMounts[0].type !== "bind"
    || caddyMounts[0].source !== process.env.LOCAL_CADDYFILE
    || caddyMounts[0].read_only !== true) {
  throw new Error("local Compose edge must read only the exact checked-in Caddyfile");
}
if (!(loft.command ?? []).includes("--bind=127.0.0.1:7717")) {
  throw new Error("local Compose loft must bind only inside its loopback namespace");
}
const published = loft.ports ?? [];
if (published.length !== 1
    || published[0].host_ip !== "127.0.0.1"
    || published[0].target !== 17717
    || published[0].protocol !== "tcp") {
  throw new Error("local Compose must expose only the edge port on host loopback");
}
NODE

docker run --rm \
  --mount "type=bind,src=$local_caddyfile,dst=/etc/caddy/Caddyfile,readonly" \
  "$caddy_image" \
  caddy adapt --config /etc/caddy/Caddyfile --adapter caddyfile --pretty \
  >"$local_adapted_config" 2>"$local_adapt_stderr"

node - "$local_adapted_config" <<'NODE'
const fs = require("node:fs");
const config = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));

if (config.admin?.disabled !== true) {
  throw new Error("local Caddy config must disable the admin endpoint");
}

const configuredLogs = config.logging?.logs ?? {};
if (Object.keys(configuredLogs).length !== 1
    || configuredLogs.default?.writer?.output !== "discard") {
  throw new Error("local Caddy config must discard only the global default runtime log");
}

const servers = Object.values(config.apps?.http?.servers ?? {});
if (servers.length !== 1) {
  throw new Error("local Caddy config must define exactly one HTTP server");
}
const [server] = servers;
if (server.logs && Object.keys(server.logs).length !== 0) {
  throw new Error("local Caddy config must not enable HTTP access logging");
}
if (server.automatic_https?.disable !== true) {
  throw new Error("local Caddy config must disable automatic HTTPS");
}
if (JSON.stringify(server.listen) !== JSON.stringify([":17717"])) {
  throw new Error("local Caddy config must listen only on the shared namespace port :17717");
}

const routes = server.routes ?? [];
const handlers = routes.flatMap((route) => route.handle ?? []);
if (routes.length !== 1
    || handlers.length !== 1
    || handlers[0].handler !== "reverse_proxy"
    || JSON.stringify(handlers[0].upstreams) !== JSON.stringify([{dial: "127.0.0.1:7717"}])) {
  throw new Error("local Caddy config must proxy only to the loopback loft on 127.0.0.1:7717");
}
NODE
echo "proxy-privacy: exact local Compose edge and Caddy config are private and loopback-scoped"

docker run --detach \
  --name "$container_name" \
  --publish 127.0.0.1::80 \
  --publish 127.0.0.1::443 \
  --mount "type=bind,src=$runtime_caddyfile,dst=/etc/caddy/Caddyfile,readonly" \
  "$caddy_image" \
  /bin/sh -c '
    caddy file-server --listen 127.0.0.1:7717 --root /usr/share/caddy \
      >/tmp/pigeonpost-test-backend.log 2>&1 &
    backend_pid=$!
    echo "$backend_pid" >/tmp/pigeonpost-test-backend.pid
    exec caddy run --config /etc/caddy/Caddyfile --adapter caddyfile
  ' >/dev/null

https_port=$(docker port "$container_name" 443/tcp | sed -n '1{s/.*://;p;}')
http_port=$(docker port "$container_name" 80/tcp | sed -n '1{s/.*://;p;}')
case "$https_port:$http_port" in
  *[!0-9:]*|:*|*:) echo "proxy-privacy: Docker did not publish both proxy ports" >&2; exit 1 ;;
esac

ready=0
for _ in {1..100}; do
  if ! docker container inspect "$container_name" >/dev/null 2>&1; then
    echo "proxy-privacy: Caddy container disappeared before readiness" >&2
    exit 1
  fi
  if curl --insecure --silent --show-error --fail --max-time 1 --noproxy '*' \
      --resolve "$proxy_domain:$https_port:127.0.0.1" \
      "https://$proxy_domain:$https_port/?selector=$selector_canary" >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 0.05
done
if [[ "$ready" != 1 ]]; then
  echo "proxy-privacy: generated Caddy proxy did not become ready" >&2
  docker logs "$container_name" >&2 || true
  exit 1
fi
echo "proxy-privacy: successful reverse-proxy request passed"

HTTP_PORT="$http_port" SELECTOR_CANARY="$selector_canary" node <<'NODE'
const net = require("node:net");
const socket = net.createConnection({host: "127.0.0.1", port: Number(process.env.HTTP_PORT)});
const deadline = setTimeout(() => socket.destroy(new Error("malformed request timed out")), 2000);
socket.on("connect", () => {
  socket.end(`GET /broken?selector=${process.env.SELECTOR_CANARY} HTTP/1.1\r\nBroken Header\r\n\r\n`);
});
socket.on("data", () => {});
socket.on("close", () => clearTimeout(deadline));
socket.on("error", (error) => {
  clearTimeout(deadline);
  throw error;
});
NODE
echo "proxy-privacy: malformed-request path passed"

docker exec "$container_name" /bin/sh -c \
  'kill "$(cat /tmp/pigeonpost-test-backend.pid)"' >/dev/null
if curl --insecure --silent --show-error --max-time 3 --noproxy '*' \
    --resolve "$proxy_domain:$https_port:127.0.0.1" \
    "https://$proxy_domain:$https_port/unavailable?selector=$selector_canary" \
    --output /dev/null --write-out '%{http_code}' | grep -qx '502'; then
  echo "proxy-privacy: failed-upstream path returned 502"
else
  echo "proxy-privacy: failed-upstream path did not return 502" >&2
  exit 1
fi

docker stop --time 5 "$container_name" >/dev/null
docker logs "$container_name" >"$proxy_log" 2>&1

PROXY_LOG="$proxy_log" SELECTOR_CANARY="$selector_canary" node <<'NODE'
const fs = require("node:fs");
const net = require("node:net");
const log = fs.readFileSync(process.env.PROXY_LOG, "utf8");

if (log.includes(process.env.SELECTOR_CANARY)) {
  throw new Error("raw request selector appeared in Caddy stdout/stderr");
}

const candidates = [];
for (const match of log.matchAll(/(?:^|[^0-9])((?:[0-9]{1,3}\.){3}[0-9]{1,3})(?![0-9])/g)) {
  candidates.push(match[1]);
}
for (const match of log.matchAll(/\[([0-9A-Fa-f:]+)\]/g)) candidates.push(match[1]);
for (const match of log.matchAll(/"((?:[0-9A-Fa-f]{0,4}:){2,}[0-9A-Fa-f:]*)"/g)) {
  candidates.push(match[1]);
}
const addresses = [...new Set(candidates.filter((candidate) => net.isIP(candidate) !== 0))];
if (addresses.length !== 0) {
  throw new Error(`IP address appeared in Caddy stdout/stderr: ${addresses.join(", ")}`);
}
NODE

echo "proxy-privacy: pinned Caddy emitted no IP address or raw selector"
