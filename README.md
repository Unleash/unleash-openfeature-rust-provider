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

## Example

```bash
cargo run --example boolean_flag -- \
  --url https://app.unleash-hosted.com/demo/api \
  --api-key "$UNLEASH_API_KEY" \
  --flag-key my-feature \
  --targeting-key user-123
```

The Unleash Rust client must be created with string feature lookup enabled:

```rust
use unleash_api_client::ClientBuilder;
use unleash_openfeature_rust_provider::{UnleashApiClient, UnleashFlagProvider};

let unleash_client = ClientBuilder::default()
    .enable_string_features()
    .into_client::<NoFeatures>(
        url,
        app_name,
        instance_id,
        Some(api_key),
    )?;

let provider = UnleashFlagProvider::new(UnleashApiClient::new(unleash_client));
provider.initialize_client().await?;
```
