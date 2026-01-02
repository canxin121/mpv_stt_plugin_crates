#!/bin/bash -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
. "${ROOT_DIR}/include/path.sh"

build=_build$ndk_suffix

if [ "$1" == "build" ]; then
  true
elif [ "$1" == "clean" ]; then
  rm -rf $build
  exit 0
else
  exit 255
fi

unset CC CXX
rm -rf "$build"
meson setup $build --cross-file "$prefix_dir"/crossfile.txt \
  -Dvulkan=disabled -Ddemos=false

ninja -C $build -j$cores
DESTDIR="$prefix_dir" ninja -C $build install

# ensure C++ stdlib linkage in .pc for static builds
${SED:-sed} '/^Libs:/ s|$| -lc++|' "$prefix_dir/lib/pkgconfig/libplacebo.pc" -i
