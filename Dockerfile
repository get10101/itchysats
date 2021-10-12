FROM debian:bullseye-slim

ARG BINARY_PATH
RUN echo "Copying $BINARY_PATH into container"

COPY $BINARY_PATH hermes

RUN chmod a+x hermes

VOLUME data

# HTTP Port and P2P Port
EXPOSE 8000 9999

ENTRYPOINT ["/hermes", "--data-dir=/data", "--http-address=0.0.0.0:8000"]
