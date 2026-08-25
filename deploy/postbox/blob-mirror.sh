#!/bin/sh
# Offsite copy of the postbox's attachment volume.
#
# Runs on the *mirror* host and pulls. The postbox cannot push: the mirror is behind NAT with no
# inbound port, and giving the public box a key to write on a private one would put the blast
# radius the wrong way round. A pull needs nothing open on this side and no credential on that one
# beyond a read-only key.
#
# The far side's key is restricted to `rrsync -ro` over the blob directory, so this key cannot get
# a shell, cannot write, and cannot read a byte outside that tree.
#
# It is a mirror, not an archive. `--delete-delay` means what the postbox has let go of — a
# reaped mailbox, a message somebody deleted, an upload that was never sent — this copy lets go of
# on the next run. That is deliberate: an attachment nobody may fetch from the postbox any more
# should not survive here, or the retention promise is only true of one host. What this protects
# against is the volume dying, not somebody deleting something they wanted.
#
# Install: see `## Attachments` in deploy/postbox/README.md.
set -eu

REMOTE="${PIGEONPOST_BLOB_REMOTE:-root@159.69.201.24}"
PORT="${PIGEONPOST_BLOB_PORT:-34251}"
KEY="${PIGEONPOST_BLOB_KEY:-$HOME/.ssh/pigeonpost-blob-mirror}"
DEST="${PIGEONPOST_BLOB_MIRROR:-$HOME/pigeonpost-mirror/blobs}"
LOG="${PIGEONPOST_BLOB_LOG:-$HOME/pigeonpost-mirror/mirror.log}"

# 0700 and nothing wider, on a filesystem that can express it. These are other people's files: the
# postbox holds them at `0700 uid 65532` and a copy that relaxed that would be the weakest link.
mkdir -p "$DEST"
chmod 700 "$DEST"

started=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
# `IdentitiesOnly=yes` is load-bearing, not tidiness. `-i` only *adds* a key: without this, ssh
# offers every other identity this account has first, and if one of those is authorised on the
# postbox without the `rrsync -ro` forced command, the pull silently succeeds against the whole
# remote filesystem instead of the blob directory. That happened once. The restriction lives in the
# far side's authorized_keys, so using the wrong key is what disables it.
if rsync -a --delete-delay --timeout=300 \
    -e "ssh -p $PORT -i $KEY -o IdentitiesOnly=yes -o BatchMode=yes -o ConnectTimeout=20" \
    "$REMOTE:/" "$DEST/" >/tmp/pigeonpost-blob-mirror.$$ 2>&1; then
    files=$(find "$DEST" -type f | wc -l | tr -d ' ')
    bytes=$(du -sk "$DEST" | cut -f1)
    echo "$started ok files=$files kbytes=$bytes" >>"$LOG"
    rm -f /tmp/pigeonpost-blob-mirror.$$
else
    status=$?
    # The failure text matters more than the exit code — an expired key and a full disk both stop
    # the mirror, and only one of them is urgent. Bounded at twenty lines, because a run that goes
    # wrong per-file writes one complaint per file: the first failure here put 195 KB of "mkdir
    # failed" into this log, and a log that grows with the size of the mistake is its own problem.
    echo "$started FAILED rsync=$status" >>"$LOG"
    tail -20 /tmp/pigeonpost-blob-mirror.$$ | sed 's/^/    /' >>"$LOG"
    rm -f /tmp/pigeonpost-blob-mirror.$$
    exit "$status"
fi
