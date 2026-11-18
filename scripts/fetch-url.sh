#!/usr/bin/env bash
# Download one URL to a file, with bounded time, for the CI workflows.
#   fetch-url.sh <out> <url>
# Arguments are in curl's `-o <out> <url>` order so a workflow line changes only its command.
#
# Why not `curl --retry 3`: on 2026-11-18 www.gutenberg.org answered every request with
# `503` and `retry-after: 3600`. curl's own retry obeys that header, so each retry slept an hour
# and the book downloads hung until every job's 150-minute timeout. And with `--retry-max-time`
# below the header's value, curl 8.18 gave up and exited 0 without writing the file.
#
# So the retries are done here, each attempt bounded (connect timeout, a stall cut-off of under
# 10 KiB/s for 60 s, a 300 s cap), and success means curl exited 0 *and* the file is non-empty.
# A www.gutenberg.org URL that still fails is tried on the gutenberg.pglaf.org mirror, which
# serves the same /cache/epub paths. On failure the partial file is removed and the exit is 1,
# so the caller keeps deciding whether a missing input is a warning or an error.
set -u
if [ "$#" -ne 2 ]; then
  echo "usage: fetch-url.sh <out> <url>" >&2
  exit 2
fi
out=$1
url=$2

fetch() {
  local src=$1 attempt
  for attempt in 1 2 3; do
    rm -f "$out"
    if curl -fsSL \
        --connect-timeout 20 \
        --speed-limit 10240 --speed-time 60 \
        --max-time 300 \
        -o "$out" "$src" && [ -s "$out" ]; then
      return 0
    fi
    [ "$attempt" -lt 3 ] && sleep $((attempt * 5))
  done
  return 1
}

if fetch "$url"; then
  exit 0
fi
case "$url" in
  https://www.gutenberg.org/* | https://gutenberg.org/*)
    mirror="https://gutenberg.pglaf.org/${url#https://*gutenberg.org/}"
    echo "::warning::$url failed; trying the mirror $mirror" >&2
    if fetch "$mirror"; then
      exit 0
    fi
    ;;
esac
rm -f "$out"
exit 1
