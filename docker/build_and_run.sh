#!/usr/bin/env sh

cd $(git rev-parse --show-toplevel)

# Build MCM
cross build --release --target=x86_64-unknown-linux-gnu

# Build
docker build -t $USER/mavlink-camera-manager:$(git rev-parse HEAD) -f ./docker/Dockerfile .

# Run
docker run -it --net=host $USER/mavlink-camera-manager:$(git rev-parse HEAD)
