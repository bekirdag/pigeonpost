#!/bin/sh
# Host-level egress limit for the postbox containers (Phase 9 hardening).
#
# The webhook worker already refuses private destinations, redirects and ports other than
# 443/8443 in the application (crates/pigeonpost-postbox/src/webhooks.rs). This is the second
# layer: whatever the application does, connections the postbox and reaper containers start may
# only go to public addresses on TCP 443/8443, plus DNS to the host's upstream resolvers.
#
# Scope: only the compose network of the postbox (source subnet of `pigeonpost-postbox_default`).
# Other containers on the host (registry, directory, loft) and the host itself are untouched.
# Everything lives in one chain, PP-POSTBOX-EGRESS, reached by one jump from DOCKER-USER, so it can
# be inspected and removed in one step:
#
#   egress-firewall.sh apply    # idempotent; run by pigeonpost-postbox-egress.service at boot
#   egress-firewall.sh remove   # rollback
#   egress-firewall.sh show
#
# Fail-open by design: if the network is missing (compose down) nothing is applied and the script
# says so; if the network is recreated with a new subnet, rerun `apply` (the deploy runbook does).
set -eu

NETWORK="${PP_POSTBOX_NETWORK:-pigeonpost-postbox_default}"
CHAIN=PP-POSTBOX-EGRESS
RESOLVERS="${PP_POSTBOX_DNS:-8.8.8.8 1.1.1.1}"
PORTS="${PP_POSTBOX_EGRESS_PORTS:-443,8443}"
PRIVATE="0.0.0.0/8 10.0.0.0/8 100.64.0.0/10 127.0.0.0/8 169.254.0.0/16 172.16.0.0/12 \
192.0.0.0/24 192.0.2.0/24 192.168.0.0/16 198.18.0.0/15 198.51.100.0/24 203.0.113.0/24 \
224.0.0.0/4 240.0.0.0/4"

subnet() {
    docker network inspect "$NETWORK" -f '{{range .IPAM.Config}}{{.Subnet}} {{end}}' 2>/dev/null \
        | tr ' ' '\n' | grep -E '^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+/[0-9]+$' | head -n1
}

remove_jumps() {
    while rule="$(iptables -S DOCKER-USER | grep -- "-j $CHAIN" | head -n1)" && [ -n "$rule" ]; do
        # shellcheck disable=SC2046
        iptables $(echo "$rule" | sed 's/^-A /-D /')
    done
}

case "${1:-}" in
apply)
    net="$(subnet)"
    if [ -z "$net" ]; then
        echo "egress-firewall: network $NETWORK not found; nothing applied" >&2
        exit 0
    fi
    iptables -N DOCKER-USER 2>/dev/null || true
    iptables -N "$CHAIN" 2>/dev/null || true
    iptables -F "$CHAIN"
    # Replies to inbound connections (the published API ports) and anything already allowed.
    iptables -A "$CHAIN" -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
    for r in $RESOLVERS; do
        iptables -A "$CHAIN" -d "$r" -p udp --dport 53 -j RETURN
        iptables -A "$CHAIN" -d "$r" -p tcp --dport 53 -j RETURN
    done
    for p in $PRIVATE; do
        iptables -A "$CHAIN" -d "$p" -j DROP
    done
    iptables -A "$CHAIN" -p tcp -m multiport --dports "$PORTS" -j RETURN
    iptables -A "$CHAIN" -m limit --limit 6/min -j LOG --log-prefix "pp-postbox-egress-drop: "
    iptables -A "$CHAIN" -j DROP
    remove_jumps
    iptables -I DOCKER-USER 1 -s "$net" -j "$CHAIN"
    echo "egress-firewall: $NETWORK ($net) limited to public TCP $PORTS and DNS to $RESOLVERS"
    ;;
remove)
    remove_jumps
    iptables -F "$CHAIN" 2>/dev/null || true
    iptables -X "$CHAIN" 2>/dev/null || true
    echo "egress-firewall: removed"
    ;;
show)
    iptables -S DOCKER-USER
    iptables -S "$CHAIN" 2>/dev/null || echo "($CHAIN not present)"
    ;;
*)
    echo "usage: $0 apply|remove|show" >&2
    exit 2
    ;;
esac
