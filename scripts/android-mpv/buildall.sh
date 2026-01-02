#!/bin/bash -e

SCRIPT_DIR="$(cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
cd "${SCRIPT_DIR}"
. ./include/depinfo.sh

cleanbuild=0
nodeps=0
clang=1
target=mpv
arch=armv7l
ANDROID_API_LEVEL=${ANDROID_API:-${API:-21}}
WORK_DIR="${ANDROID_MPV_WORK_DIR:-${ROOT_DIR}/target/android-mpv}"
DEPS_DIR="${ANDROID_MPV_DEPS_DIR:-${WORK_DIR}/deps}"
ANDROID_PREFIX_BASE="${ANDROID_MPV_PREFIX_BASE:-${WORK_DIR}/prefix}"

getdeps () {
  varname="dep_${1//-/_}[*]"
  echo ${!varname}
}

loadarch () {
  unset CC CXX CPATH LIBRARY_PATH C_INCLUDE_PATH CPLUS_INCLUDE_PATH
  unset CFLAGS CXXFLAGS CPPFLAGS LDFLAGS

  local apilvl=${ANDROID_API_LEVEL}
  if [ "$1" == "armv7l" ]; then
    export ndk_suffix=
    export ndk_triple=arm-linux-androideabi
    cc_triple=armv7a-linux-androideabi$apilvl
    prefix_name=armv7l
  elif [ "$1" == "arm64" ]; then
    export ndk_suffix=-arm64
    export ndk_triple=aarch64-linux-android
    cc_triple=$ndk_triple$apilvl
    prefix_name=arm64
  elif [ "$1" == "x86" ]; then
    export ndk_suffix=-x86
    export ndk_triple=i686-linux-android
    cc_triple=$ndk_triple$apilvl
    prefix_name=x86
  elif [ "$1" == "x86_64" ]; then
    export ndk_suffix=-x64
    export ndk_triple=x86_64-linux-android
    cc_triple=$ndk_triple$apilvl
    prefix_name=x86_64
  else
    echo "Invalid architecture" >&2
    exit 1
  fi

  export prefix_dir="${ANDROID_PREFIX_BASE}/${prefix_name}"
  if [ ! -d "$prefix_dir" ]; then
    mkdir -p "$prefix_dir"
    ln -sf . "$prefix_dir/usr"
    ln -sf . "$prefix_dir/local"
  fi

  # toolchain bin should be on PATH via include/path.sh; ensure cc vars set
  if [ $clang -eq 1 ]; then
    export CC=$cc_triple-clang
    export CXX=$cc_triple-clang++
  else
    export CC=$cc_triple-gcc
    export CXX=$cc_triple-g++
  fi
  export LDFLAGS="-Wl,-O1,--icf=safe -Wl,-z,max-page-size=16384"
  export AR=llvm-ar
  export RANLIB=llvm-ranlib
}

setup_prefix () {
  local cpu_family=${ndk_triple%%-*}
  [ "$cpu_family" == "i686" ] && cpu_family=x86

  if ! command -v pkg-config >/dev/null; then
    echo "pkg-config not provided!"
    return 1
  fi

  cat >"$prefix_dir/crossfile.tmp" <<CROSSFILE
[built-in options]
buildtype = 'release'
default_library = 'static'
wrap_mode = 'nodownload'
prefix = '/usr/local'
[binaries]
c = '$CC'
cpp = '$CXX'
ar = 'llvm-ar'
nm = 'llvm-nm'
strip = 'llvm-strip'
pkgconfig = 'pkg-config'
pkg-config = 'pkg-config'
[host_machine]
system = 'android'
cpu_family = '$cpu_family'
cpu = '${CC%%-*}'
endian = 'little'
CROSSFILE
  if cmp -s "$prefix_dir"/crossfile.{tmp,txt}; then
    rm "$prefix_dir/crossfile.tmp"
  else
    mv "$prefix_dir"/crossfile.{tmp,txt}
  fi

  cat >"$prefix_dir/cmake_toolchain.cmake" <<CMTOOL
set(CMAKE_SYSTEM_NAME Android)
set(CMAKE_SYSTEM_VERSION ${ANDROID_API_LEVEL})
set(CMAKE_ANDROID_ARCH_ABI ${cc_triple%%-*})
set(CMAKE_ANDROID_NDK $(resolve_ndk_path))
set(CMAKE_ANDROID_STL_TYPE c++_shared)
set(CMAKE_C_COMPILER ${CC})
set(CMAKE_CXX_COMPILER ${CXX})
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
CMTOOL
}

build () {
  if [ $1 != "mpv" ] && [ ! -d "$DEPS_DIR/$1" ]; then
    printf >&2 '\e[1;31m%s\e[m\n' "Target $1 not found"
    return 1
  fi
  if [ $nodeps -eq 0 ]; then
    printf >&2 '\e[1;34m%s\e[m\n' "Preparing $1..."
    local deps=$(getdeps $1)
    echo >&2 "Dependencies: $deps"
    for dep in $deps; do
      build $dep
    done
  fi
  printf >&2 '\e[1;34m%s\e[m\n' "Building $1..."
  if [ "$1" == "mpv" ]; then
    pushd "$DEPS_DIR/mpv" >/dev/null
    BUILDSCRIPT="${SCRIPT_DIR}/scripts/$1.sh"
  else
    pushd "$DEPS_DIR/$1" >/dev/null
    BUILDSCRIPT="${SCRIPT_DIR}/scripts/$1.sh"
  fi
  [ $cleanbuild -eq 1 ] && $BUILDSCRIPT clean
  $BUILDSCRIPT build
  popd >/dev/null
}

usage () {
  printf '%s\n' \
    "Usage: buildall.sh [options] [target]" \
    "Builds the specified target (default: $target)" \
    "-n             Do not build dependencies" \
    "--clean        Clean build dirs before compiling" \
    "--gcc          Use gcc compiler (unsupported!)" \
    "--arch <arch>  Build for specified architecture (default: $arch; supported: armv7l, arm64, x86, x86_64)"
  exit 0
}

while [ $# -gt 0 ]; do
  case "$1" in
    --clean) cleanbuild=1 ;;
    -n|--no-deps) nodeps=1 ;;
    --gcc) clang=0 ;;
    --arch) shift; arch=$1 ;;
    -h|--help) usage ;;
    -*)
      echo "Unknown flag $1" >&2
      exit 1
      ;;
    *)
      target=$1
      ;;
  esac
  shift
done

# ensure deps sources
ANDROID_MPV_WORK_DIR="$WORK_DIR" ANDROID_MPV_DEPS_DIR="$DEPS_DIR" ./include/download-deps.sh

loadarch $arch
. ./include/path.sh
setup_prefix
build $target

exit 0
