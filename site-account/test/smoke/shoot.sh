#!/bin/sh
# Screenshot every page at 1440x900 and 390x844 from an assembled preview served on :8817.
# usage: test/smoke/shoot.sh <outdir>
C="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
OUT="$1"; mkdir -p "$OUT"
shot() { "$C" --headless=new --disable-gpu --hide-scrollbars --user-data-dir="${TMPDIR:-/tmp}/pp-shoot" --virtual-time-budget=4000 --window-size="$2" --screenshot="$OUT/$1.png" "$3" >/dev/null 2>&1; }
shot signed-out-1440 1440,900 "http://localhost:8817/"
shot signed-out-390 390,844 "http://localhost:8817/__preview/m.html?../index.html"
for r in ${PAGES:-overview inboxes handles handles/get nests store store/services/lst_hr store/services/lst_tr purchases wallet plan billing billing/edit invoices security privacy/delete support}; do
  n=$(echo "$r" | tr / -)
  shot "$n-1440" 1440,900 "http://localhost:8817/__preview/index.html${Q}#/$r"
  shot "$n-390" 390,844 "http://localhost:8817/__preview/m.html?index.html${Q}#/$r"
done
