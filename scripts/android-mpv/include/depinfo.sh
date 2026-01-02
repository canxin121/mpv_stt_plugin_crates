#!/bin/bash -e

# Versions pulled from mpv-android buildscripts (trimmed for mpv/libmpv build)
v_ndk=r29
v_unibreak=6.1
v_harfbuzz=12.2.0
v_fribidi=1.0.16
v_freetype=2.14.1

# Dependency tree (minimal for libmpv)
dep_ffmpeg=()
dep_freetype2=()
dep_fribidi=()
dep_harfbuzz=()
dep_unibreak=()
dep_libplacebo=()
dep_libass=(freetype2 fribidi harfbuzz unibreak)
dep_mpv=(ffmpeg libass libplacebo)

# pinned ffmpeg revision
v_ci_ffmpeg=n8.0
