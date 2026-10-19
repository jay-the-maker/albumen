# Multi-stage build for albumen-mcp.
# Search shells out to ExifTool, so the runtime image installs the same
# pinned Image-ExifTool 13.59 release as CI, not a distro package.

FROM rust:1.99.1-bookworm AS build

WORKDIR /src
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY ingest ./ingest
COPY cli ./cli
COPY mcp ./mcp
COPY receive ./receive
COPY bench ./bench
COPY win ./win
RUN cargo build --release --locked -p albumen-mcp

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl perl \
    && rm -rf /var/lib/apt/lists/*
COPY scripts/install-exiftool.sh /tmp/install-exiftool.sh
RUN dir="$(bash /tmp/install-exiftool.sh)" \
    && rm -rf /opt/Image-ExifTool \
    && mv "$dir" /opt/Image-ExifTool \
    && ln -sf /opt/Image-ExifTool/exiftool /usr/local/bin/exiftool \
    && exiftool -ver \
    && rm -rf /tmp/install-exiftool.sh /root/.cache
COPY --from=build /src/target/release/albumen-mcp /usr/local/bin/albumen-mcp

LABEL io.modelcontextprotocol.server.name="io.github.jay-the-maker/albumen"
RUN mkdir -p /library
WORKDIR /library
ENTRYPOINT ["albumen-mcp","--library","/library"]
