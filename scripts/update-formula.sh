#!/bin/sh
set -eu

version="${1:?usage: scripts/update-formula.sh <version> <SHA256SUMS file>}"
sums="${2:?usage: scripts/update-formula.sh <version> <SHA256SUMS file>}"
formula="$(dirname "$0")/../Formula/tracer.rb"

sum_of() {
  sum="$(awk -v file="trace-$1" '$2 == file { print $1 }' "$sums")"
  if [ -z "$sum" ]; then
    echo "$sums lists no trace-$1" >&2
    exit 1
  fi
  echo "$sum"
}

awk \
  -v version="$version" \
  -v darwin_arm64="$(sum_of darwin-arm64)" \
  -v darwin_x64="$(sum_of darwin-x64)" \
  -v linux_arm64="$(sum_of linux-arm64)" \
  -v linux_x64="$(sum_of linux-x64)" '
  /^  version "/ { sub(/"[^"]*"/, "\"" version "\"") }
  /releases\/download\// {
    sub(/download\/v[^\/]*\//, "download/v" version "/")
    platform = $0
    sub(/.*trace-/, "", platform)
    sub(/".*/, "", platform)
  }
  /^      sha256 "/ {
    sum = platform == "darwin-arm64" ? darwin_arm64 : platform == "darwin-x64" ? darwin_x64 : platform == "linux-arm64" ? linux_arm64 : linux_x64
    sub(/"[^"]*"/, "\"" sum "\"")
  }
  { print }
' "$formula" > "$formula.tmp"
mv "$formula.tmp" "$formula"
