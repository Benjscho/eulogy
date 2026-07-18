# Library POC

A worked example of a library that uses `eulogy` without picking a runtime,
then consuming it from two downstreams — one on `tokio`, one on `smol`.
This demonstrates how library authors can use `AsyncDrop` in a runtime
agnostic way.

## Layout

```
library-poc/
├── session-lib/    library — depends on eulogy with no runtime feature,
│                   re-exposes `tokio` and `smol` as passthrough features
├── tokio-app/      binary — enables session-lib/tokio, runs on #[tokio::main]
└── smol-app/       binary — enables session-lib/smol, runs on smol::block_on
```

## Run it

```sh
cd tokio-app && cargo run
# [session-lib] opening session 1
# [tokio-app] using session 1
# [session-lib] closing session 1

cd smol-app && cargo run
# [session-lib] opening session 1
# [smol-app] using session 1
# [session-lib] closing session 1
```

