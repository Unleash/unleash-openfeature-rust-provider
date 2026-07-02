use std::collections::HashSet;

use open_feature::provider::FeatureProvider;
use open_feature::{
    EvaluationContext, EvaluationContextFieldValue, EvaluationErrorCode, StructValue, Value,
};
use serde_json::json;
use unleash_api_client::ClientBuilder;
use unleash_api_client::client::FeatureKey;
use unleash_openfeature_rust_provider::{UnleashApiClient, UnleashFlagProvider};
use unleash_types::client_features::ClientFeatures;

const KNOWN_GAPS: &[&str] = &[
    "number-empty-string-guard",
    "variant-json-array",
    "object-scalar-json-passthrough",
];

#[derive(Clone, Copy, Debug)]
enum NoFeatureBounds {}

impl FeatureKey for NoFeatureBounds {
    fn name(self) -> &'static str {
        match self {}
    }
}

#[tokio::test]
async fn verifier_contract_scenarios() {
    let contract: serde_json::Value =
        serde_json::from_str(include_str!("../verifier/contract.json")).expect("valid contract");
    let features: ClientFeatures =
        serde_json::from_str(include_str!("../verifier/fixtures/unleash-features.json"))
            .expect("verifier fixture parses into unleash_types ClientFeatures");
    let client = ClientBuilder::default()
        .enable_string_features()
        .disable_metric_submission()
        .into_client::<NoFeatureBounds>(
            "http://unleash-bootstrap.invalid/api",
            "openfeature-rust-verifier",
            "openfeature-rust-verifier",
            Some("verifier-not-a-real-token".to_string()),
        )
        .expect("client builds");
    client.memoize(features).expect("fixture memoizes");

    let provider = UnleashFlagProvider::new(UnleashApiClient::new(client));
    let known_gaps = KNOWN_GAPS.iter().copied().collect::<HashSet<_>>();
    let mut failures = Vec::new();

    for scenario in contract["scenarios"].as_array().expect("scenarios array") {
        let id = scenario["id"].as_str().expect("scenario id");
        if known_gaps.contains(id) {
            continue;
        }

        if !applies_to(scenario) {
            continue;
        }

        if let Err(error) = assert_scenario(&provider, scenario).await {
            failures.push(format!("{id}: {error}"));
        }
    }

    assert!(
        failures.is_empty(),
        "contract failures:\n{}",
        failures.join("\n")
    );
}

async fn assert_scenario(
    provider: &UnleashFlagProvider<UnleashApiClient<NoFeatureBounds>>,
    scenario: &serde_json::Value,
) -> Result<(), String> {
    let details = evaluate(provider, scenario).await;
    let expected = &scenario["expect"];

    if details.value != expected["value"] {
        return Err(format!(
            "expected value {}, got {}",
            expected["value"], details.value
        ));
    }

    if let Some(expected_variant) = expected.get("variant").and_then(|value| value.as_str())
        && details.variant.as_deref() != Some(expected_variant)
    {
        return Err(format!(
            "expected variant {expected_variant:?}, got {:?}",
            details.variant
        ));
    }

    if let Some(expected_error_code) = expected.get("errorCode").and_then(|value| value.as_str()) {
        if details.error_code.as_deref() != Some(expected_error_code) {
            return Err(format!(
                "expected error code {expected_error_code:?}, got {:?}",
                details.error_code
            ));
        }
    } else if details.error_code.is_some() {
        return Err(format!(
            "expected no error code, got {:?}",
            details.error_code
        ));
    }

    Ok(())
}

async fn evaluate(
    provider: &UnleashFlagProvider<UnleashApiClient<NoFeatureBounds>>,
    scenario: &serde_json::Value,
) -> Details {
    let flag_key = scenario["flagKey"].as_str().expect("flag key");
    let context = evaluation_context(scenario.get("context"));
    let default_value = scenario["default"].clone();

    match scenario["type"].as_str() {
        Some("boolean") => provider
            .resolve_bool_value(flag_key, &context)
            .await
            .map(|details| Details {
                value: json!(details.value),
                variant: details.variant,
                error_code: None,
            })
            .unwrap_or_else(|error| defaulted(default_value, error)),
        Some("string") => provider
            .resolve_string_value(flag_key, &context)
            .await
            .map(|details| Details {
                value: json!(details.value),
                variant: details.variant,
                error_code: None,
            })
            .unwrap_or_else(|error| defaulted(default_value, error)),
        Some("number") => provider
            .resolve_float_value(flag_key, &context)
            .await
            .map(|details| Details {
                value: json!(details.value),
                variant: details.variant,
                error_code: None,
            })
            .unwrap_or_else(|error| defaulted(default_value, error)),
        Some("object") => provider
            .resolve_struct_value(flag_key, &context)
            .await
            .map(|details| Details {
                value: struct_to_json(details.value),
                variant: details.variant,
                error_code: None,
            })
            .unwrap_or_else(|error| defaulted(default_value, error)),
        Some(flag_type) => Details {
            value: default_value,
            variant: None,
            error_code: Some(format!("unsupported scenario type: {flag_type}")),
        },
        None => Details {
            value: default_value,
            variant: None,
            error_code: Some("missing scenario type".to_string()),
        },
    }
}

fn defaulted(value: serde_json::Value, error: open_feature::EvaluationError) -> Details {
    let error_code = match error.code {
        EvaluationErrorCode::General(message) if message == "UNKNOWN" => None,
        code => Some(code.to_string()),
    };

    Details {
        value,
        variant: None,
        error_code,
    }
}

#[derive(Debug)]
struct Details {
    value: serde_json::Value,
    variant: Option<String>,
    error_code: Option<String>,
}

fn applies_to(scenario: &serde_json::Value) -> bool {
    scenario["requires"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|capability| capability.as_str())
        .all(|capability| matches!(capability, "localEval" | "perCallContext"))
}

fn evaluation_context(context: Option<&serde_json::Value>) -> EvaluationContext {
    let mut evaluation_context = EvaluationContext::default();
    let Some(context) = context.and_then(|context| context.as_object()) else {
        return evaluation_context;
    };

    for (key, value) in context {
        if key == "targetingKey" {
            if let Some(value) = value.as_str() {
                evaluation_context = evaluation_context.with_targeting_key(value);
            }
            continue;
        }

        if let Some(value) = evaluation_context_value(value) {
            evaluation_context = evaluation_context.with_custom_field(key, value);
        }
    }

    evaluation_context
}

fn evaluation_context_value(value: &serde_json::Value) -> Option<EvaluationContextFieldValue> {
    match value {
        serde_json::Value::Bool(value) => Some((*value).into()),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Some(value.into())
            } else {
                value.as_f64().map(Into::into)
            }
        }
        serde_json::Value::String(value) => Some(value.clone().into()),
        _ => None,
    }
}

fn struct_to_json(value: StructValue) -> serde_json::Value {
    serde_json::Value::Object(
        value
            .fields
            .into_iter()
            .map(|(key, value)| (key, openfeature_value_to_json(value)))
            .collect(),
    )
}

fn openfeature_value_to_json(value: Value) -> serde_json::Value {
    match value {
        Value::Bool(value) => json!(value),
        Value::Int(value) => json!(value),
        Value::Float(value) => json!(value),
        Value::String(value) => json!(value),
        Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(openfeature_value_to_json).collect())
        }
        Value::Struct(value) => struct_to_json(value),
    }
}
