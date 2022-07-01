# Run jaeger for viewing traces when debugging locally
docker run \
  -p 5778:5778 \
  -p 16686:16686 \
  -p 13133:13133 \
  -p 4317:55680 \
  jaegertracing/opentelemetry-all-in-one:latest
