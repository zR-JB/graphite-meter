#!/usr/bin/env bash
# apt-cached.sh <dir>: installs the target checks' C cross compilers from the .debs in <dir>, downloading them there
# first when no cache restored them. Ubuntu's mirror can trickle at a few KB/s; a warm cache never reaches it.
set -euo pipefail
packages=(gcc-mingw-w64-x86-64-win32 mingw-w64-x86-64-dev musl-tools gcc-aarch64-linux-gnu libc6-dev-arm64-cross)
dir=$1
apt=(sudo apt-get -o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Dir::Cache::archives="$dir")
mkdir -p "$dir/partial"
if ! compgen -G "$dir/*.deb" >/dev/null; then
  "${apt[@]}" update
  "${apt[@]}" install -y --no-install-recommends --download-only "${packages[@]}"
fi
# Nothing was missing, so nothing was downloaded.
compgen -G "$dir/*.deb" >/dev/null || exit 0
# apt orders upgrades of installed packages, which dpkg -i alone refuses; it never reaches the network here.
"${apt[@]}" install -y --no-install-recommends --no-download "$dir"/*.deb
