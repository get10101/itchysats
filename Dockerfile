FROM debian:bullseye-slim

LABEL org.opencontainers.image.source https://github.com/itchysats/itchysats

ARG TARGETPLATFORM
ARG BINARY_PATH

COPY $TARGETPLATFORM/$BINARY_PATH /usr/bin/binary

VOLUME data

# HTTP Port and P2P Port
EXPOSE 8000 9999

ENTRYPOINT ["/usr/bin/binary", "--data-dir=/data", "--http-address=0.0.0.0:8000"]
