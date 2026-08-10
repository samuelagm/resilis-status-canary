# resilis-status-canary

Private synthetic target for Resilis build and deployment status checks.

The service keeps a small, stable HTTP contract so build and deployment checks
can detect both a running service and the deployed canary value.

## HTTP contract

All responses include these headers:

```text
cache-control: no-store
content-type: text/plain; charset=utf-8
```

| Request | Status | Body |
| --- | ---: | --- |
| `GET /` | `200` | `Resilis deployment canary\n` |
| `GET /healthz` | `200` | `ok\n` |
| `GET /canary` | `200` | The exact contents of `src/canary.txt` |
| Any other path | `404` | `not found\n` |

The canary file is embedded in the executable at build time. The runtime image
does not need a writable filesystem or a copy of the source tree.

## Runtime

- The server uses only the Rust standard library. `Cargo.toml` has zero third-party dependencies.
- It binds to `0.0.0.0`.
- `PORT` selects the listen port. An unset or empty `PORT` uses `8080`. A non-empty value must be an integer from `0` through `65535`; invalid values stop startup with an error.
- Connections use two workers, a queue limited to eight pending connections, a small 64 KiB worker stack, and a five-second read/write timeout. Backpressure keeps slow clients from creating unbounded application threads.
- Release settings enable size optimization, link-time optimization, one codegen unit, panic abort, and symbol stripping.
- The production Dockerfile builds a statically linked musl release binary and runs it as UID/GID `65532` in a `scratch` image.

## Existing deployment migration

Cloud Core persists the build strategy and start command for a deployment. A
configured start command has the highest Railpack precedence. This repository
change alone does not update that persisted deployment configuration.

For an existing Node deployment, select the Dockerfile build strategy with
path `Dockerfile`, clear the old Node build/start command, and redeploy. If the
old command remains configured, Railpack can continue to select it instead of
running this Rust container.

## Local build and test

The project requires only Rust and Cargo:

```sh
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
PORT=8080 cargo run
```

In another terminal:

```sh
curl -i http://127.0.0.1:8080/
curl -i http://127.0.0.1:8080/healthz
curl -i http://127.0.0.1:8080/canary
```

`cargo metadata --no-deps --format-version 1` can verify the zero-dependency
manifest. The release binary can be built with `cargo build --release`.

## Container build

```sh
docker build -t resilis-status-canary .
docker run --rm --publish 8080:8080 resilis-status-canary
```

The `scratch` image has no shell or HTTP client. Use an external probe or the
deployment platform's HTTP health check against `/healthz`.
