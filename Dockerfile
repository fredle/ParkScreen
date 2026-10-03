FROM node:22-slim AS web
WORKDIR /w
COPY web/client/package*.json ./
RUN npm ci
COPY web/client/ ./
RUN npm run build

FROM rust:1-slim AS build
WORKDIR /b
COPY Cargo.toml Cargo.lock ./
COPY protocol protocol
COPY server server
COPY --from=web /w/dist web/client/dist
RUN cargo build --release -p parkscreen-server

FROM debian:bookworm-slim
COPY --from=build /b/target/release/parkscreen-server /usr/local/bin/
ENV LISTEN=0.0.0.0:8080
CMD ["parkscreen-server"]
