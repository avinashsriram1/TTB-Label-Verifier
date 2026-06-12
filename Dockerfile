# syntax=docker/dockerfile:1

FROM node:20-bookworm AS ui-build
WORKDIR /ui
COPY crates/ttb-ui/package.json crates/ttb-ui/tsconfig.json ./
RUN npm install
COPY crates/ttb-ui/index.html ./index.html
COPY crates/ttb-ui/src ./src
RUN npm run build

FROM rust:1.96-bookworm AS rust-build
WORKDIR /app
COPY Cargo.toml ./
COPY crates ./crates
RUN cargo build --release -p ttb-api

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tesseract-ocr tesseract-ocr-eng \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=rust-build /app/target/release/ttb-api /app/ttb-api
COPY --from=ui-build /ui/dist /app/public
COPY samples /app/samples
ENV TTB_UI_DIR=/app/public
ENV PORT=8080
EXPOSE 8080
CMD ["/app/ttb-api"]

