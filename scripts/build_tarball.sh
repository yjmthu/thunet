#!/usr/bin/env bash
# Build the portable Linux tarball.
#
# The version comes from Cargo.toml unless one is passed explicitly:
#     pixi run -e package tarball
#     bash scripts/build_tarball.sh 0.2.0
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

version="${1:-}"
if [ -z "$version" ]; then
  version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)"
fi
if [ -z "$version" ]; then
  echo "cannot read version from Cargo.toml; pass one as the first argument" >&2
  exit 1
fi

stage="dist/thunet"
archive="dist/thunet-$version-linux-x64.tar.gz"

rm -rf "$stage"
mkdir -p "$stage"
cp target/release/thunet assets/thunet.desktop assets/thunet.svg "$stage/"
tar -C dist -czf "$archive" thunet

echo "built $archive"
