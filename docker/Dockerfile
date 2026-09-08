# syntax=docker/dockerfile:1
FROM rust:slim-trixie AS builder

WORKDIR /usr/src/mavlink-camera-manager

# Install build dependencies
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt update -y && \
    apt install -y --no-install-recommends \
        libunwind-dev \
        libclang-dev \
        pkg-config \
        build-essential \
        curl \
        gnupg \
        ca-certificates \
        git \
        libmount-dev \
        libsepol-dev \
        libselinux1-dev \
        libglib2.0-dev \
        libgudev-1.0-dev \
        gstreamer1.0-tools \
        gstreamer1.0-nice \
        libgstreamer1.0-dev \
        libgstreamer-plugins-base1.0-dev \
        libgstreamer-plugins-bad1.0-dev \
        libgstrtspserver-1.0-dev \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/* /tmp/* /var/tmp/*


# Install Node.js (v20) and Yarn for frontend build (per README)
RUN set -eux; \
    curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key | gpg --dearmor -o /usr/share/keyrings/nodesource.gpg; \
    echo "deb [signed-by=/usr/share/keyrings/nodesource.gpg] https://deb.nodesource.com/node_20.x nodistro main" \
        > /etc/apt/sources.list.d/nodesource.list; \
    curl -fsSL https://dl.yarnpkg.com/debian/pubkey.gpg | gpg --dearmor -o /usr/share/keyrings/yarnkey.gpg; \
    echo "deb [signed-by=/usr/share/keyrings/yarnkey.gpg] https://dl.yarnpkg.com/debian stable main" \
        > /etc/apt/sources.list.d/yarn.list; \
    apt-get update -y; \
    apt-get install -y --no-install-recommends nodejs yarn; \
    rm -rf /var/lib/apt/lists/*

# Copy source and build release binary

ENV PATH="/usr/local/cargo/bin:${PATH}"

COPY --link . .
# Build bindings and the project using cache for the cargo registry/git and the target dir.
# This speeds up subsequent builds by reusing downloaded crates and intermediate build artifacts.
RUN --mount=type=cache,id=cargo-registry,target=/root/.cargo/registry \
    --mount=type=cache,id=cargo-git,target=/root/.cargo/git \
    --mount=type=cache,id=cargo-target,target=/usr/src/mavlink-camera-manager/target \
    cargo run --package=bindings --release --target=x86_64-unknown-linux-gnu \
    && cargo build --release --target=x86_64-unknown-linux-gnu \
    && cargo build --release --bin mavlink-camera-manager

    ### Final stage: runtime image with only needed libs and binary
FROM ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive

RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt update -y && \
    apt install -y --no-install-recommends \
        libgstreamer1.0-0 \
        libgstreamer-plugins-base1.0-0 \
        libgstreamer-plugins-bad1.0-0 \
        libgstrtspserver-1.0-0 \
        gstreamer1.0-x \
        gstreamer1.0-nice \
        gstreamer1.0-libav \
        gstreamer1.0-plugins-ugly \
        wget \
        ca-certificates \
        htop \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/* /tmp/* /var/tmp/*

# Copy binary from builder
COPY --from=builder /usr/src/mavlink-camera-manager/target/release/mavlink-camera-manager /

ENTRYPOINT ["./mavlink-camera-manager", "--mavlink=tcpout:192.168.2.2:5777", "--verbose", "--reset"]
