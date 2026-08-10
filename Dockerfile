FROM rust:1.88-alpine@sha256:9dfaae478ecd298b6b5a039e1f2cc4fc040fc818a2de9aa78fa714dea036574d AS builder

WORKDIR /src

RUN apk add --no-cache musl-dev

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --locked --release && \
    cp target/release/resilis-status-canary /resilis-status-canary

FROM scratch

USER 65532:65532
ENV PORT=8080
EXPOSE 8080

COPY --from=builder /resilis-status-canary /resilis-status-canary

ENTRYPOINT ["/resilis-status-canary"]
