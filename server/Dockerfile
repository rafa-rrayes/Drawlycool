# Drawlycool sync server.
#   docker run -d -p 4180:4180 -v drawlycool:/data ghcr.io/rafa-rrayes/drawlycool-server
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
RUN cargo build --release --locked --bin drawlycool-server \
 && mkdir /data

FROM gcr.io/distroless/cc-debian12:nonroot
LABEL org.opencontainers.image.title="Drawlycool server" \
      org.opencontainers.image.description="Sync server for Drawlycool shared canvases" \
      org.opencontainers.image.source="https://github.com/rafa-rrayes/Drawlycool"
COPY --from=build /src/target/release/drawlycool-server /usr/local/bin/drawlycool-server
# Owned by the image's non-root user, so a fresh named volume is writable.
COPY --from=build --chown=nonroot:nonroot /data /data
VOLUME /data
EXPOSE 4180
ENTRYPOINT ["/usr/local/bin/drawlycool-server", "--data", "/data"]
