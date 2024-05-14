#!/usr/bin/env sh

cd $(git rev-parse --show-toplevel)

# Get cross-rs
cargo install cross

# Build MCM
cross build --release --target=x86_64-unknown-linux-gnu

# Build
docker build -t $USER/mavlink-camera-manager:$(git rev-parse HEAD) -f ./docker/Dockerfile .

# Run
docker run --rm -it --net=host $USER/mavlink-camera-manager:$(git rev-parse HEAD)
