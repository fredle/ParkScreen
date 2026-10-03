# Signalling server for Cloud Run. The web client is hosted separately on Firebase Hosting.
FROM rust:1-slim AS build
WORKDIR /b
COPY Cargo.toml Cargo.lock ./
COPY protocol protocol
COPY server server
# The workspace lists host/, which is not part of this image.
RUN sed -i 's/, "host"//' Cargo.toml && cargo build --release -p parkscreen-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /b/target/release/parkscreen-server /usr/local/bin/
ENV STORE=firestore
# Cloud Run supplies PORT.
CMD ["parkscreen-server"]
