# curl
FROM curlimages/curl:latest AS curl-builder

WORKDIR /build

# Gather Dependencies
RUN mkdir -p curl_deps/usr/bin curl_deps/usr/lib curl_deps/lib /build/bin \
    && cp /usr/bin/curl curl_deps/usr/bin/ \
    && ldd /usr/bin/curl | awk '/=>/ {print $3}' | while read -r lib; do \
         case "$lib" in \
           /usr/lib/*) cp "$lib" "curl_deps/usr/lib/" ;; \
           /lib/*)     cp "$lib" "curl_deps/lib/" ;; \
         esac; \
       done

# btrfs
FROM alpine:latest AS btrfs-builder
WORKDIR /build

RUN apk add --no-cache build-base linux-headers util-linux-dev lzo-dev zlib-dev zstd-dev jq curl automake autoconf

# Build
RUN tag=$(curl -s https://api.github.com/repos/kdave/btrfs-progs/releases/latest | jq -r '.tag_name') && \
    curl -O -L -f -S --retry 3 --retry-delay 2 --connect-timeout 30 --max-time 300 https://github.com/kdave/btrfs-progs/archive/refs/tags/$tag.tar.gz && \
    tar -xf $tag.tar.gz && \
    cd btrfs-progs-${tag#v} && \
    ./autogen.sh && \
    ./configure --disable-backtrace --disable-documentation --disable-convert --disable-libudev --disable-python && \
    make btrfs.box && \
    mv btrfs.box ../btrfs && \
    chmod +x ../btrfs

# Gather dependencies
RUN mkdir -p btrfs_deps/usr/bin btrfs_deps/usr/lib btrfs_deps/lib \
    && cp btrfs btrfs_deps/usr/bin/ \
    && ldd btrfs | awk '/=>/ {print $3}' | while read -r lib; do \
         case "$lib" in \
           /usr/lib/*) cp "$lib" "btrfs_deps/usr/lib/" ;; \
           /lib/*)     cp "$lib" "btrfs_deps/lib/" ;; \
         esac; \
       done

# runner
FROM gcr.io/distroless/cc-debian12

COPY --from=curl-builder /build/curl_deps/ /
COPY --from=btrfs-builder /build/btrfs_deps/ /

# wings-rs
ARG TARGETPLATFORM
COPY .docker/${TARGETPLATFORM#linux/}/wings-rs /usr/bin/wings-rs
COPY .docker/entrypoint.sh /entrypoint.sh

ENV LD_LIBRARY_PATH=/lib:/usr/lib
ENTRYPOINT ["/usr/bin/wings-rs"]
