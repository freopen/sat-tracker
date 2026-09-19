FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS dev
RUN chmod 1777 /tmp
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git libssl-dev openssh-client \
        gh jq pkg-config procps sudo vim \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash --uid 1000 vscode \
    && mkdir /src \
    && chown vscode:vscode /src \
    && echo 'vscode ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/vscode \
    && chmod 0440 /etc/sudoers.d/vscode

COPY --from=ghcr.io/j178/prek:latest@sha256:f27d17a6b21959c5ba7d65039d72e9502c6951e79cbbfae2c1a0498a4859a5cb /prek /usr/local/bin/prek

COPY --from=rhysd/actionlint:1.7.12@sha256:b1934ee5f1c509618f2508e6eb47ee0d3520686341fec936f3b79331f9315667 /usr/local/bin/actionlint /usr/local/bin/actionlint

# Use the static binary so this also works on Debian Bookworm and ARM64.
ARG RELEASE_PLZ_VERSION=0.3.169
RUN case "$(uname -m)" in \
        x86_64) release_plz_sha=ed709642b7f5b5fda4d47309884e65f84ca097cf9176cfd9793e8e66e28b48ad ;; \
        aarch64) release_plz_sha=9fe32973a63bf1d18f02e877becf8619abc8b283f25d294f00d951e55c9946f2 ;; \
        *) exit 1 ;; \
    esac \
    && curl -fsSL "https://github.com/release-plz/release-plz/releases/download/release-plz-v${RELEASE_PLZ_VERSION}/release-plz-$(uname -m)-unknown-linux-musl.tar.gz" -o /tmp/release-plz.tar.gz \
    && echo "${release_plz_sha}  /tmp/release-plz.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/release-plz.tar.gz -C /usr/local/bin release-plz \
    && rm /tmp/release-plz.tar.gz

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

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

COPY --from=builder --chown=65532:65532 /src/runtime /app

WORKDIR /app

EXPOSE 8080

ENTRYPOINT ["./sat-tracker"]
