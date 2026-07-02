use std::{error::Error, io, time::Duration};

use open_feature::EvaluationContext;
use open_feature::provider::FeatureProvider;
use tokio::time::sleep;
use unleash_api_client::ClientBuilder;
use unleash_api_client::client::FeatureKey;
use unleash_openfeature_rust_provider::{UnleashApiClient, UnleashFlagProvider};

#[derive(Clone, Copy, Debug)]
enum NoFeatureBounds {}

impl FeatureKey for NoFeatureBounds {
    fn name(self) -> &'static str {
        match self {}
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let args = Args::parse()?;

    let unleash_client = ClientBuilder::default()
        .enable_string_features()
        .into_client::<NoFeatureBounds>(
            &args.url,
            &args.app_name,
            &args.instance_id,
            Some(args.api_key),
        )?;

    let provider = UnleashFlagProvider::new(UnleashApiClient::new(unleash_client));
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
    url: String,
    api_key: String,
    flag_key: String,
    app_name: String,
    instance_id: String,
    targeting_key: Option<String>,
}

impl Args {
    fn parse() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut url = None;
        let mut api_key = None;
        let mut flag_key = None;
        let mut app_name = "openfeature-example".to_string();
        let mut instance_id = "openfeature-example".to_string();
        let mut targeting_key = None;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--url" => url = args.next(),
                "--api-key" => api_key = args.next(),
                "--flag-key" => flag_key = args.next(),
                "--app-name" => app_name = required_value("--app-name", args.next())?,
                "--instance-id" => instance_id = required_value("--instance-id", args.next())?,
                "--targeting-key" => targeting_key = args.next(),
                _ => return Err(format!("unknown argument: {arg}").into()),
            }
        }

        Ok(Self {
            url: required_value("--url", url)?,
            api_key: required_value("--api-key", api_key)?,
            flag_key: required_value("--flag-key", flag_key)?,
            app_name,
            instance_id,
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
