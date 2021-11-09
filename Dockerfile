FROM debian:bullseye-slim

LABEL org.opencontainers.image.source https://github.com/itchysats/itchysats

ARG TARGETPLATFORM
ARG BINARY_PATH

RUN echo "Copying $TARGETPLATFORM/$BINARY_PATH into container"

COPY $TARGETPLATFORM/$BINARY_PATH binary

RUN chmod a+x binary

VOLUME data

# HTTP Port and P2P Port
EXPOSE 8000 9999

ENTRYPOINT ["/binary", "--data-dir=/data", "--http-address=0.0.0.0:8000"]
