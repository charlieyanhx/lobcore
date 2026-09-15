#!/bin/sh
# Range-download the first 50 MiB of the public Nasdaq TotalView-ITCH 5.0 sample day 2019-12-30 into data/
# (gitignored). The bytes are Nasdaq's and are not redistributed; every number lobcore reports from them is an
# aggregate. The md5 pins the exact prefix the README's local-only rows were measured on.
set -eu
URL='https://emi.nasdaq.com/ITCH/Nasdaq%20ITCH/12302019.NASDAQ_ITCH50.gz'
OUT="$(dirname "$0")/../data/itch_12302019_head50m.gz"
WANT_MD5='8bd91e6f5b4a31d4d50dd6ac8a8fe7e2'
mkdir -p "$(dirname "$OUT")"
curl -sS -r 0-52428799 -o "$OUT" "$URL"
GOT=$(md5 -q "$OUT" 2>/dev/null || md5sum "$OUT" | cut -d' ' -f1)
[ "$GOT" = "$WANT_MD5" ] || { echo "md5 mismatch: got $GOT, want $WANT_MD5 (the sample changed or the download was partial)"; exit 1; }
echo "ok: $OUT ($(wc -c < "$OUT") bytes, md5 $GOT)"
