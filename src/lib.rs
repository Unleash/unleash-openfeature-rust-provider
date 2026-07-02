//! OpenFeature provider for Unleash.

use std::{collections::HashMap, error::Error, sync::Arc};

use log::debug;
use open_feature::async_trait;
use open_feature::provider::{FeatureProvider, ProviderMetadata, ResolutionDetails};
use open_feature::{
    EvaluationContext, EvaluationContextFieldValue, EvaluationError, EvaluationErrorCode,
    EvaluationReason, EvaluationResult, StructValue, Value,
};
use unleash_api_client::Context as UnleashContext;
use unleash_api_client::client::{FeatureKey, Variant};

type BoxError = Box<dyn Error + Send + Sync + 'static>;

const BASE_CONTEXT_KEYS: [&str; 6] = [
    "currentTime",
    "userId",
    "sessionId",
    "remoteAddress",
    "environment",
    "appName",
];

#[async_trait]
pub trait UnleashClient: Send + Sync + 'static {
    /// Initialize the client. Implementations must make this idempotent.
    async fn initialize(&self) -> Result<(), BoxError>;

    /// Shut down the client. Implementations must make this idempotent.
    async fn shutdown(&self);

    /// Resolve a boolean flag.
    fn is_enabled(&self, flag_key: &str, context: Option<&UnleashContext>, default: bool) -> bool;

    /// Resolve a variant by flag key.
    fn get_variant(&self, flag_key: &str, context: &UnleashContext) -> Variant;
}

pub struct UnleashApiClient<F>
where
    F: FeatureKey + Send + Sync,
{
    client: Arc<unleash_api_client::Client<F>>,
    poll_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl<F> UnleashApiClient<F>
where
    F: FeatureKey + Send + Sync,
{
    pub fn new(client: unleash_api_client::Client<F>) -> Self {
        Self {
            client: Arc::new(client),
            poll_task: tokio::sync::Mutex::new(None),
        }
    }
}

#[async_trait]
impl<F> UnleashClient for UnleashApiClient<F>
where
    F: FeatureKey + Send + Sync,
{
    async fn initialize(&self) -> Result<(), BoxError> {
        let mut poll_task = self.poll_task.lock().await;
        if poll_task.is_some() {
            return Ok(());
        }

        self.client.register().await?;

        let client = Arc::clone(&self.client);
        *poll_task = Some(tokio::spawn(async move {
            client.poll_for_updates().await;
        }));

        Ok(())
    }

    async fn shutdown(&self) {
        let mut poll_task = self.poll_task.lock().await;
        if let Some(poll_task) = poll_task.take() {
            self.client.stop_poll().await;
            poll_task.abort();
        }
    }

    fn is_enabled(&self, flag_key: &str, context: Option<&UnleashContext>, default: bool) -> bool {
        self.client.is_enabled_str(flag_key, context, default)
    }

    fn get_variant(&self, flag_key: &str, context: &UnleashContext) -> Variant {
        self.client.get_variant_str(flag_key, context)
    }
}

pub struct UnleashFlagProvider<C> {
    client: C,
    metadata: ProviderMetadata,
}

impl<C> UnleashFlagProvider<C>
where
    C: UnleashClient,
{
    pub fn new(client: C) -> Self {
        Self {
            client,
            metadata: ProviderMetadata::new("Unleash OpenFeature Provider"),
        }
    }

    /// Initialize provider-owned Unleash resources.
    pub async fn initialize_client(&self) -> Result<(), BoxError> {
        self.client.initialize().await
    }

    /// Shut down provider-owned Unleash resources.
    pub async fn shutdown(&self) {
        self.client.shutdown().await;
    }
}

#[async_trait]
impl<C> FeatureProvider for UnleashFlagProvider<C>
where
    C: UnleashClient,
{
    async fn initialize(&mut self, _context: &EvaluationContext) {
        if let Err(error) = self.initialize_client().await {
            log::warn!("failed to initialize Unleash client: {error}");
        }
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn resolve_bool_value(
        &self,
        flag_key: &str,
        evaluation_context: &EvaluationContext,
    ) -> EvaluationResult<ResolutionDetails<bool>> {
        let context = to_unleash_context(evaluation_context);
        let value = self.client.is_enabled(flag_key, Some(&context), false);
        Ok(resolution(value, None, EvaluationReason::Unknown))
    }

    async fn resolve_int_value(
        &self,
        flag_key: &str,
        evaluation_context: &EvaluationContext,
    ) -> EvaluationResult<ResolutionDetails<i64>> {
        self.resolve_variant_value(flag_key, evaluation_context, &["number"], parse_int_payload)
    }

    async fn resolve_float_value(
        &self,
        flag_key: &str,
        evaluation_context: &EvaluationContext,
    ) -> EvaluationResult<ResolutionDetails<f64>> {
        self.resolve_variant_value(
            flag_key,
            evaluation_context,
            &["number"],
            parse_float_payload,
        )
    }

    async fn resolve_string_value(
        &self,
        flag_key: &str,
        evaluation_context: &EvaluationContext,
    ) -> EvaluationResult<ResolutionDetails<String>> {
        self.resolve_variant_value(flag_key, evaluation_context, &["string", "csv"], |value| {
            Ok(value.to_string())
        })
    }

    async fn resolve_struct_value(
        &self,
        flag_key: &str,
        evaluation_context: &EvaluationContext,
    ) -> EvaluationResult<ResolutionDetails<StructValue>> {
        let context = to_unleash_context(evaluation_context);
        let variant = self.client.get_variant(flag_key, &context);
        let variant_name = Some(variant.name.clone());

        let payload_value = resolve_payload_value(&variant, &["json"])?;
        let value: serde_json::Value = serde_json::from_str(payload_value).map_err(|error| {
            evaluation_error(EvaluationErrorCode::ParseError, error.to_string())
        })?;
        let value = json_to_openfeature_value(value)?;

        match value {
            Value::Struct(value) => Ok(resolution(value, variant_name, EvaluationReason::Unknown)),
            Value::Array(_) => Err(evaluation_error(
                EvaluationErrorCode::TypeMismatch,
                "OpenFeature Rust provider API does not support top-level array object values",
            )),
            _ => Err(evaluation_error(
                EvaluationErrorCode::TypeMismatch,
                "Variant payload is not a JSON object",
            )),
        }
    }
}

impl<C> UnleashFlagProvider<C>
where
    C: UnleashClient,
{
    fn resolve_variant_value<T>(
        &self,
        flag_key: &str,
        evaluation_context: &EvaluationContext,
        payload_types: &[&str],
        convert: impl FnOnce(&str) -> EvaluationResult<T>,
    ) -> EvaluationResult<ResolutionDetails<T>> {
        let context = to_unleash_context(evaluation_context);
        let variant = self.client.get_variant(flag_key, &context);
        let variant_name = Some(variant.name.clone());
        let payload_value = resolve_payload_value(&variant, payload_types)?;
        let value = convert(payload_value)?;

        Ok(resolution(value, variant_name, EvaluationReason::Unknown))
    }
}

fn resolution<T>(
    value: T,
    variant: Option<String>,
    reason: EvaluationReason,
) -> ResolutionDetails<T> {
    ResolutionDetails {
        value,
        variant,
        reason: Some(reason),
        flag_metadata: None,
    }
}

fn resolve_payload_value<'a>(
    variant: &'a Variant,
    payload_types: &[&str],
) -> EvaluationResult<&'a str> {
    if !variant.enabled {
        return Err(evaluation_error(
            EvaluationErrorCode::General("UNKNOWN".to_string()),
            "Variant is disabled",
        ));
    }

    let payload_type = variant.payload.get("type").ok_or_else(|| {
        evaluation_error(
            EvaluationErrorCode::TypeMismatch,
            "Variant payload type is not present on the resolved variant",
        )
    })?;
    let payload_value = variant.payload.get("value").ok_or_else(|| {
        evaluation_error(
            EvaluationErrorCode::TypeMismatch,
            "Variant payload value is not present on the resolved variant",
        )
    })?;

    if !payload_types.contains(&payload_type.as_str()) {
        return Err(evaluation_error(
            EvaluationErrorCode::TypeMismatch,
            format!("Variant payload has type {payload_type:?}, expected one of {payload_types:?}"),
        ));
    }

    Ok(payload_value)
}

fn evaluation_error(code: EvaluationErrorCode, message: impl Into<String>) -> EvaluationError {
    EvaluationError {
        code,
        message: Some(message.into()),
    }
}

fn parse_int_payload(value: &str) -> EvaluationResult<i64> {
    value
        .parse::<i64>()
        .map_err(|error| evaluation_error(EvaluationErrorCode::ParseError, error.to_string()))
}

fn parse_float_payload(value: &str) -> EvaluationResult<f64> {
    value
        .parse::<f64>()
        .map_err(|error| evaluation_error(EvaluationErrorCode::ParseError, error.to_string()))
}

fn to_unleash_context(evaluation_context: &EvaluationContext) -> UnleashContext {
    let mut context = UnleashContext::default();

    for (key, value) in &evaluation_context.custom_fields {
        if BASE_CONTEXT_KEYS.contains(&key.as_str()) {
            set_base_context_value(&mut context, key, value);
            continue;
        }

        if let Some(value) = string_property_value(value) {
            context.properties.insert(key.clone(), value);
        } else {
            debug!("Discarding nested Unleash context property: {key}");
        }
    }

    if let Some(targeting_key) = &evaluation_context.targeting_key {
        context.user_id = Some(targeting_key.clone());
    }

    context
}

fn set_base_context_value(
    context: &mut UnleashContext,
    key: &str,
    value: &EvaluationContextFieldValue,
) {
    match key {
        "userId" => context.user_id = string_property_value(value),
        "sessionId" => context.session_id = string_property_value(value),
        "environment" => context.environment = string_property_value(value).unwrap_or_default(),
        "appName" => context.app_name = string_property_value(value).unwrap_or_default(),
        "remoteAddress" => {
            if let Some(remote_address) = value.as_str().and_then(|value| value.parse().ok()) {
                context.remote_address =
                    Some(unleash_api_client::context::IPAddress(remote_address));
            }
        }
        "currentTime" => {
            if let Some(current_time) = value.as_date_time()
                && let Some(current_time) = chrono::DateTime::from_timestamp(
                    current_time.unix_timestamp(),
                    current_time.nanosecond(),
                )
            {
                context.current_time = Some(current_time);
            }
        }
        _ => {}
    }
}

fn string_property_value(value: &EvaluationContextFieldValue) -> Option<String> {
    match value {
        EvaluationContextFieldValue::Bool(value) => Some(value.to_string()),
        EvaluationContextFieldValue::Int(value) => Some(value.to_string()),
        EvaluationContextFieldValue::Float(value) => Some(value.to_string()),
        EvaluationContextFieldValue::String(value) => Some(value.clone()),
        EvaluationContextFieldValue::DateTime(value) => Some(value.to_string()),
        EvaluationContextFieldValue::Struct(_) => None,
    }
}

fn json_to_openfeature_value(value: serde_json::Value) -> EvaluationResult<Value> {
    match value {
        serde_json::Value::Null => Err(evaluation_error(
            EvaluationErrorCode::TypeMismatch,
            "JSON null is not a supported OpenFeature object value",
        )),
        serde_json::Value::Bool(value) => Ok(Value::Bool(value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(Value::Int(value))
            } else if let Some(value) = value.as_f64() {
                Ok(Value::Float(value))
            } else {
                Err(evaluation_error(
                    EvaluationErrorCode::TypeMismatch,
                    "JSON number cannot be represented as i64 or f64",
                ))
            }
        }
        serde_json::Value::String(value) => Ok(Value::String(value)),
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(json_to_openfeature_value)
            .collect::<EvaluationResult<Vec<_>>>()
            .map(Value::Array),
        serde_json::Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| Ok((key, json_to_openfeature_value(value)?)))
            .collect::<EvaluationResult<HashMap<_, _>>>()
            .map(|fields| Value::Struct(StructValue { fields })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeClient {
        variants: HashMap<String, Variant>,
    }

    impl FakeClient {
        fn new(variants: impl IntoIterator<Item = (&'static str, Variant)>) -> Self {
            Self {
                variants: variants
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value))
                    .collect(),
            }
        }
    }

    #[async_trait]
    impl UnleashClient for FakeClient {
        async fn initialize(&self) -> Result<(), BoxError> {
            Ok(())
        }

        async fn shutdown(&self) {}

        fn is_enabled(
            &self,
            flag_key: &str,
            _context: Option<&UnleashContext>,
            default: bool,
        ) -> bool {
            self.variants
                .get(flag_key)
                .map(|variant| variant.enabled)
                .unwrap_or(default)
        }

        fn get_variant(&self, flag_key: &str, _context: &UnleashContext) -> Variant {
            self.variants.get(flag_key).cloned().unwrap_or_default()
        }
    }

    fn variant(payload_type: &str, payload_value: &str) -> Variant {
        Variant {
            name: "variant-a".to_string(),
            enabled: true,
            payload: HashMap::from([
                ("type".to_string(), payload_type.to_string()),
                ("value".to_string(), payload_value.to_string()),
            ]),
        }
    }

    #[tokio::test]
    async fn resolves_boolean_flag() {
        let provider =
            UnleashFlagProvider::new(FakeClient::new([("enabled", variant("string", ""))]));

        let details = provider
            .resolve_bool_value("enabled", &EvaluationContext::default())
            .await
            .unwrap();

        assert!(details.value);
    }

    #[tokio::test]
    async fn resolves_string_variant_payload() {
        let provider =
            UnleashFlagProvider::new(FakeClient::new([("string", variant("string", "hello"))]));

        let details = provider
            .resolve_string_value("string", &EvaluationContext::default())
            .await
            .unwrap();

        assert_eq!(details.value, "hello");
        assert_eq!(details.variant.as_deref(), Some("variant-a"));
    }

    #[tokio::test]
    async fn resolves_csv_variant_payload_as_string() {
        let provider =
            UnleashFlagProvider::new(FakeClient::new([("csv", variant("csv", "a,b,c"))]));

        let details = provider
            .resolve_string_value("csv", &EvaluationContext::default())
            .await
            .unwrap();

        assert_eq!(details.value, "a,b,c");
    }

    #[tokio::test]
    async fn resolves_integer_variant_payload() {
        let provider =
            UnleashFlagProvider::new(FakeClient::new([("integer", variant("number", "42"))]));

        let details = provider
            .resolve_int_value("integer", &EvaluationContext::default())
            .await
            .unwrap();

        assert_eq!(details.value, 42);
    }

    #[tokio::test]
    async fn resolves_float_variant_payload() {
        let provider =
            UnleashFlagProvider::new(FakeClient::new([("float", variant("number", "4.2"))]));

        let details = provider
            .resolve_float_value("float", &EvaluationContext::default())
            .await
            .unwrap();

        assert_eq!(details.value, 4.2);
    }

    #[tokio::test]
    async fn empty_number_payload_returns_parse_error() {
        let provider =
            UnleashFlagProvider::new(FakeClient::new([("float", variant("number", ""))]));

        let error = provider
            .resolve_float_value("float", &EvaluationContext::default())
            .await
            .unwrap_err();

        assert_eq!(error.code, EvaluationErrorCode::ParseError);
    }

    #[tokio::test]
    async fn resolves_json_object_payload() {
        let provider = UnleashFlagProvider::new(FakeClient::new([(
            "object",
            variant("json", r#"{"enabled":true,"count":3}"#),
        )]));

        let details = provider
            .resolve_struct_value("object", &EvaluationContext::default())
            .await
            .unwrap();

        assert_eq!(
            details.value.fields.get("enabled"),
            Some(&Value::Bool(true))
        );
        assert_eq!(details.value.fields.get("count"), Some(&Value::Int(3)));
    }

    #[tokio::test]
    async fn json_array_payload_returns_type_mismatch() {
        let provider =
            UnleashFlagProvider::new(FakeClient::new([("array", variant("json", "[1,2,3]"))]));

        let error = provider
            .resolve_struct_value("array", &EvaluationContext::default())
            .await
            .unwrap_err();

        assert_eq!(error.code, EvaluationErrorCode::TypeMismatch);
    }

    #[test]
    fn maps_openfeature_context_to_unleash_context() {
        let context = EvaluationContext::default()
            .with_targeting_key("targeting-key")
            .with_custom_field("userId", "explicit-user")
            .with_custom_field("sessionId", "session-123")
            .with_custom_field("thing", "test")
            .with_custom_field("enabled", true)
            .with_custom_field("nested", EvaluationContextFieldValue::new_struct(42_i64));

        let context = to_unleash_context(&context);

        assert_eq!(context.user_id.as_deref(), Some("targeting-key"));
        assert_eq!(context.session_id.as_deref(), Some("session-123"));
        assert_eq!(
            context.properties.get("thing").map(String::as_str),
            Some("test")
        );
        assert_eq!(
            context.properties.get("enabled").map(String::as_str),
            Some("true")
        );
        assert!(!context.properties.contains_key("nested"));
    }
}
