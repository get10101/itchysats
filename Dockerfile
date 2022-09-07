FROM gcr.io/distroless/cc

ENV ITCHYSATS_ENV=docker

USER 1000

LABEL "org.opencontainers.image.source"="https://github.com/itchysats/itchysats"
LABEL "org.opencontainers.image.authors"="hello@itchysats.network"

ARG BINARY_PATH

COPY $BINARY_PATH /usr/bin/binary

VOLUME /data

# HTTP Port and libp2p P2P Port
EXPOSE 8000 10000

ENTRYPOINT ["/usr/bin/binary", "--data-dir=/data", "--http-address=0.0.0.0:8000"]
