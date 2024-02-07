from ubuntu:22.04

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
        htop

# We need libssl.so.1.1
# Note: if the link breaks, find it here: http://nz2.archive.ubuntu.com/ubuntu/pool/main/o/openssl/?C=M;O=D
RUN wget http://nz2.archive.ubuntu.com/ubuntu/pool/main/o/openssl/libssl1.1_1.1.1f-1ubuntu2.21_amd64.deb && \
    dpkg -i libssl1.1_1.1.1f-1ubuntu2.21_amd64.deb ; \
    rm -rf libssl1.1_1.1.1f-1ubuntu2.21_amd64.deb

COPY ./target/x86_64-unknown-linux-gnu/release/mavlink-camera-manager /

ENTRYPOINT ./mavlink-camera-manager --mavlink=tcpout:192.168.2.2:5777 --verbose --reset
