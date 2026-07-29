#!/usr/bin/env bash
# Build a libcamera install prefix for a foreign arch via Docker buildx.
#
# Usage:
#   ci/build-libcamera-sysroot.sh <linux/amd64|linux/arm64|linux/arm/v7> <output.tar.gz>
set -euo pipefail

PLATFORM=${1:?platform required, e.g. linux/arm64}
OUT_TAR=${2:?output tar.gz path required}
LIBCAMERA_VERSION=${LIBCAMERA_VERSION:-v0.7.1}
REPO_URL=${REPO_URL:-"https://github.com/raspberrypi/libcamera.git"}
IMAGE=${IMAGE:-debian:bookworm-slim}

mkdir -p "$(dirname "$OUT_TAR")"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Building libcamera ${LIBCAMERA_VERSION} for ${PLATFORM} into ${OUT_TAR}"

docker run --rm --platform "${PLATFORM}" \
    -v "${TMP_DIR}:/out" \
    -e LIBCAMERA_VERSION="${LIBCAMERA_VERSION}" \
    -e REPO_URL="${REPO_URL}" \
    "${IMAGE}" \
    bash -lc '
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq \
  build-essential git pkg-config \
  meson ninja-build cmake \
  python3-pip python3-yaml python3-ply python3-jinja2 \
  libyaml-dev libssl-dev openssl \
  libudev-dev libdw-dev libunwind-dev \
  libglib2.0-dev
python3 -m pip install -q "meson>=1.0.1"
export PATH="/root/.local/bin:${PATH}"

PREFIX=/opt/libcamera
SRC=/tmp/libcamera-src
git clone --depth 1 --branch "${LIBCAMERA_VERSION}" "${REPO_URL}" "${SRC}"
meson setup "${SRC}/build" "${SRC}" \
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
meson compile -C "${SRC}/build" -j"$(nproc)"
meson install -C "${SRC}/build"

mkdir -p "${PREFIX}/lib/pkgconfig"
shopt -s nullglob
for pc in "${PREFIX}"/lib/*/pkgconfig/libcamera*.pc "${PREFIX}"/lib/pkgconfig/libcamera*.pc; do
  cp -f "$pc" "${PREFIX}/lib/pkgconfig/"
done
sed -i "s|^prefix=.*|prefix=${PREFIX}|" "${PREFIX}/lib/pkgconfig"/libcamera*.pc
# Archive prefix contents (not the opt/ parent) so extract dir == PREFIX.
tar -C "${PREFIX}" -czf /out/libcamera-prefix.tar.gz .
'

cp "${TMP_DIR}/libcamera-prefix.tar.gz" "${OUT_TAR}"
ls -lh "${OUT_TAR}"
