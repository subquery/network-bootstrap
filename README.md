# Network bootstrap service

## develop mode

``` shell
export USE_TESTNET_QUERY=true
cargo build; cargo run
```

## Deploy

### Steps

- `vim .env` configure and edit
- run `cargo run --release`
