# Web rollout progress

- PR92 merged e017ba3 after all required CI passed at e741b0f. Core image pigeonpost-postbox:rollout-e741b0f built on the production host with2CPU/4GiB limits, id432b8b2ebd603890877b5a6bc3f36211ca49861da908738fdec20ad7b1812bd1. The first build used an unsupported legacy --cpus flag and stopped before building; corrected to the documented CPU period/quota options. Live containers are unchanged.
- Browser16 is our read-only approval-page tab. Public preflight confirms unversioned loader/manifest82e8257b and old WASM match; JavaScript public max-age14400, WASM/HTML no-cache. Raijin origin itself sends no-cache for all four. Preserve that old unversioned vendor bundle while adding the new digest-addressed resources.
- Proposed helpers/configuration are not deployed. Public assets and backend hotfix still await controlled rollout. Apple87 a00c259 requalifies a cache-token merge after b0f66d1 had all checks green; its full merged inbox suite passes198/198.


## Compatibility qualification

- HTML now keys groups.js by the digest of its final stamped source, so a group UI change without a WASM change receives a fresh URL. The3 stamp tests and full inbox **196/196** pass, including10 actual WASM/MLS cases. A local draft overwrote its input with a placeholder and was rejected.
- New map/location snippets preserve the old unversioned bundle, resolve only installed digest folders and refuse invalid/unknown versions. A real isolated nginx on raijin validates both snippets: **eight** legacy/current resources have the exact expected hashes and correct WASM MIME, and **four** malformed/unknown versions return404. The task-owned canary process is stopped in finally; the live webapps master is untouched. Canary metadata/stage are private release artifacts; no secrets or backups in a web root.
- Corrected the static-site runbook's retired wodomini deployment and unsafe backup/delete examples. It now records raijin origins, private backups, tracked assets/operator-config preservation, immutable bundles and public cache-key checks before HTML publication. Nginx has no Docdex AST symbols; exact include scopes and real nginx parsing/HTTP checks supply validation. Source/bundle reproducibility and hosted checks are still required for this final cache-key refinement.
