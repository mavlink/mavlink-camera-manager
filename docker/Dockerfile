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

# We need libssl.so.1.1
# Note: if the link breaks, find it here: https://packages.ubuntu.com/focal/amd64/libssl1.1/download
RUN wget http://security.ubuntu.com/ubuntu/pool/main/o/openssl/libssl1.1_1.1.1f-1ubuntu2.22_amd64.deb -O /tmp/openssl.deb && \
dpkg -i /tmp/openssl.deb

COPY ./target/x86_64-unknown-linux-gnu/release/mavlink-camera-manager /

ENTRYPOINT ./mavlink-camera-manager --mavlink=tcpout:192.168.2.2:5777 --verbose --reset
