FROM debian:bullseye-slim

ARG TARGETPLATFORM
ARG BINARY_PATH

RUN echo "Copying $TARGETPLATFORM/$BINARY_PATH into container"

COPY $TARGETPLATFORM/$BINARY_PATH hermes

RUN chmod a+x hermes

VOLUME data

# HTTP Port and P2P Port
EXPOSE 8000 9999

ENTRYPOINT ["/hermes", "--data-dir=/data", "--http-address=0.0.0.0:8000"]
