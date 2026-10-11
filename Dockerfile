FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p cofre-server

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/cofre-server /usr/local/bin/cofre-server
ENV COFRE_LISTEN=0.0.0.0:8000 \
    COFRE_DATA=/data \
    COFRE_APPEND_ONLY=true
VOLUME ["/data"]
EXPOSE 8000
USER nonroot
ENTRYPOINT ["/usr/local/bin/cofre-server"]
