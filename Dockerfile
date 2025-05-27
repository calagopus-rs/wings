# Build Stage
FROM curlimages/curl:latest AS build-env

WORKDIR /build

# curl
RUN mkdir -p curl_deps/usr/bin curl_deps/usr/lib curl_deps/lib /build/bin \
    && cp /usr/bin/curl curl_deps/usr/bin/ \
    && ldd /usr/bin/curl | awk '/=>/ {print $3}' | while read -r lib; do \
         case "$lib" in \
           /usr/lib/*) cp "$lib" "curl_deps/usr/lib/" ;; \
           /lib/*)     cp "$lib" "curl_deps/lib/" ;; \
         esac; \
       done

# btrfs
RUN curl -L https://github.com/kdave/btrfs-progs/releases/latest/download/btrfs.box.static \
      -o /build/bin/btrfs.box.static \
    && chmod +x /build/bin/btrfs.box.static \
    && ln -s btrfs.box.static /build/bin/btrfs

# wings-rs
RUN curl -L https://github.com/0x7d8/wings-rs/releases/latest/download/wings-rs-x86_64-linux \
      -o /build/bin/wings \
    && chmod +x /build/bin/wings

# Run Stage
FROM gcr.io/distroless/cc-debian12

# Copy over dependencies from build stage
COPY --from=build-env /build/curl_deps/ /
COPY --from=build-env /build/bin/ /usr/bin/

ENV LD_LIBRARY_PATH=/lib:/usr/lib

ENTRYPOINT ["/usr/bin/wings"]
