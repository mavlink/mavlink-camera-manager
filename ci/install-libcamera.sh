#!/usr/bin/env bash
# Build and install libcamera into PREFIX for cargo (libcamera-native).
#
# Env:
#   LIBCAMERA_VERSION  git tag (default: v0.7.1)
#   PREFIX             install prefix (default: $HOME/.local/libcamera-$LIBCAMERA_VERSION)
#   JOBS               parallel build jobs (default: nproc)
set -euo pipefail

LIBCAMERA_VERSION=${LIBCAMERA_VERSION:-v0.7.1}
PREFIX=${PREFIX:-"${HOME}/.local/libcamera-${LIBCAMERA_VERSION}"}
JOBS=${JOBS:-"$(nproc)"}
SRC_DIR=${SRC_DIR:-"${HOME}/.cache/libcamera-src-${LIBCAMERA_VERSION}"}
MARKER="${PREFIX}/.mcm-libcamera-installed"
REPO_URL=${REPO_URL:-"https://github.com/raspberrypi/libcamera.git"}

export PATH="${PREFIX}/bin:${PATH}"
export PKG_CONFIG_PATH="${PREFIX}/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
# Prefer multiarch pkgconfig layout after install.
for pc_dir in \
    "${PREFIX}/lib/x86_64-linux-gnu/pkgconfig" \
    "${PREFIX}/lib/aarch64-linux-gnu/pkgconfig" \
    "${PREFIX}/lib/arm-linux-gnueabihf/pkgconfig"; do
    if [[ -d "$pc_dir" ]]; then
        export PKG_CONFIG_PATH="${pc_dir}:${PKG_CONFIG_PATH}"
    fi
done

if [[ -f "$MARKER" ]] && pkg-config --exists libcamera; then
    echo "libcamera ${LIBCAMERA_VERSION} already installed at ${PREFIX} ($(pkg-config --modversion libcamera))"
    exit 0
fi

echo "Installing libcamera ${LIBCAMERA_VERSION} into ${PREFIX}"

if [[ "${EUID}" -eq 0 ]]; then
    APT=(apt-get)
else
    APT=(sudo apt-get)
fi

export DEBIAN_FRONTEND=noninteractive
"${APT[@]}" update -qq
"${APT[@]}" install -y -qq \
    build-essential git pkg-config \
    meson ninja-build cmake \
    python3-pip python3-yaml python3-ply python3-jinja2 \
    libyaml-dev libssl-dev openssl \
    libudev-dev libdw-dev libunwind-dev \
    libglib2.0-dev

# Ubuntu 22.04 system meson can be older than libcamera's requirement; prefer pip.
python3 -m pip install --user -q 'meson>=1.0.1'
export PATH="${HOME}/.local/bin:${PATH}"

rm -rf "${SRC_DIR}"
git clone --depth 1 --branch "${LIBCAMERA_VERSION}" "${REPO_URL}" "${SRC_DIR}"

meson setup "${SRC_DIR}/build" "${SRC_DIR}" \
    --prefix="${PREFIX}" \
    --buildtype=release \
    -Dandroid=disabled \
    -Ddocumentation=disabled \
    -Dgstreamer=disabled \
    -Dqcam=disabled \
    -Dcam=disabled \
    -Dlc-compliance=disabled \
    -Dtest=false \
    -Dv4l2=true

meson compile -C "${SRC_DIR}/build" -j "${JOBS}"
meson install -C "${SRC_DIR}/build"

# Ensure pkg-config can find the install without multiarch surprises.
mkdir -p "${PREFIX}/lib/pkgconfig"
shopt -s nullglob
for pc in "${PREFIX}"/lib/*/pkgconfig/libcamera*.pc "${PREFIX}"/lib/pkgconfig/libcamera*.pc; do
    cp -f "$pc" "${PREFIX}/lib/pkgconfig/"
done
shopt -u nullglob

# Rewrite prefix in .pc files to the actual PREFIX (meson may embed the path already).
sed -i "s|^prefix=.*|prefix=${PREFIX}|" "${PREFIX}/lib/pkgconfig"/libcamera*.pc

pkg-config --exists libcamera
echo "${LIBCAMERA_VERSION}" >"${MARKER}"
echo "Installed libcamera $(pkg-config --modversion libcamera) (cflags: $(pkg-config --cflags libcamera))"
