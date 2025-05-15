FROM --platform=$TARGETPLATFORM alpine:latest
LABEL author="Robert Jansen" maintainer="me@rjns.dev"

ARG TARGETPLATFORM

COPY .docker/${TARGETPLATFORM#linux/}/wings-rs /app/server/bin
COPY .docker/entrypoint.sh /entrypoint.sh

WORKDIR /app

RUN chmod +x /app/server/bin

CMD [ "/bin/ash", "/entrypoint.sh" ]