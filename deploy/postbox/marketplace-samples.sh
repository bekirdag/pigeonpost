#!/usr/bin/env bash
# Seed or withdraw the marketplace's nine "coming soon" sample listings (developer-program plan §1).
#
# Runs on the postbox host against the internal listener, with the ops service token:
#
#   POSTBOX_SVC_TOKEN_OPS=svc_... deploy/postbox/marketplace-samples.sh seed
#   POSTBOX_SVC_TOKEN_OPS=svc_... deploy/postbox/marketplace-samples.sh withdraw
#
# `seed` is idempotent: the samples have fixed ids (`lst_sample_*`) under the platform publisher
# ("Pigeonpost"), and a listing that already exists is left exactly as it is, so a withdrawn sample
# stays withdrawn. `withdraw` takes every sample still listed out of the catalog. To withdraw one
# sample only, use staff moderation on it (POST /v1/staff/listings/{listing}/moderation).
# The postbox needs the `marketplace` feature on for its platform owner account.
set -euo pipefail

INTERNAL="${POSTBOX_INTERNAL_URL:-http://127.0.0.1:8991}"
: "${POSTBOX_SVC_TOKEN_OPS:?set POSTBOX_SVC_TOKEN_OPS to the ops service token}"

case "${1:-}" in
  seed) method=POST ;;
  withdraw) method=DELETE ;;
  *) echo "usage: $0 seed|withdraw" >&2; exit 2 ;;
esac

curl -fsS -X "$method" \
  -H "Authorization: Bearer ${POSTBOX_SVC_TOKEN_OPS}" \
  "${INTERNAL}/internal/v1/marketplace/samples"
echo
