use std::{error::Error, io, time::Duration};

use open_feature::EvaluationContext;
use open_feature::provider::FeatureProvider;
use tokio::time::sleep;
use unleash_api_client::{ClientBuilder, EnvironmentConfig};
use unleash_openfeature_rust_provider::UnleashFlagProvider;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let config = EnvironmentConfig::from_env()?;
    let args = Args::parse()?;

    let provider = UnleashFlagProvider::new(
        ClientBuilder::default(),
        &config.api_url,
        &config.app_name,
        &config.instance_id,
        config.secret,
    )?;
    provider.initialize_client().await?;
    sleep(Duration::from_millis(500)).await;

    let context = args
        .targeting_key
        .map(|targeting_key| EvaluationContext::default().with_targeting_key(targeting_key))
        .unwrap_or_default();

    let details = provider
        .resolve_bool_value(&args.flag_key, &context)
        .await
        .map_err(|error| io::Error::other(format!("{error:?}")))?;

    println!("{}={}", args.flag_key, details.value);
    println!("reason={:?}", details.reason);

    provider.shutdown().await;

    Ok(())
}

struct Args {
    flag_key: String,
    targeting_key: Option<String>,
}

impl Args {
    fn parse() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut flag_key = None;
        let mut targeting_key = None;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--flag-key" => flag_key = args.next(),
                "--targeting-key" => targeting_key = args.next(),
                _ => return Err(format!("unknown argument: {arg}").into()),
            }
        }

        Ok(Self {
            flag_key: required_value("--flag-key", flag_key)?,
            targeting_key,
        })
    }
}

fn required_value(
    name: &'static str,
    value: Option<String>,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    value.ok_or_else(|| format!("missing required argument: {name}").into())
}
