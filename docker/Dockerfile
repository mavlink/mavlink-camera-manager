from ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive

RUN apt update -y && \
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
        gnupg \
        htop

COPY ./target/x86_64-unknown-linux-gnu/release/mavlink-camera-manager /

ENTRYPOINT ./mavlink-camera-manager --mavlink=tcpout:192.168.2.2:5777 --verbose --reset
