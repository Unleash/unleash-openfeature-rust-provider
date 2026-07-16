# Unleash OpenFeature Rust Provider

Rust OpenFeature provider backed by the Unleash Rust SDK.

## Build

```bash
cargo build
```

## Test

```bash
cargo test
```

## Test Harness

This repository includes the OpenFeature provider verifier as a git submodule.
After cloning, initialize it before running the full test suite:

```bash
git submodule update --init --recursive
```

If the verifier submodule is intentionally updated, refresh it and commit the
new submodule pointer:

```bash
git submodule update --remote --merge verifier
git status
```

## Example

```bash
export UNLEASH_API_URL=https://app.unleash-hosted.com/demo/api
export UNLEASH_APP_NAME=openfeature-example
export UNLEASH_INSTANCE_ID=openfeature-example
export UNLEASH_CLIENT_SECRET="$UNLEASH_API_KEY"

cargo run --example boolean_flag -- \
  --flag-key my-feature \
  --targeting-key user-123
```

The Unleash Rust client must be created with string feature lookup enabled:

```rust
use unleash_api_client::{ClientBuilder, EnvironmentConfig};
use unleash_openfeature_rust_provider::UnleashFlagProvider;

let config = EnvironmentConfig::from_env()?;
let provider = UnleashFlagProvider::new(
    ClientBuilder::default(),
    &config.api_url,
    &config.app_name,
    &config.instance_id,
    config.secret,
)?;
provider.initialize_client().await?;
```
