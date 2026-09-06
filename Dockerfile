FROM rust:bookworm AS builder

WORKDIR /src

COPY . .

RUN git --version \
    && git rev-parse --verify HEAD \
    && cargo test --workspace --locked
RUN cargo build --release --locked --bin sat-tracker \
    && mkdir /runtime \
    && cp target/release/sat-tracker /runtime/

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder --chown=65532:65532 /runtime /app

WORKDIR /app

EXPOSE 8080

ENTRYPOINT ["./sat-tracker"]
