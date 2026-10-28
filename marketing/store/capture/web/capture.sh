#!/bin/sh
# Web-surface store captures (the pages every app embeds for calls, live streams and meeting notes),
# in headless Chrome at each device class's exact pixels. Synthetic people and media only; every
# page is limited to 127.0.0.1. Needs: Chrome, ImageMagick (magick), livekit-server on PATH, and
# puppeteer-core resolvable from this directory (e.g. `npm i --no-save puppeteer-core@24.10.0`).
#
#   marketing/store/capture/web/capture.sh            # call.png -> marketing/store/captures/<class>/
#   PP_WEB_EXTRA_OUT=<dir> capture.sh --extras        # also the web live/meeting-notes, into <dir>
#
# The store set's live and meeting-notes screens come from the native apps; the web versions here are
# for comparison only and are never written into captures/.
set -eu
cd "$(dirname "$0")"
node call.mjs 7 sfu          # seven people: six tiles fill the grid on phones and tablets
if [ "${1:-}" = "--extras" ]; then
  node notes.mjs
  node live.mjs 24
fi
