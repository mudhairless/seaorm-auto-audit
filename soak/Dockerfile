# Builds the soak binaries (api, writer, verify). Build context is the repository root, so the crate
# itself is part of the build, exactly as a consumer would get it.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --manifest-path soak/Cargo.toml --bins \
        --target-dir /build-target \
    && mkdir /out \
    && for b in api writer verify; do \
         if [ -f "/build-target/release/$b" ]; then cp "/build-target/release/$b" /out/; fi; \
       done \
    && ls -la /out

FROM debian:bookworm-slim
COPY --from=build /out/ /usr/local/bin/
CMD ["api"]
