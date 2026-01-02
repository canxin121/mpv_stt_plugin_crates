#!/bin/bash -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
. "${ROOT_DIR}/include/path.sh"

if [ "$1" == "build" ]; then
  true
elif [ "$1" == "clean" ]; then
  rm -rf _build$ndk_suffix
  exit 0
else
  exit 255
fi

rm -rf _build$ndk_suffix
mkdir -p _build$ndk_suffix
cd _build$ndk_suffix

CPPFLAGS="-I$prefix_dir/include" LDFLAGS="-L$prefix_dir/lib" \
  ../configure --host="$ndk_triple" --prefix=/usr/local --enable-shared --disable-static
make -j$cores
make install DESTDIR="$prefix_dir"
