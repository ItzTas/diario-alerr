FROM --platform=$BUILDPLATFORM rust:1.99.1-trixie AS build

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG ZIG_VERSION=0.17.0
ARG CARGO_ZIGBUILD_VERSION=0.24.0
ARG BUILDARCH
RUN case "$BUILDARCH" in \
      amd64) arch=x86_64 \
        zig_sha256=1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026 \
        zigbuild_sha256=3ca25ee4348f9abd0932ed4debe826d246e20fb616d0201de5bd05e7f965e9d9 ;; \
      arm64) arch=aarch64 \
        zig_sha256=9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8 \
        zigbuild_sha256=de42626985f428033078dfc12ac8a59ee713f8e5627235ec883b1c899bee895c ;; \
      *) echo "unsupported build platform: $BUILDARCH" >&2; exit 1 ;; \
    esac \
    && zigbuild="cargo-zigbuild-${arch}-unknown-linux-gnu" \
    && curl -fsSLo /tmp/zig.tar.xz "https://ziglang.org/download/${ZIG_VERSION}/zig-${arch}-linux-${ZIG_VERSION}.tar.xz" \
    && curl -fsSLo /tmp/zigbuild.tar.xz "https://github.com/rust-cross/cargo-zigbuild/releases/download/v${CARGO_ZIGBUILD_VERSION}/${zigbuild}.tar.xz" \
    && echo "${zig_sha256}  /tmp/zig.tar.xz" | sha256sum -c - \
    && echo "${zigbuild_sha256}  /tmp/zigbuild.tar.xz" | sha256sum -c - \
    && mkdir /opt/zig \
    && tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1 \
    && tar -xJf /tmp/zigbuild.tar.xz -C /usr/local/cargo/bin --strip-components=1 "${zigbuild}/cargo-zigbuild" \
    && rm /tmp/zig.tar.xz /tmp/zigbuild.tar.xz
ENV PATH="/opt/zig:${PATH}"

WORKDIR /src

COPY rust-toolchain.toml ./
RUN rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl

COPY Cargo.toml Cargo.lock ./
RUN mkdir src && touch src/lib.rs && cargo fetch --locked

COPY src src
COPY sql sql

ARG TARGETARCH
RUN case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-musl ;; \
      arm64) target=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported target platform: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && cargo zigbuild --release --locked --target "$target" \
    && mkdir /out \
    && cp "target/${target}/release/diario-alerr" /out/diario-alerr

FROM scratch

COPY plugin.json /plugin/plugin.json
COPY --from=build /out/diario-alerr /plugin/diario-alerr

USER 65532:65532

HEALTHCHECK NONE

ENTRYPOINT ["/plugin/diario-alerr"]
