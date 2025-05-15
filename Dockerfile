FROM --platform=$TARGETPLATFORM alpine:latest
LABEL author="Robert Jansen" maintainer="me@rjns.dev"

ARG TARGETPLATFORM

COPY .docker/${TARGETPLATFORM#linux/}/wings-rs /app/bin

WORKDIR /app

RUN chmod +x /app/bin

CMD [ "/bin/ash", "/app/bin" ]