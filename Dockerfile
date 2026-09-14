FROM debian:bookworm-slim AS dev
RUN chmod 1777 /tmp
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git libssl-dev openssh-client \
        pkg-config procps sudo vim \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash --uid 1000 vscode \
    && mkdir /src \
    && chown vscode:vscode /src \
    && echo 'vscode ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/vscode \
    && chmod 0440 /etc/sudoers.d/vscode

COPY --from=ghcr.io/j178/prek:latest /prek /usr/local/bin/prek

USER vscode
ENV CARGO_HOME=/home/vscode/.cargo \
    RUSTUP_HOME=/home/vscode/.rustup
ENV PATH="/home/vscode/.cargo/bin:${PATH}"

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain stable \
        --component rustfmt,clippy,rust-src \
    && rm /tmp/rustup-init.sh \
    && rustc --version \
    && cargo --version \
    && prek --version

WORKDIR /home/vscode
CMD ["sleep", "infinity"]

FROM dev AS checks

WORKDIR /src

COPY --chown=vscode:vscode . .

RUN git --version \
    && git rev-parse --verify HEAD \
    && make check

FROM checks AS builder

RUN cargo build --release --locked --bin sat-tracker \
    && mkdir /src/runtime \
    && cp target/release/sat-tracker /src/runtime/

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder --chown=65532:65532 /src/runtime /app

WORKDIR /app

EXPOSE 8080

ENTRYPOINT ["./sat-tracker"]
