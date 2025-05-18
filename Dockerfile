FROM --platform=$TARGETPLATFORM debian:bookworm-slim
LABEL author="Robert Jansen" maintainer="me@rjns.dev"

ARG TARGETPLATFORM

COPY .docker/${TARGETPLATFORM#linux/}/wings-rs /app/server/wings-rs
COPY .docker/entrypoint.sh /entrypoint.sh

WORKDIR /app

RUN chmod +x /app/server/wings-rs

CMD [ "/bin/bash", "/entrypoint.sh" ]