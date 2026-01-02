#!/bin/bash -e

. "$(dirname "${BASH_SOURCE[0]}")/depinfo.sh"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
WORK_DIR="${ANDROID_MPV_WORK_DIR:-${ROOT_DIR}/target/android-mpv}"
DEPS_DIR="${ANDROID_MPV_DEPS_DIR:-${WORK_DIR}/deps}"

[ -z "$IN_CI" ] && IN_CI=0
[ -z "$WGET" ] && WGET=wget

mkdir -p "${DEPS_DIR}"
cd "${DEPS_DIR}"

# ffmpeg
if [ ! -d ffmpeg ]; then
  git clone --depth 1 --branch $v_ci_ffmpeg https://github.com/FFmpeg/FFmpeg ffmpeg
fi

# freetype2
[ ! -d freetype2 ] && git clone --recurse-submodules https://gitlab.freedesktop.org/freetype/freetype.git freetype2 -b VER-${v_freetype//./-}

# fribidi
if [ ! -d fribidi ]; then
  mkdir fribidi
  $WGET https://github.com/fribidi/fribidi/releases/download/v$v_fribidi/fribidi-$v_fribidi.tar.xz -O - | \
    tar -xJ -C fribidi --strip-components=1
fi

# harfbuzz
if [ ! -d harfbuzz ]; then
  mkdir harfbuzz
  $WGET https://github.com/harfbuzz/harfbuzz/releases/download/$v_harfbuzz/harfbuzz-$v_harfbuzz.tar.xz -O - | \
    tar -xJ -C harfbuzz --strip-components=1
fi

# unibreak
if [ ! -d unibreak ]; then
  mkdir unibreak
  $WGET https://github.com/adah1972/libunibreak/releases/download/libunibreak_${v_unibreak//./_}/libunibreak-${v_unibreak}.tar.gz -O - | \
    tar -xz -C unibreak --strip-components=1
fi

# libass
[ ! -d libass ] && git clone --depth 1 https://github.com/libass/libass

# libplacebo
[ ! -d libplacebo ] && git clone --depth 1 --recursive https://github.com/haasn/libplacebo

# mpv
[ ! -d mpv ] && git clone --depth 1 https://github.com/mpv-player/mpv

cd ..
