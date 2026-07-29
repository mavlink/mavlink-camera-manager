#!/usr/bin/env bash
# Print env exports so cargo/cross can find a libcamera PREFIX.
# Usage: eval "$(ci/libcamera-env.sh [/path/to/prefix])"
set -euo pipefail

LIBCAMERA_VERSION=${LIBCAMERA_VERSION:-v0.7.1}
PREFIX=${1:-"${HOME}/.local/libcamera-${LIBCAMERA_VERSION}"}

if [[ ! -d "$PREFIX" ]]; then
    echo "libcamera prefix not found: $PREFIX" >&2
    exit 1
fi

PC_PATH="${PREFIX}/lib/pkgconfig"
for pc_dir in \
    "${PREFIX}/lib/x86_64-linux-gnu/pkgconfig" \
    "${PREFIX}/lib/aarch64-linux-gnu/pkgconfig" \
    "${PREFIX}/lib/arm-linux-gnueabihf/pkgconfig"; do
    if [[ -d "$pc_dir" ]]; then
        PC_PATH="${pc_dir}:${PC_PATH}"
    fi
done

LIB_PATH="${PREFIX}/lib"
for lib_dir in \
    "${PREFIX}/lib/x86_64-linux-gnu" \
    "${PREFIX}/lib/aarch64-linux-gnu" \
    "${PREFIX}/lib/arm-linux-gnueabihf"; do
    if [[ -d "$lib_dir" ]]; then
        LIB_PATH="${lib_dir}:${LIB_PATH}"
    fi
done

cat <<EOF
export LIBCAMERA_PREFIX=${PREFIX}
export PKG_CONFIG_PATH=${PC_PATH}\${PKG_CONFIG_PATH:+:\${PKG_CONFIG_PATH}}
export LIBRARY_PATH=${LIB_PATH}\${LIBRARY_PATH:+:\${LIBRARY_PATH}}
export LD_LIBRARY_PATH=${LIB_PATH}\${LD_LIBRARY_PATH:+:\${LD_LIBRARY_PATH}}
export BINDGEN_EXTRA_CLANG_ARGS="--sysroot=/ -I${PREFIX}/include \${BINDGEN_EXTRA_CLANG_ARGS:-}"
EOF
