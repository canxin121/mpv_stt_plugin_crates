#!/usr/bin/env bash
set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${WORKSPACE_ROOT}/dist"
BUILD_LOG="${DIST_DIR}/build.log"
# Preserve the caller's original RUSTFLAGS so per-ABI setup can start clean.
ORIG_RUSTFLAGS="${RUSTFLAGS:-}"

# --- Embedded dependency/bootstrap helpers (inlined former setup-deps.*) ----

MPV_REPO_DEFAULT="https://github.com/mpv-player/mpv.git"

resolve_ndk() {
    local ndk_default="${WORKSPACE_ROOT}/target/android-ndk-r29"
    local ndk_path="${ANDROID_NDK_HOME:-${NDK:-${CMAKE_ANDROID_NDK:-${ndk_default}}}}"
    echo "${ndk_path}"
}

ensure_ndk() {
    local ndk_path
    ndk_path="$(resolve_ndk)"
    if [[ ! -d "${ndk_path}" ]]; then
        echo "NDK not found: ${ndk_path}. Set ANDROID_NDK_HOME or NDK." >&2
        return 1
    fi
    echo "${ndk_path}"
}

android_abi_spec() {
    case "$1" in
        arm64-v8a)   echo "arm64-v8a:arm64:aarch64-linux-android:aarch64-linux-android" ;;
        armeabi-v7a) echo "armeabi-v7a:armv7l:armv7-linux-androideabi:armv7a-linux-androideabi" ;;
        x86)         echo "x86:x86:i686-linux-android:i686-linux-android" ;;
        x86_64)      echo "x86_64:x86_64:x86_64-linux-android:x86_64-linux-android" ;;
        *)           return 1 ;;
    esac
}

ensure_rust_target() {
    local target="$1"
    if ! rustup target list --installed | grep -q "^${target}$"; then
        rustup target add "$target"
    fi
}

ensure_mpv_headers() {
    local cache_dir="$1"
    local repo="${2:-$MPV_REPO_DEFAULT}"
    if [[ ! -d "${cache_dir}" ]]; then
        echo "[setup] Cloning mpv headers (depth=1) into ${cache_dir}..."
        git clone --depth 1 "${repo}" "${cache_dir}"
    fi
}

find_mpv_include_dir() {
    local cache_dir="$1"
    local candidates=(
        "${cache_dir}/include"
        "${cache_dir}/libmpv"
        "${cache_dir}"
    )
    for p in "${candidates[@]}"; do
        if [[ -f "${p}/mpv/client.h" ]]; then
            echo "${p}"
            return 0
        fi
    done
    return 1
}

host_tag() {
    local host_os host_arch
    host_os="$(uname -s)"
    host_arch="$(uname -m)"
    case "${host_os}" in
        Linux)  echo "linux-${host_arch}" ;;
        Darwin) [[ "${host_arch}" == "arm64" ]] && echo "darwin-arm64" || echo "darwin-x86_64" ;;
        *)      echo "linux-${host_arch}" ;;
    esac
}

# Rust host triple of the current machine (used to skip --target for native
# builds, which is required for ffmpeg's static source build).
host_rust_target() {
    rustc -vV 2>/dev/null | sed -n 's/^host: //p'
}

ensure_mpv_prefix() {
    local arch="$1"
    local builder_dir="$2"
    local work_dir="$3"
    local prefix_base="$4"
    local api_default="$5"

    local prefix="${prefix_base}/${arch}/usr/local"
    if [[ -f "${prefix}/lib/libmpv.so" ]]; then
        echo "${prefix}"
        return 0
    fi

    echo "libmpv.so not found for ${arch}, building mpv (android-mpv helper)..." >&2
    # Run helper with a minimal environment to avoid hitting ARG_MAX after large Android vars accumulate.
    # IMPORTANT: redirect the helper's stdout/stderr into the build log. This
    # function is invoked via command substitution (prefix="$(ensure_mpv_prefix
    # ...)"), so any stdout here becomes $prefix; the android-mpv build scripts
    # run configure/make/meson/ninja with no redirects, so their output would be
    # captured into $prefix, and the later "export FFMPEG_DIR=$prefix" etc. would
    # blow the environment past exec's ARG_MAX (every later command fails with
    # "Argument list too long").
    (cd "${builder_dir}" && env -i \
        PATH="${PATH}" HOME="${HOME:-/tmp}" TERM="${TERM:-}" \
        ANDROID_MPV_WORK_DIR="${work_dir}" \
        ANDROID_MPV_PREFIX_BASE="${prefix_base}" \
        ANDROID_NDK_HOME="$(resolve_ndk)" \
        ANDROID_API="${api_default}" \
        ./buildall.sh --arch "${arch}" mpv) >> "${BUILD_LOG}" 2>&1

    if [[ -f "${prefix}/lib/libmpv.so" ]]; then
        echo "${prefix}"
        return 0
    fi

    echo "Failed to build libmpv.so for ${arch}" >&2
    return 1
}

setup_android_env() {
    local abi="${1:-}"
    if [[ -z "${abi}" ]]; then
        echo "No ANDROID_ABI provided. Set MPV_STT_PLUGIN_RS_ANDROID_ABI or call with an ABI." >&2
        return 1
    fi

    local spec
    spec="$(android_abi_spec "${abi}")" || { echo "Unknown ABI: ${abi}" >&2; return 1; }
    IFS=":" read -r _abi arch rust_target clang_target <<<"${spec}"

    local ndk_path
    ndk_path="$(ensure_ndk)" || return 1
    local host_tag_val
    host_tag_val="$(host_tag)"
    local toolchain_root="${ndk_path}/toolchains/llvm/prebuilt/${host_tag_val}"
    if [[ ! -d "${toolchain_root}" ]]; then
        # Diagnose the toolchain lookup: if host_tag came back with an empty
        # arch (e.g. "linux-"), show exactly what uname reported.
        echo "Toolchain not found: ${toolchain_root}" >&2
        echo "  (host_tag='${host_tag_val}', uname -s='$(uname -s)', uname -m='$(uname -m)', OSTYPE='${OSTYPE:-unset}')" >&2
        return 1
    fi
    local sysroot="${toolchain_root}/sysroot"

    ensure_rust_target "${rust_target}"

    local api="${ANDROID_API:-${API:-${ANDROID_API_DEFAULT}}}"
    local prefix_base="${ANDROID_PREFIX_BASE:-${WORKSPACE_ROOT}/target/android-mpv/prefix}"
    local work_dir="${ANDROID_WORK_DIR:-${WORKSPACE_ROOT}/target/android-mpv}"
    local builder_dir="${ANDROID_MPV_BUILDER_DIR:-${WORKSPACE_ROOT}/scripts/android-mpv}"
    local prefix
    prefix="$(ensure_mpv_prefix "${arch}" "${builder_dir}" "${work_dir}" "${prefix_base}" "${api}")" || return 1

    export ANDROID_ABI="${abi}"
    export ANDROID_API="${api}"
    export NDK="${ndk_path}"
    export ANDROID_NDK_HOME="${ndk_path}"
    export ANDROID_SYSROOT="${sysroot}"
    export PATH="${toolchain_root}/bin:${PATH}"

    export CC="${toolchain_root}/bin/${clang_target}${api}-clang"
    export CXX="${toolchain_root}/bin/${clang_target}${api}-clang++"
    export AR="${toolchain_root}/bin/llvm-ar"
    export RANLIB="${toolchain_root}/bin/llvm-ranlib"
    export STRIP="${toolchain_root}/bin/llvm-strip"
    export "CC_${rust_target//-/_}"="${CC}"
    export TARGET_CC="${CC}"

    local linker_var="CARGO_TARGET_$(echo "${rust_target}" | tr '[:lower:]' '[:upper:]' | tr '-' '_')_LINKER"
    export "${linker_var}"="${CC}"

    export BINDGEN_EXTRA_CLANG_ARGS="--target=${clang_target} --sysroot=${sysroot} -I${prefix}/include"
    export CMAKE_TOOLCHAIN_FILE="${WORKSPACE_ROOT}/toolchains/android.cmake"
    export FFMPEG_DIR="${prefix}"
    export MPV_PREFIX="${prefix}"
    export MPV_INCLUDE_DIR="${prefix}/include"
    export LIBMPV_LIB_DIR="${prefix}/lib"
    # Reset per-ABI search paths to avoid leaking other architectures.
    export LIBRARY_PATH="${prefix}/lib"
    export PKG_CONFIG_PATH="${prefix}/lib/pkgconfig"
    export PKG_CONFIG_LIBDIR="${prefix}/lib/pkgconfig"
    export PKG_CONFIG_ALLOW_CROSS=1
    export CARGO_NDK_SYSROOT_PATH="${sysroot}"
    export ANDROID_SYSROOT="${sysroot}"
    # Rewrite RUSTFLAGS for each ABI so earlier -L entries (e.g., arm64 when building armv7) are discarded.
    export RUSTFLAGS="${ORIG_RUSTFLAGS} -C link-arg=-Wl,-z,defs -L${prefix}/lib -lmpv"

    # Per-target hints some crates expect
    local target_env_var
    target_env_var=${rust_target//-/_}
    export "CMAKE_TOOLCHAIN_FILE_${target_env_var}"="${CMAKE_TOOLCHAIN_FILE}"
    export "CMAKE_PREFIX_PATH_${target_env_var}"="${prefix}"
    export "CMAKE_SYSTEM_NAME_${target_env_var}"="Android"
    export "CMAKE_SYSTEM_PROCESSOR_${target_env_var}"="${clang_target%%-*}"
}

# Windows/git-bash PATH fix. Git Bash prepends its own /usr/bin (which ships a
# GNU link.exe) ahead of the MSVC bin dirs, so when cl.exe internally spawns
# link.exe during ffmpeg configure's compile test, it gets Git's linker and
# fails with "cl.exe is unable to create an executable file". Put the MSVC bin
# dir first so both cl.exe's internal link step and cc-based build scripts use
# the real MSVC linker. No-op when VCToolsInstallDir is unset (non-Windows).
fixup_windows_path() {
    if [[ -z "${VCToolsInstallDir:-}" ]]; then
        return
    fi
    local vc_bin
    vc_bin="${VCToolsInstallDir//\\//}/bin/Hostx64/x64"
    if [[ -d "${vc_bin}" ]]; then
        export PATH="${vc_bin}:${PATH}"
        echo "[setup] Prepended MSVC bin to PATH: ${vc_bin}" >&2
    fi
}

ensure_setup_env() {
    if [[ -n "${SETUP_ENV_DONE:-}" ]]; then
        return
    fi

    fixup_windows_path

    local cache_dir="${WORKSPACE_ROOT}/target/mpv-headers"
    local repo="${MPV_REPO:-$MPV_REPO_DEFAULT}"
    ensure_mpv_headers "${cache_dir}" "${repo}"

    MPV_INCLUDE_DIR="${MPV_INCLUDE_DIR:-$(find_mpv_include_dir "${cache_dir}" || true)}"
    if [[ -z "${MPV_INCLUDE_DIR}" ]]; then
        echo "mpv/client.h not found in ${cache_dir}; set MPV_INCLUDE_DIR manually." >&2
        exit 1
    fi

    export MPV_INCLUDE_DIR
    export BINDGEN_EXTRA_CLANG_ARGS="-I${MPV_INCLUDE_DIR}"
    export RUSTFLAGS="${RUSTFLAGS:-} -A deprecated"
    export CMAKE_INSTALL_LIBDIR="${CMAKE_INSTALL_LIBDIR:-lib}"

    # Android defaults / paths used by setup_android_env
    ANDROID_API_DEFAULT="${ANDROID_API:-${API:-21}}"
    ANDROID_WORK_DIR="${ANDROID_WORK_DIR:-${WORKSPACE_ROOT}/target/android-mpv}"
    ANDROID_PREFIX_BASE="${ANDROID_PREFIX_BASE:-${ANDROID_WORK_DIR}/prefix}"
    ANDROID_MPV_BUILDER_DIR="${ANDROID_MPV_BUILDER_DIR:-${WORKSPACE_ROOT}/scripts/android-mpv}"

    if [[ -n "${MPV_STT_PLUGIN_RS_ANDROID_ABI:-}" ]]; then
        setup_android_env "${MPV_STT_PLUGIN_RS_ANDROID_ABI}"
    fi

    SETUP_ENV_DONE=1
}

# Color output (bash 3.2-compatible; GNU `date` provides the timestamp)
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color
timestamp() { date +%H:%M:%S; }
log_core() { local lvl="$1"; shift; local msg="$*"; local line="${lvl}[$(timestamp)]${NC} ${msg}"; printf "%b\n" "${line}"; printf "%b\n" "${line}" >> "${BUILD_LOG}"; }
log()  { log_core "${GREEN}" "$*"; }
warn() { log_core "${YELLOW}[WARN]" "$*"; }
error(){ log_core "${RED}[ERROR]" "$*"; }

# Platform configurations (single .so per platform/ABI, both STT backends are
# compiled in via default features and picked at runtime via config.stt.backend).
# Parallel arrays (bash 3.2-compatible; the GitHub macOS runner ships bash 3.2,
# which has no associative arrays or namerefs).
DESKTOP_PLATFORMS_ALL=(linux-x86_64 darwin-arm64 darwin-x86_64 windows-x86_64)
DESKTOP_TARGETS=(x86_64-unknown-linux-gnu aarch64-apple-darwin x86_64-apple-darwin x86_64-pc-windows-msvc)

target_for_platform() {
    local p="$1" i
    for i in "${!DESKTOP_PLATFORMS_ALL[@]}"; do
        if [[ "${DESKTOP_PLATFORMS_ALL[$i]}" == "$p" ]]; then
            echo "${DESKTOP_TARGETS[$i]}"
            return 0
        fi
    done
    return 1
}

# Android ABI configurations (details resolved via android_abi_spec in setup-deps)
SUPPORTED_ANDROID_ABIS=("arm64-v8a" "armeabi-v7a" "x86" "x86_64")
# Default is 64-bit only: the 32-bit ABIs (armeabi-v7a, x86) are currently
# blocked by an upstream ffmpeg-sys-next bug — its Vulkan stub header hardcodes
# `sizeof(VkPhysicalDeviceFeatures2) == 240`, which only holds for 64-bit
# pointers, so bindgen fails on 32-bit targets.
DEFAULT_ANDROID_ABIS=("arm64-v8a")

# Feature configurations. The plugin is a pure remote client; both backends are
# compiled in by default (single .so, runtime switch). `-f` selects a
# single-backend build (--no-default-features) — an optional override.
PLUGIN_FEATURES=("stt_ferrum" "stt_openai")

# CLI selections (populated by parse_args)
SELECTED_PLATFORMS=()
SELECTED_FEATURES=()
SELECTED_ANDROID_ABIS=()
CLEAN_DIST=0
SHOW_MATRIX=0
DESKTOP_PLATFORMS=()
DO_ANDROID=0
BUILD_MODE="build" # or "check"

usage() {
    cat <<'EOUSAGE'
Usage: ./scripts/build-all.sh [options]

Build the mpv STT plugin across platforms. Defaults to the full matrix.

One artifact per platform/ABI (both STT backends compiled in, switched at
runtime via config.stt.backend); pass -f for a single-backend build.

Options:
  -p, --platform   <list>   Comma-separated platforms (linux-x86_64, darwin-arm64,
                            darwin-x86_64, windows-x86_64, android)
  -f, --feature    <list>   Optional single-backend build: comma-separated features
                            (stt_ferrum, stt_openai). Omit to build both backends in
                            one .so (default).
  -a, --abi        <list>   Comma-separated Android ABIs (arm64-v8a, armeabi-v7a, x86, x86_64)
      --check               Run cargo check instead of building artifacts
      --clean               Remove dist/ before building (default: keep)
  -l, --list               Show supported values and exit
  -h, --help               Show this help and exit

Examples:
  # Full matrix (default): one libmpv_stt_plugin.so per platform/ABI, both backends
  ./scripts/build-all.sh

  # Single platform (e.g. the current macOS host)
  ./scripts/build-all.sh -p darwin-arm64

  # Single-backend build (OpenAI only)
  ./scripts/build-all.sh -p darwin-arm64 -f stt_openai

  # Android arm64 (needs NDK; ferrum pulls opusic-sys cross-compile).
  # 32-bit ABIs (armeabi-v7a, x86) are currently blocked by an upstream
  # ffmpeg-sys-next Vulkan-stub assert; see DEFAULT_ANDROID_ABIS.
  ./scripts/build-all.sh -p android -a arm64-v8a

Environment:
  Desktop platforms link dynamically against a prebuilt FFmpeg (no source
  compile):
    FFPREFIX          macOS only: brew FFmpeg prefix (default: brew --prefix ffmpeg)
    FFMPEG_BTBN_URL   Linux/Windows: override the auto-resolved BtbN asset URL
    FFMPEG_DIR        If set, used as-is on every platform (CI override)
  The BtbN packages are cached under target/ffmpeg-btbn and the Linux/Windows
  runtime libraries are copied into dist/<platform>/runtime.
EOUSAGE
}

print_supported() {
    echo "Supported platforms : ${DESKTOP_PLATFORMS_ALL[*]} android"
    echo "Plugin features     : ${PLUGIN_FEATURES[*]} (omit -f to build both in one .so)"
    echo "Android ABIs        : ${SUPPORTED_ANDROID_ABIS[*]}"
}

append_list() {
    # Append comma-separated "$2" into the global array named by "$1".
    # (bash 3.2-compatible: no nameref; eval with a fixed internal name.)
    local name="$1" p
    IFS=',' read -ra parts <<<"$2"
    for p in "${parts[@]}"; do
        [[ -n "$p" ]] && eval "${name}+=(\"\$p\")"
    done
}

dedup_array() {
    # Dedup the global array named by "$1" in place (bash 3.2-compatible).
    # bash 3.2 + nounset treats expanding an EMPTY array as unbound, so the
    # whole function runs with nounset off (no early returns to leak it).
    set +u
    local name="$1" seen="" item
    local -a src out=()
    eval "src=(\"\${$name[@]}\")"
    for item in "${src[@]}"; do
        if [[ -z "$item" || ",${seen}," == *",${item},"* ]]; then
            continue
        fi
        out+=("$item")
        seen="${seen:+${seen},}${item}"
    done
    eval "${name}=(\"\${out[@]}\")"
    set -u
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -p|--platform)
                [[ $# -lt 2 ]] && { echo "ERROR: --platform requires a value" >&2; exit 1; }
                append_list SELECTED_PLATFORMS "$2"
                shift 2
                ;;
            -f|--feature)
                [[ $# -lt 2 ]] && { echo "ERROR: --feature requires a value" >&2; exit 1; }
                append_list SELECTED_FEATURES "$2"
                shift 2
                ;;
            -a|--abi)
                [[ $# -lt 2 ]] && { echo "ERROR: --abi requires a value" >&2; exit 1; }
                append_list SELECTED_ANDROID_ABIS "$2"
                shift 2
                ;;
            --check)
                BUILD_MODE="check"
                CLEAN_DIST=0
                shift
                ;;
            --clean)
                CLEAN_DIST=1
                shift
                ;;
            -l|--list|--matrix)
                SHOW_MATRIX=1
                shift
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                echo "ERROR: Unknown option: $1" >&2
                usage
                exit 1
                ;;
        esac
    done
}

set_defaults() {
    if [[ ${#SELECTED_PLATFORMS[@]} -eq 0 ]]; then
        SELECTED_PLATFORMS=("${DESKTOP_PLATFORMS_ALL[@]}" "android")
    fi
    if [[ ${#SELECTED_ANDROID_ABIS[@]} -eq 0 ]]; then
        SELECTED_ANDROID_ABIS=("${DEFAULT_ANDROID_ABIS[@]}")
    fi

    dedup_array SELECTED_PLATFORMS
    dedup_array SELECTED_FEATURES
    dedup_array SELECTED_ANDROID_ABIS
}

is_supported_android_abi() {
    local candidate="$1"
    for abi in "${SUPPORTED_ANDROID_ABIS[@]}"; do
        [[ "$abi" == "$candidate" ]] && return 0
    done
    return 1
}

validate_inputs() {
    local ok=1

    for p in "${SELECTED_PLATFORMS[@]+"${SELECTED_PLATFORMS[@]}"}"; do
        if [[ "$p" != "android" && -z "$(target_for_platform "$p")" ]]; then
            echo "ERROR: Unknown platform '${p}'" >&2
            ok=0
        fi
    done

    for a in "${SELECTED_ANDROID_ABIS[@]+"${SELECTED_ANDROID_ABIS[@]}"}"; do
        if ! is_supported_android_abi "$a"; then
            echo "ERROR: Unknown Android ABI '${a}'" >&2
            ok=0
        fi
    done

    for f in "${SELECTED_FEATURES[@]+"${SELECTED_FEATURES[@]}"}"; do
        local found=0
        for af in "${PLUGIN_FEATURES[@]}"; do
            if [[ "$af" == "$f" ]]; then
                found=1
                break
            fi
        done
        if [[ $found -eq 0 ]]; then
            echo "ERROR: Unknown feature '${f}'" >&2
            ok=0
        fi
    done

    if [[ $ok -eq 0 ]]; then
        exit 1
    fi
}

compute_platforms() {
    DESKTOP_PLATFORMS=()
    DO_ANDROID=0

    for p in "${SELECTED_PLATFORMS[@]+"${SELECTED_PLATFORMS[@]}"}"; do
        if [[ "$p" == "android" ]]; then
            DO_ANDROID=1
        elif [[ -n "$(target_for_platform "$p")" ]]; then
            DESKTOP_PLATFORMS+=("$p")
        fi
    done
}

describe_array() {
    # Print a global array named by "$1" joined by spaces, or "$2" if empty.
    local name="$1" fallback="$2"
    local -a arr=()
    set +u
    eval "arr=(\"\${$name[@]}\")"
    set -u
    if [[ ${#arr[@]} -eq 0 ]]; then
        echo "${fallback}"
    else
        echo "${arr[*]}"
    fi
}

# Build spec: with no -f we build default features (both backends, one .so);
# with -f we build --no-default-features --features X per selected feature.
# Writes into the global array named by "$1" (bash 3.2-compatible).
get_build_specs() {
    local name="$1" f
    local -a feats=()
    set +u
    eval "feats=(\"\${SELECTED_FEATURES[@]}\")"
    set -u
    eval "${name}=()"

    if [[ ${#feats[@]} -eq 0 ]]; then
        eval "${name}+=(\"\")"
        return
    fi

    for f in "${feats[@]}"; do
        eval "${name}+=(\"\$f\")"
    done
}

is_in_array() {
    local needle="$1"; shift
    for item in "$@"; do
        [[ "$item" == "$needle" ]] && return 0
    done
    return 1
}

# Check environment
check_env() {
    log "Checking build environment..."

    if [[ -z "${MPV_INCLUDE_DIR:-}" ]]; then
        error "Environment bootstrap failed (MPV_INCLUDE_DIR missing)."
        exit 1
    fi
}

# Artifact filename for the single crate on a given rust target triple.
artifact_name() {
    local target="$1"
    case "$target" in
        *windows*)   echo "mpv_stt_plugin.dll" ;;
        *apple*)     echo "libmpv_stt_plugin.dylib" ;;
        *)           echo "libmpv_stt_plugin.so" ;;
    esac
}

# Build Android plugin for a specific ABI using android-mpv toolchain helpers.
# `spec` is empty (default, both backends) or a single feature (single-backend).
build_android_abi() {
    local abi="$1"
    local spec="$2"

    local spec_desc
    if [[ -n "${spec}" ]]; then
        spec_desc="${spec}"
    else
        spec_desc="stt_ferrum,stt_openai"
    fi

    local abi_spec
    if ! abi_spec="$(android_abi_spec "$abi")"; then
        error "Unknown Android ABI '${abi}'"
        return 1
    fi

    IFS=":" read -r _abi arch rust_target clang_target <<<"${abi_spec}"

    # Prepare env + deps (builds mpv/ffmpeg prefix on demand via scripts/android-mpv)
    if ! setup_android_env "$abi"; then
        error "Failed to set Android env for ${abi}"
        return 1
    fi

    log "Building mpv-stt-plugin [${spec_desc}] for Android ${abi} (${rust_target})..."

    local cargo_cmd="${BUILD_MODE}"

    local cargo_args=(
        "${cargo_cmd}"
        "--release"
        "--target" "${rust_target}"
    )

    if [[ -n "${spec}" ]]; then
        cargo_args+=("--no-default-features" "--features" "${spec}")
    fi

    local sysroot_env="${ANDROID_SYSROOT:-}"
    local cc_env="CC_${rust_target//-/_}"
    local cflags_env="CFLAGS_${rust_target//-/_}"
    local env_prefix=("env" "${cc_env}=${CC}")
    if [[ -n "${sysroot_env}" ]]; then
        env_prefix+=("${cflags_env}=--sysroot=${sysroot_env} -I${MPV_PREFIX}/include")
    fi

    if "${env_prefix[@]}" cargo "${cargo_args[@]}" >> "${BUILD_LOG}" 2>&1; then
        log "✓ mpv-stt-plugin [${spec_desc}] for Android ${abi}"

        if [[ "${BUILD_MODE}" == "build" ]]; then
            local out_dir="${DIST_DIR}/android/${abi}/plugin"
            mkdir -p "${out_dir}"
            cp "${WORKSPACE_ROOT}/target/${rust_target}/release/libmpv_stt_plugin.so" \
               "${out_dir}/libmpv_stt_plugin.so"
        fi
        return 0
    else
        error "✗ mpv-stt-plugin [${spec_desc}] for Android ${abi} (see ${BUILD_LOG})"
        return 1
    fi
}

# Build a single desktop (non-Android) platform. `spec` is empty (default, both
# backends) or a single feature (single-backend build).
# --- Dynamic FFmpeg resolution (no source compile) -------------------------
#
# Desktop platforms link dynamically against a prebuilt FFmpeg instead of
# compiling one from source (the `build`/`static` ffmpeg-sys-next features are
# gone from Cargo.toml). These helpers resolve FFMPEG_DIR per platform:
#
#   darwin-*   brew FFmpeg prefix (FFPREFIX overrides; falls back to
#              `brew --prefix ffmpeg`, then pkg-config). The plugin depends on
#              the host's own dynamic libraries at their brew install paths.
#   linux-*    BtbN/FFmpeg-Builds linux64-lgpl-shared tarball, cached under
#              target/ffmpeg-btbn (FFMPEG_BTBN_URL overrides the auto-resolved
#              URL). Runtime .so files are copied into dist/<platform>/runtime.
#   windows-*  BtbN/FFmpeg-Builds win64-lgpl-shared zip, same cache; runtime
#              .dll files are copied into dist/<platform>/runtime.
#
# The resolved prefix is exported as FFMPEG_DIR, which ffmpeg-sys-next's
# prebuilt branch uses for both -L{dir}/lib and -I{dir}/include.

FFMPEG_CACHE_DIR="${WORKSPACE_ROOT}/target/ffmpeg-btbn"

# Download+extract a BtbN shared package into a per-platform cache dir, then
# export FFMPEG_DIR pointing at it. Reuses the cache on subsequent runs.
# The asset names are stable aliases, so no API lookup is needed:
#   https://github.com/BtbN/FFmpeg-Builds/releases/latest/download/<asset>
ensure_ffmpeg_btbn() {
    local platform="$1" suffix cache_dir archive top
    case "${platform}" in
        linux-x86_64)   suffix="linux64-lgpl-shared.tar.xz" ;;
        windows-x86_64) suffix="win64-lgpl-shared.zip" ;;
        *) error "ensure_ffmpeg_btbn: unsupported platform '${platform}'"; return 1 ;;
    esac
    cache_dir="${FFMPEG_CACHE_DIR}/${platform}"

    if [[ ! -f "${cache_dir}/.ready" ]]; then
        log "Downloading prebuilt FFmpeg package (${suffix})..."
        local asset_url
        asset_url="${FFMPEG_BTBN_URL:-https://github.com/BtbN/FFmpeg-Builds/releases/latest/download/ffmpeg-master-latest-${suffix}}"
        archive="${FFMPEG_CACHE_DIR}/${suffix}"
        mkdir -p "${FFMPEG_CACHE_DIR}"
        curl -fL --retry 3 -o "${archive}" "${asset_url}" \
            || { error "Failed to download ${asset_url}"; return 1; }
        rm -rf "${cache_dir}"
        mkdir -p "${cache_dir}"
        if [[ "${suffix}" == *.zip ]]; then
            unzip -q "${archive}" -d "${cache_dir}"
            # The zip's top-level dir is ffmpeg-master-latest-win64-lgpl-shared;
            # hoist it so the cache dir itself is the FFmpeg prefix.
            top="$(find "${cache_dir}" -mindepth 1 -maxdepth 1 -type d | head -1)"
            if [[ -n "${top}" && "${top}" != "${cache_dir}" ]]; then
                mv "${top}"/* "${cache_dir}"/ 2>/dev/null || true
                rmdir "${top}" 2>/dev/null || true
            fi
        else
            tar -xJf "${archive}" -C "${cache_dir}" --strip-components=1
        fi
        touch "${cache_dir}/.ready"
    fi

    export FFMPEG_DIR="${cache_dir}"
    log "FFMPEG_DIR=${FFMPEG_DIR}"
}

# Resolve the brew FFmpeg prefix on macOS (CI sets FFPREFIX explicitly).
ensure_ffmpeg_darwin() {
    local prefix="${FFPREFIX:-}"
    if [[ -z "${prefix}" ]]; then
        prefix="$(brew --prefix ffmpeg 2>/dev/null || true)"
    fi
    if [[ -z "${prefix}" ]]; then
        prefix="$(pkg-config --variable=prefix libavcodec 2>/dev/null || true)"
    fi
    if [[ -z "${prefix}" || ! -f "${prefix}/include/libavcodec/avcodec.h" ]]; then
        error "No brew FFmpeg found for macOS. Install it (brew install ffmpeg) or set FFPREFIX."
        return 1
    fi
    export FFMPEG_DIR="${prefix}"
    log "FFMPEG_DIR=${FFMPEG_DIR}"
}

ensure_ffmpeg() {
    local platform="$1"
    if [[ -n "${FFMPEG_DIR:-}" ]]; then
        log "Using caller-provided FFMPEG_DIR=${FFMPEG_DIR}"
        return 0
    fi
    case "${platform}" in
        darwin-*)   ensure_ffmpeg_darwin || return 1 ;;
        linux-*)    ensure_ffmpeg_btbn "${platform}" || return 1 ;;
        windows-*)  ensure_ffmpeg_btbn "${platform}" || return 1 ;;
        *) error "ensure_ffmpeg: no dynamic FFmpeg source for '${platform}'"; return 1 ;;
    esac
}

# Copy the FFmpeg runtime libraries next to the plugin so the shipped dist is
# self-contained (Windows needs the .dll on the DLL search path; Linux needs
# the .so available via LD_LIBRARY_PATH). macOS is skipped: the plugin links
# the brew dylibs at their absolute install paths.
collect_ffmpeg_runtime() {
    local platform="$1"
    # macOS: the plugin links the brew dylibs at their absolute install paths,
    # so there is nothing to bundle.
    if [[ "${platform}" == darwin-* ]]; then
        return 0
    fi
    [[ -n "${FFMPEG_DIR:-}" ]] || return 0
    local rt="${DIST_DIR}/${platform}/runtime"
    mkdir -p "${rt}"
    case "${platform}" in
        linux-x86_64)
            cp -P "${FFMPEG_DIR}"/lib/lib*.so* "${rt}"/ 2>/dev/null || true
            ;;
        windows-x86_64)
            cp "${FFMPEG_DIR}"/bin/*.dll "${rt}"/ 2>/dev/null || true
            ;;
    esac
    if [[ -z "$(ls -A "${rt}")" ]]; then
        warn "No FFmpeg runtime libraries collected for ${platform} (FFMPEG_DIR=${FFMPEG_DIR:-unset})."
    fi
}

build_desktop() {
    local platform="$1"
    local spec="$2"
    local target
    target="$(target_for_platform "$platform")" || { error "Unknown platform '${platform}'"; return 1; }

    local spec_desc
    if [[ -n "${spec}" ]]; then
        spec_desc="${spec}"
    else
        spec_desc="stt_ferrum,stt_openai"
    fi

    log "Building mpv-stt-plugin [${spec_desc}] for ${platform} (${target})..."

    local cargo_cmd="${BUILD_MODE}"
    local cargo_args=(
        "${cargo_cmd}"
        "--release"
        "--target" "${target}"
    )

    if [[ -n "${spec}" ]]; then
        cargo_args+=("--no-default-features" "--features" "${spec}")
    fi

    # Desktop links dynamically against a prebuilt FFmpeg (no source compile),
    # so cross-compiling only needs the target linker — warn so the user isn't
    # surprised by a missing one. (The old -march/-mtune/MSYS argument-conversion
    # hacks were specific to ffmpeg's source build and are gone with it.)
    local host_triple
    host_triple="$(host_rust_target)"
    if [[ "${target}" != "${host_triple}" ]]; then
        warn "Cross-compiling ${target} from host ${host_triple}; needs a matching linker/toolchain."
    fi

    # Resolve FFMPEG_DIR (brew on macOS, BtbN prebuilt on Linux/Windows).
    ensure_ffmpeg "${platform}" || return 1

    if cargo "${cargo_args[@]}" >> "${BUILD_LOG}" 2>&1; then
        log "✓ mpv-stt-plugin [${spec_desc}] for ${platform}"

        if [[ "${BUILD_MODE}" == "build" ]]; then
            local out_dir="${DIST_DIR}/${platform}/plugin"
            mkdir -p "${out_dir}"
            local art
            art="$(artifact_name "${target}")"
            cp "${WORKSPACE_ROOT}/target/${target}/release/${art}" "${out_dir}/${art}"
            # Ship the dynamic FFmpeg libraries alongside the plugin (Windows
            # .dll, Linux .so); macOS keeps the brew dylibs in place.
            collect_ffmpeg_runtime "${platform}"
        fi
        return 0
    else
        error "✗ mpv-stt-plugin [${spec_desc}] for ${platform} (see ${BUILD_LOG})"
        return 1
    fi
}

# Generate build manifest
generate_manifest() {
    local manifest="${DIST_DIR}/MANIFEST.txt"

    if [[ "${BUILD_MODE}" == "check" ]]; then
        log "Check mode: skipping manifest generation."
        return 0
    fi

    log "Generating build manifest..."

    {
        echo "MPV STT Build Artifacts"
        echo "Generated: $(date -Iseconds)"
        echo "Workspace: ${WORKSPACE_ROOT}"
        echo ""
        echo "=== Build Matrix ==="
        echo ""

        for platform in "${DESKTOP_PLATFORMS_ALL[@]}"; do
            echo "Platform: ${platform} ($(target_for_platform "$platform"))"

            if [[ -d "${DIST_DIR}/${platform}/plugin" ]]; then
                echo "  Plugin:"
                for f in "${DIST_DIR}/${platform}/plugin"/*; do
                    [[ -f "$f" ]] && echo "    - $(basename "$f") ($(du -h "$f" | cut -f1))"
                done
            fi

            echo ""
        done

        if [[ -d "${DIST_DIR}/android" ]]; then
            for abi_dir in "${DIST_DIR}/android"/*; do
                if [[ -d "$abi_dir" ]]; then
                    local abi=$(basename "$abi_dir")
                    echo "Platform: Android ${abi}"

                    if [[ -d "${abi_dir}/plugin" ]]; then
                        echo "  Plugin:"
                        for f in "${abi_dir}/plugin"/*; do
                            [[ -f "$f" ]] && echo "    - $(basename "$f") ($(du -h "$f" | cut -f1))"
                        done
                    fi

                    echo ""
                fi
            done
        fi
    } > "${manifest}"

    cat "${manifest}"
}

# Ensure needed Rust targets are installed
ensure_targets() {
    log "Ensuring Rust targets are installed..."

    for platform in "${DESKTOP_PLATFORMS[@]+"${DESKTOP_PLATFORMS[@]}"}"; do
        local target
        target="$(target_for_platform "$platform")" || continue
        ensure_rust_target "${target}"
    done

    if ((DO_ANDROID)); then
        for abi in "${SELECTED_ANDROID_ABIS[@]+"${SELECTED_ANDROID_ABIS[@]}"}"; do
            local spec
            spec="$(android_abi_spec "$abi")" || { error "Unknown Android ABI '${abi}'"; return 1; }
            IFS=":" read -r _abi _arch rust_target _clang_target <<<"${spec}"
            ensure_rust_target "${rust_target}"
        done
    fi
}

main() {
    ensure_setup_env

    parse_args "$@"
    set_defaults
    validate_inputs
    compute_platforms

    if ((SHOW_MATRIX)); then
        print_supported
        exit 0
    fi

    if [[ "${CLEAN_DIST}" -eq 1 ]]; then
        rm -rf "${DIST_DIR}"
    fi
    mkdir -p "${DIST_DIR}"
    : > "${BUILD_LOG}"

    log "==> Starting multi-platform build"
    log "Selected platforms : $(describe_array SELECTED_PLATFORMS "n/a")"
    log "Selected features  : $(describe_array SELECTED_FEATURES "both backends")"
    log "Android ABIs       : $(describe_array SELECTED_ANDROID_ABIS "default")"
    log "Clean dist         : ${CLEAN_DIST}"
    log "Mode               : ${BUILD_MODE}"

    check_env
    ensure_targets

    local specs=()
    get_build_specs specs

    local total=0
    local success=0
    local failed=0

    # Desktop builds (DESKTOP_PLATFORMS may be empty — e.g. android-only —
    # which under bash 3.2 + nounset expands to unbound; guard it).
    for platform in "${DESKTOP_PLATFORMS[@]+"${DESKTOP_PLATFORMS[@]}"}"; do
        for spec in "${specs[@]+"${specs[@]}"}"; do
            ((total++)) || true
            if build_desktop "${platform}" "${spec}"; then
                ((success++)) || true
            else
                ((failed++)) || true
            fi
        done
    done

    # Android builds
    if ((DO_ANDROID)); then
        log "Building Android targets..."
        for abi in "${SELECTED_ANDROID_ABIS[@]+"${SELECTED_ANDROID_ABIS[@]}"}"; do
            for spec in "${specs[@]+"${specs[@]}"}"; do
                ((total++)) || true
                if build_android_abi "${abi}" "${spec}"; then
                    ((success++)) || true
                else
                    ((failed++)) || true
                fi
            done
        done
    fi

    generate_manifest

    echo ""
    log "==> Build complete!"
    log "Total: ${total} | Success: ${success} | Failed: ${failed}"
    log "Artifacts: ${DIST_DIR}"
    log "Log: ${BUILD_LOG}"

    if [[ ${failed} -gt 0 ]]; then
        exit 1
    fi
}

main "$@"
