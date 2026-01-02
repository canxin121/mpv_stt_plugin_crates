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
    (cd "${builder_dir}" && env -i \
        PATH="${PATH}" HOME="${HOME:-/tmp}" TERM="${TERM:-}" \
        ANDROID_MPV_WORK_DIR="${work_dir}" \
        ANDROID_MPV_PREFIX_BASE="${prefix_base}" \
        ANDROID_NDK_HOME="$(resolve_ndk)" \
        ANDROID_API="${api_default}" \
        ./buildall.sh --arch "${arch}" mpv)

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
    local toolchain_root="${ndk_path}/toolchains/llvm/prebuilt/$(host_tag)"
    if [[ ! -d "${toolchain_root}" ]]; then
        echo "Toolchain not found: ${toolchain_root}" >&2
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

ensure_setup_env() {
    if [[ -n "${SETUP_ENV_DONE:-}" ]]; then
        return
    fi

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

# Color output (bash printf with %(%H:%M:%S)T avoids external date)
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color
timestamp() { printf '%(%H:%M:%S)T' -1; }
log_core() { local lvl="$1"; shift; local msg="$*"; local line="${lvl}[$(timestamp)]${NC} ${msg}"; printf "%b\n" "${line}"; printf "%b\n" "${line}" >> "${BUILD_LOG}"; }
log()  { log_core "${GREEN}" "$*"; }
warn() { log_core "${YELLOW}[WARN]" "$*"; }
error(){ log_core "${RED}[ERROR]" "$*"; }

# Platform configurations
declare -A TARGETS=(
    ["linux-x86_64"]="x86_64-unknown-linux-gnu"
)

# Android ABI configurations (details resolved via android_abi_spec in setup-deps)
SUPPORTED_ANDROID_ABIS=("arm64-v8a" "armeabi-v7a" "x86" "x86_64")
DEFAULT_ANDROID_ABIS=("arm64-v8a" "armeabi-v7a")

# Feature configurations
PLUGIN_FEATURES=("stt_local_cpu" "stt_local_cuda" "stt_remote_http")
SERVER_FEATURES=("stt_local_cpu" "stt_local_cuda")

# CLI selections (populated by parse_args)
SELECTED_PLATFORMS=()
SELECTED_CRATES=()
SELECTED_FEATURES=()
SELECTED_ANDROID_ABIS=()
CLEAN_DIST=0
SHOW_MATRIX=0
LINUX_PLATFORMS=()
DO_ANDROID=0
BUILD_MODE="build" # or "check"

usage() {
    cat <<'EOUSAGE'
Usage: ./scripts/build-all.sh [options]

Build the mpv STT plugin/server across platforms. Defaults to the full matrix.

Options:
  -p, --platform   <list>   Comma-separated platforms (linux-x86_64, android)
  -c, --crate      <list>   Comma-separated crates (mpv-stt-plugin, mpv-stt-server)
  -f, --feature    <list>   Comma-separated features to build
  -a, --abi        <list>   Comma-separated Android ABIs (arm64-v8a, armeabi-v7a, x86, x86_64)
      --check               Run cargo check instead of building artifacts
      --clean               Remove dist/ before building (default: keep)
  -l, --list               Show supported values and exit
  -h, --help               Show this help and exit

Examples:
  # Only build Linux plugin with remote feature
  ./scripts/build-all.sh -p linux-x86_64 -c mpv-stt-plugin -f stt_remote_http

  # Build Android arm64 & armv7 CPU feature only
  ./scripts/build-all.sh -p android -a arm64-v8a,armeabi-v7a -f stt_local_cpu
EOUSAGE
}

print_supported() {
    echo "Supported platforms : ${!TARGETS[*]} android"
    echo "Supported crates    : mpv-stt-plugin mpv-stt-server"
    echo "Plugin features     : ${PLUGIN_FEATURES[*]}"
    echo "Server features     : ${SERVER_FEATURES[*]}"
    echo "Android ABIs        : ${SUPPORTED_ANDROID_ABIS[*]}"
}

append_list() {
    local -n _arr="$1"
    IFS=',' read -ra parts <<<"$2"
    for p in "${parts[@]}"; do
        [[ -n "${p}" ]] && _arr+=("${p}")
    done
}

dedup_array() {
    local -n arr="$1"
    declare -A seen=()
    local deduped=()
    for item in "${arr[@]}"; do
        if [[ -n "${item}" && -z "${seen[$item]:-}" ]]; then
            deduped+=("${item}")
            seen[$item]=1
        fi
    done
    arr=("${deduped[@]}")
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -p|--platform)
                [[ $# -lt 2 ]] && { echo "ERROR: --platform requires a value" >&2; exit 1; }
                append_list SELECTED_PLATFORMS "$2"
                shift 2
                ;;
            -c|--crate)
                [[ $# -lt 2 ]] && { echo "ERROR: --crate requires a value" >&2; exit 1; }
                append_list SELECTED_CRATES "$2"
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
        SELECTED_PLATFORMS=("${!TARGETS[@]}" "android")
    fi
    if [[ ${#SELECTED_CRATES[@]} -eq 0 ]]; then
        SELECTED_CRATES=("mpv-stt-plugin" "mpv-stt-server")
    fi
    if [[ ${#SELECTED_ANDROID_ABIS[@]} -eq 0 ]]; then
        SELECTED_ANDROID_ABIS=("${DEFAULT_ANDROID_ABIS[@]}")
    fi

    dedup_array SELECTED_PLATFORMS
    dedup_array SELECTED_CRATES
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

    for p in "${SELECTED_PLATFORMS[@]}"; do
        if [[ "$p" != "android" && -z "${TARGETS[$p]:-}" ]]; then
            echo "ERROR: Unknown platform '${p}'" >&2
            ok=0
        fi
    done

    for c in "${SELECTED_CRATES[@]}"; do
        case "$c" in
            mpv-stt-plugin|mpv-stt-server) ;;
            *) echo "ERROR: Unknown crate '${c}'" >&2; ok=0 ;;
        esac
    done

    for a in "${SELECTED_ANDROID_ABIS[@]}"; do
        if ! is_supported_android_abi "$a"; then
            echo "ERROR: Unknown Android ABI '${a}'" >&2
            ok=0
        fi
    done

    local all_features=("${PLUGIN_FEATURES[@]}" "${SERVER_FEATURES[@]}")
    for f in "${SELECTED_FEATURES[@]}"; do
        local found=0
        for af in "${all_features[@]}"; do
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
    LINUX_PLATFORMS=()
    DO_ANDROID=0

    for p in "${SELECTED_PLATFORMS[@]}"; do
        if [[ "$p" == "android" ]]; then
            DO_ANDROID=1
        elif [[ -n "${TARGETS[$p]:-}" ]]; then
            LINUX_PLATFORMS+=("$p")
        fi
    done
}

describe_array() {
    local -n arr="$1"
    local fallback="$2"
    if [[ ${#arr[@]} -eq 0 ]]; then
        echo "${fallback}"
    else
        echo "${arr[*]}"
    fi
}

get_features() {
    local crate="$1"
    local out_var="$2"
    local -n out="$out_var"

    local allowed_ref
    if [[ "${crate}" == "mpv-stt-plugin" ]]; then
        allowed_ref="PLUGIN_FEATURES"
    else
        allowed_ref="SERVER_FEATURES"
    fi

    local -n allowed="$allowed_ref"
    out=()

    if [[ ${#SELECTED_FEATURES[@]} -eq 0 ]]; then
        out=("${allowed[@]}")
        return
    fi

    for f in "${SELECTED_FEATURES[@]}"; do
        local matched=0
        for a in "${allowed[@]}"; do
            if [[ "$f" == "$a" ]]; then
                matched=1
                break
            fi
        done
        if [[ $matched -eq 1 ]]; then
            out+=("$f")
        fi
    done

    dedup_array out
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

# Build Android plugin for a specific ABI using android-mpv toolchain helpers
build_android_abi() {
    local abi="$1"
    local feature="$2"

    local spec
    if ! spec="$(android_abi_spec "$abi")"; then
        error "Unknown Android ABI '${abi}'"
        return 1
    fi

    IFS=":" read -r _abi arch rust_target clang_target <<<"${spec}"

    # Prepare env + deps (builds mpv/ffmpeg prefix on demand via scripts/android-mpv)
    if ! setup_android_env "$abi"; then
        error "Failed to set Android env for ${abi}"
        return 1
    fi

    log "Building mpv-stt-plugin [${feature}] for Android ${abi} (${rust_target})..."

    local cargo_cmd="${BUILD_MODE}"

    local cargo_args=(
        "${cargo_cmd}"
        "--release"
        "-p" "mpv-stt-plugin"
        "--target" "${rust_target}"
    )

    if [[ "${feature}" != "stt_local_cpu" ]]; then
        cargo_args+=("--features" "${feature}" "--no-default-features")
    fi

    local sysroot_env="${ANDROID_SYSROOT:-}"
    local cc_env="CC_${rust_target//-/_}"
    local cflags_env="CFLAGS_${rust_target//-/_}"
    local env_prefix=("env" "${cc_env}=${CC}")
    if [[ -n "${sysroot_env}" ]]; then
        env_prefix+=("${cflags_env}=--sysroot=${sysroot_env} -I${MPV_PREFIX}/include")
    fi

    if "${env_prefix[@]}" cargo "${cargo_args[@]}" >> "${BUILD_LOG}" 2>&1; then
        log "✓ mpv-stt-plugin [${feature}] for Android ${abi}"

        if [[ "${BUILD_MODE}" == "build" ]]; then
            local feature_suffix
            case "${feature}" in
                stt_local_cpu) feature_suffix="cpu" ;;
                stt_remote_http) feature_suffix="remote" ;;
                *) feature_suffix="${feature}" ;;
            esac

            local out_dir="${DIST_DIR}/android/${abi}/plugin"
            mkdir -p "${out_dir}"
            cp "${WORKSPACE_ROOT}/target/${rust_target}/release/libmpv_stt_plugin.so" \
               "${out_dir}/libmpv_stt_plugin_${feature_suffix}.so"
        fi
        return 0
    else
        error "✗ mpv-stt-plugin [${feature}] for Android ${abi} (see ${BUILD_LOG})"
        return 1
    fi
}

# Build a single configuration (Linux only)
build_artifact() {
    local platform="$1"
    local crate="$2"
    local feature="$3"
    local target="${TARGETS[$platform]}"

    log "Building ${crate} [${feature}] for ${platform}..."

    local cargo_cmd="${BUILD_MODE}"
    local cargo_args=(
        "${cargo_cmd}"
        "--release"
        "-p" "${crate}"
        "--target" "${target}"
    )

    if [[ "${feature}" != "stt_local_cpu" ]]; then
        cargo_args+=("--features" "${feature}" "--no-default-features")
    fi

    if cargo "${cargo_args[@]}" >> "${BUILD_LOG}" 2>&1; then
        log "✓ ${crate} [${feature}] for ${platform}"
        return 0
    else
        error "✗ ${crate} [${feature}] for ${platform} (see ${BUILD_LOG})"
        return 1
    fi
}

# Copy artifacts to dist directory
copy_artifact() {
    local platform="$1"
    local crate="$2"
    local feature="$3"
    local target="${TARGETS[$platform]}"

    local src_dir="${WORKSPACE_ROOT}/target/${target}/release"
    local dest_base="${DIST_DIR}/${platform}"

    if [[ "${BUILD_MODE}" == "check" ]]; then
        return 0
    fi

    if [[ "${crate}" == "mpv-stt-plugin" ]]; then
        local src="${src_dir}/libmpv_stt_plugin.so"
        local dest_dir="${dest_base}/plugin"
        local feature_suffix

        case "${feature}" in
            stt_local_cpu) feature_suffix="cpu" ;;
            stt_local_cuda) feature_suffix="cuda" ;;
            stt_remote_http) feature_suffix="remote" ;;
            *) feature_suffix="${feature}" ;;
        esac

        mkdir -p "${dest_dir}"
        cp "${src}" "${dest_dir}/libmpv_stt_plugin_${feature_suffix}.so"

    elif [[ "${crate}" == "mpv-stt-server" ]]; then
        local src="${src_dir}/mpv-stt-server"
        local dest_dir="${dest_base}/server"
        local feature_suffix

        case "${feature}" in
            stt_local_cpu) feature_suffix="cpu" ;;
            stt_local_cuda) feature_suffix="cuda" ;;
            *) feature_suffix="${feature}" ;;
        esac

        mkdir -p "${dest_dir}"
        cp "${src}" "${dest_dir}/mpv-stt-server_${feature_suffix}"
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

        for platform in "${!TARGETS[@]}"; do
            echo "Platform: ${platform} (${TARGETS[$platform]})"

            if [[ -d "${DIST_DIR}/${platform}/plugin" ]]; then
                echo "  Plugin:"
                for f in "${DIST_DIR}/${platform}/plugin"/*; do
                    [[ -f "$f" ]] && echo "    - $(basename "$f") ($(du -h "$f" | cut -f1))"
                done
            fi

            if [[ -d "${DIST_DIR}/${platform}/server" ]]; then
                echo "  Server:"
                for f in "${DIST_DIR}/${platform}/server"/*; do
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

    for platform in "${LINUX_PLATFORMS[@]}"; do
        local target="${TARGETS[$platform]}"
        if ! rustup target list | grep -q "${target} (installed)"; then
            log "Installing target: ${target}"
            rustup target add "${target}"
        fi
    done

    if ((DO_ANDROID)); then
        for abi in "${SELECTED_ANDROID_ABIS[@]}"; do
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
    log "Selected crates    : $(describe_array SELECTED_CRATES "n/a")"
    log "Selected features  : $(describe_array SELECTED_FEATURES "all")"
    log "Android ABIs       : $(describe_array SELECTED_ANDROID_ABIS "default")"
    log "Clean dist         : ${CLEAN_DIST}"
    log "Mode               : ${BUILD_MODE}"

    check_env
    ensure_targets

    local total=0
    local success=0
    local failed=0

    # Linux builds
    for platform in "${LINUX_PLATFORMS[@]}"; do
        for crate in "${SELECTED_CRATES[@]}"; do
            if [[ "${crate}" == "mpv-stt-server" && "${platform}" != "linux-x86_64" ]]; then
                warn "Skipping ${crate} on ${platform} (not supported)"
                continue
            fi

            local features=()
            get_features "${crate}" features

            if [[ ${#features[@]} -eq 0 ]]; then
                warn "No valid features selected for ${crate}; skipping"
                continue
            fi

            for feature in "${features[@]}"; do
                ((total++)) || true
                        if build_artifact "${platform}" "${crate}" "${feature}"; then
                            [[ "${BUILD_MODE}" == "build" ]] && copy_artifact "${platform}" "${crate}" "${feature}"
                            ((success++)) || true
                        else
                            ((failed++)) || true
                        fi
            done
        done
    done

    # Android builds
    if ((DO_ANDROID)); then
        if ! is_in_array "mpv-stt-plugin" "${SELECTED_CRATES[@]}"; then
            warn "Skipping Android builds: mpv-stt-plugin not selected"
        else
            log "Building Android targets..."
            local android_features=()
            get_features "mpv-stt-plugin" android_features

            if [[ ${#android_features[@]} -eq 0 ]]; then
                warn "No valid features selected for Android plugin; skipping"
            else
                for abi in "${SELECTED_ANDROID_ABIS[@]}"; do
                    for feature in "${android_features[@]}"; do
                        if [[ "${feature}" == "stt_local_cuda" ]]; then
                            warn "Skipping feature ${feature} for Android (unsupported)"
                            continue
                        fi

                        ((total++)) || true
                        if build_android_abi "${abi}" "${feature}"; then
                            ((success++)) || true
                        else
                            ((failed++)) || true
                        fi
                    done
                done
            fi
        fi
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
