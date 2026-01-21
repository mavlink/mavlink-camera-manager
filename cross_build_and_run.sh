#!/usr/bin/env bash
set -euo pipefail

REMOTE_USER="pi"
REMOTE_HOST="192.168.2.2"
REMOTE_PASS="raspberry"
CONTAINER="blueos-core"
TARGET="armv7-unknown-linux-gnueabihf"
BINARY_NAME="mavlink-camera-manager"

mkdir -p frontend/dist

SKIP_WEB=1 cross build --release --target "$TARGET"

BINARY="target/${TARGET}/release/mavlink-camera-manager"

sshpass -p "$REMOTE_PASS" scp -o StrictHostKeyChecking=no \
    "$BINARY" "${REMOTE_USER}@${REMOTE_HOST}:/tmp/${BINARY_NAME}"

sshpass -p "$REMOTE_PASS" ssh -o StrictHostKeyChecking=no \
    "${REMOTE_USER}@${REMOTE_HOST}" \
    "docker cp /tmp/${BINARY_NAME} ${CONTAINER}:/root/${BINARY_NAME} && \
     docker exec ${CONTAINER} chmod +x /root/${BINARY_NAME}"

sshpass -p "$REMOTE_PASS" ssh -o StrictHostKeyChecking=no \
    "${REMOTE_USER}@${REMOTE_HOST}" \
    "docker exec ${CONTAINER} tmux send-keys -t video C-c"

sshpass -p "$REMOTE_PASS" ssh -o StrictHostKeyChecking=no \
    "${REMOTE_USER}@${REMOTE_HOST}" \
    "docker exec ${CONTAINER} tmux send-keys -t video \
     '/root/${BINARY_NAME} --default-settings BlueROVUDP --mavlink tcpout:127.0.0.1:5777 --mavlink-system-id 1 --mavlink-camera-component-id-range=100-105 --gst-feature-rank omxh264enc=0,v4l2h264enc=250,x264enc=260 --log-path /var/logs/blueos/services/mavlink-camera-manager --stun-server stun://stun.l.google.com:19302 --enable-realtime-threads --recording-path=/usr/blueos/userdata/mavlink-camera-manager/recordings --verbose' Enter"

echo "\nDone. Binary deployed and running in tmux session 'video' inside container ${CONTAINER}"
