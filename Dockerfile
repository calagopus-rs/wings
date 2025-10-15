FROM alpine:latest

# Copy gathered binaries and libs
COPY --from=builder /build/gathered/ /

RUN apk add --no-cache ca-certificates coreutils curl btrfs-progs xfsprogs-extra zfs restic && \
	update-ca-certificates

# Add wings-rs and entrypoint
ARG TARGETPLATFORM
COPY .docker/${TARGETPLATFORM#linux/}/wings-rs /usr/bin/wings-rs

ENTRYPOINT ["/usr/bin/wings-rs"]
