FROM rust:1.99.1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 todo && useradd --uid 10001 --gid todo --no-create-home todo \
    && mkdir /data && chown todo:todo /data
COPY --from=build /build/target/release/rust-microsoft-todo-mcp /usr/local/bin/todo-mcp
USER 10001:10001
ENV TODO_BIND=0.0.0.0:8080 TODO_DATA_DIR=/data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s CMD curl --fail --silent http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["/usr/local/bin/todo-mcp"]
