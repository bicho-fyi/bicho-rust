#![doc = include_str!("../README.md")]

mod privacy;

use privacy::{normalize, scrub, sensitive};
use reqwest::{
    Url,
    header::{HeaderMap, HeaderValue, RETRY_AFTER},
    redirect::Policy,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_PAYLOAD: usize = 60 * 1024;
const MAX_RESPONSE: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidConfig(&'static str),
    InvalidEvent(&'static str),
    PayloadTooLarge,
    Transport,
    Timeout,
    Http {
        status: u16,
        retry_after: Option<Duration>,
    },
    InvalidResponse,
    PartialSuccess,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) | Self::InvalidEvent(message) => f.write_str(message),
            Self::PayloadTooLarge => f.write_str("Bicho event exceeds 60 KiB"),
            Self::Transport => f.write_str("Bicho request could not be completed"),
            Self::Timeout => f.write_str("Bicho delivery deadline exceeded"),
            Self::Http { status, .. } => write!(f, "Bicho returned HTTP {status}"),
            Self::InvalidResponse => f.write_str("Bicho returned an invalid OTLP response"),
            Self::PartialSuccess => {
                f.write_str("Bicho reported OTLP partial success; do not retry this event")
            }
        }
    }
}
impl std::error::Error for Error {}

#[derive(Clone)]
pub struct Config {
    endpoint: String,
    key: String,
    secret: Option<String>,
    service_name: String,
    environment: Option<String>,
    release: Option<String>,
    timeout: Duration,
}
impl Config {
    pub fn new(endpoint: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            key: key.into(),
            secret: None,
            service_name: "app/rust".into(),
            environment: None,
            release: None,
            timeout: Duration::from_secs(15),
        }
    }
    pub fn service_name(mut self, value: impl Into<String>) -> Self {
        self.service_name = value.into();
        self
    }
    pub fn environment(mut self, value: impl Into<String>) -> Self {
        self.environment = Some(value.into());
        self
    }
    pub fn release(mut self, value: impl Into<String>) -> Self {
        self.release = Some(value.into());
        self
    }
    pub fn secret(mut self, value: impl Into<String>) -> Self {
        self.secret = Some(value.into());
        self
    }
    pub fn timeout(mut self, value: Duration) -> Self {
        self.timeout = value;
        self
    }
}
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Config { credentials: [Filtered], .. }")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warning,
    Error,
    Fatal,
}
impl Level {
    fn severity(self) -> u8 {
        match self {
            Self::Debug => 5,
            Self::Info => 9,
            Self::Warning => 13,
            Self::Error => 17,
            Self::Fatal => 21,
        }
    }
}

#[derive(Clone)]
pub struct Event {
    uid: String,
    timestamp: String,
    message: String,
    exception: Option<String>,
    stacktrace: Option<String>,
    level: Level,
    tags: BTreeMap<String, Value>,
    contexts: BTreeMap<String, Value>,
}
impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Event")
            .field("uid", &self.uid)
            .finish_non_exhaustive()
    }
}
impl Event {
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            uid: uuid::Uuid::new_v4().to_string(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .to_string(),
            message: message.into(),
            exception: None,
            stacktrace: None,
            level: Level::Error,
            tags: BTreeMap::new(),
            contexts: BTreeMap::new(),
        }
    }
    pub fn exception(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            exception: Some(kind.into()),
            ..Self::message(message)
        }
    }
    pub fn from_error<E: std::error::Error + ?Sized>(error: &E) -> Self {
        let mut event = Self::exception(std::any::type_name::<E>(), error.to_string());
        let mut causes = Vec::new();
        let mut source = error.source();
        while let Some(error) = source {
            causes.push(scrub(&error.to_string(), 2048));
            if causes.len() == 8 {
                break;
            }
            source = error.source();
        }
        if !causes.is_empty() {
            event.contexts.insert("error_chain".into(), json!(causes));
        }
        event
    }
    pub fn uid(&self) -> &str {
        &self.uid
    }
    pub fn with_stacktrace(mut self, value: impl Into<String>) -> Self {
        self.stacktrace = Some(value.into());
        self
    }
    pub fn with_level(mut self, value: Level) -> Self {
        self.level = value;
        self
    }
    pub fn with_tag(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.tags.insert(key.into(), value.into());
        self
    }
    pub fn with_context(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.contexts.insert(key.into(), value.into());
        self
    }
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: Url,
    config: Config,
}
impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Client { config: [Filtered], .. }")
    }
}
impl Client {
    pub fn new(config: Config) -> Result<Self, Error> {
        let endpoint = Url::parse(&config.endpoint)
            .map_err(|_| Error::InvalidConfig("Invalid Bicho endpoint"))?;
        let loopback = endpoint.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        let raw_path = config
            .endpoint
            .split_once("://")
            .and_then(|(_, rest)| rest.find('/').map(|start| &rest[start..]));
        if endpoint.path() != "/v1/logs"
            || raw_path != Some("/v1/logs")
            || config.endpoint.contains(['?', '#', '@', '\\'])
            || config.endpoint.chars().any(char::is_whitespace)
            || endpoint.host().is_none()
            || !(endpoint.scheme() == "https" || endpoint.scheme() == "http" && loopback)
        {
            return Err(Error::InvalidConfig(
                "Expected an HTTPS /v1/logs endpoint without credentials, query or fragment; HTTP is allowed only on loopback",
            ));
        }
        if config.timeout.is_zero()
            || config.timeout > Duration::from_secs(60)
            || config.service_name.is_empty()
            || [&config.service_name]
                .into_iter()
                .chain(config.environment.iter())
                .chain(config.release.iter())
                .any(|value| value.len() > 512)
        {
            return Err(Error::InvalidConfig(
                "Invalid metadata or timeout (must be between zero and 60 seconds)",
            ));
        }
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("X-Bicho-Key", Some(&config.key)),
            ("X-Bicho-Secret", config.secret.as_ref()),
        ] {
            if let Some(value) = value {
                if value.is_empty() || value.len() > 512 {
                    return Err(Error::InvalidConfig("Invalid ingest credential"));
                }
                let mut header = HeaderValue::from_str(value)
                    .map_err(|_| Error::InvalidConfig("Invalid ingest credential"))?;
                header.set_sensitive(true);
                headers.insert(name, header);
            }
        }
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .referer(false)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| Error::Transport)?;
        Ok(Self {
            http,
            endpoint,
            config,
        })
    }
    pub async fn capture_message(&self, message: impl Into<String>) -> Result<String, Error> {
        self.send(&Event::message(message)).await
    }
    pub async fn capture_error<E: std::error::Error + ?Sized>(
        &self,
        error: &E,
    ) -> Result<String, Error> {
        self.send(&Event::from_error(error)).await
    }
    pub async fn send(&self, event: &Event) -> Result<String, Error> {
        let payload = self.payload(event)?;
        tokio::time::timeout(self.config.timeout, self.deliver(&payload))
            .await
            .map_err(|_| Error::Timeout)??;
        Ok(event.uid.clone())
    }
    fn payload(&self, event: &Event) -> Result<Vec<u8>, Error> {
        if event.exception.is_none() && event.level.severity() < 17 {
            return Err(Error::InvalidEvent(
                "Plain Bicho messages must use ERROR or FATAL severity",
            ));
        }
        if event.tags.len() > 100 || event.contexts.len() > 50 {
            return Err(Error::InvalidEvent("Too many tags or contexts"));
        }
        let mut attrs = Vec::new();
        for (key, value) in &event.tags {
            if key.is_empty()
                || key.len() > 128
                || !(value.is_string() || value.is_boolean() || value.is_number())
            {
                return Err(Error::InvalidEvent(
                    "Tags require bounded keys and scalar values",
                ));
            }
            if reserved(key) {
                continue;
            }
            let value = if sensitive(key) {
                json!("[Filtered]")
            } else if let Some(value) = value.as_str() {
                json!(scrub(value, 512))
            } else {
                value.clone()
            };
            attrs.push(attribute(key, value));
        }
        attrs.push(attribute("log.record.uid", json!(event.uid)));
        attrs.push(attribute("sdk.name", json!("bicho.rust")));
        attrs.push(attribute("sdk.version", json!(VERSION)));
        if let Some(kind) = &event.exception {
            attrs.push(attribute("exception.type", json!(scrub(kind, 512))));
            attrs.push(attribute(
                "exception.message",
                json!(scrub(&event.message, 16384)),
            ));
        }
        if let Some(stack) = &event.stacktrace {
            attrs.push(attribute(
                "exception.stacktrace",
                json!(scrub(stack, 16384)),
            ));
        }
        if !event.contexts.is_empty() {
            attrs.push(attribute(
                "bicho.contexts",
                json!(normalize(&json!(event.contexts)).to_string()),
            ));
        }
        let mut resource = vec![attribute(
            "service.name",
            json!(scrub(&self.config.service_name, 512)),
        )];
        for (key, value) in [
            ("deployment.environment.name", &self.config.environment),
            ("service.version", &self.config.release),
        ] {
            if let Some(value) = value {
                resource.push(attribute(key, json!(scrub(value, 512))));
            }
        }
        let payload = serde_json::to_vec(&json!({"resourceLogs": [{"resource": {"attributes": resource}, "scopeLogs": [{"scope": {"name": "bicho.rust", "version": VERSION}, "logRecords": [{"timeUnixNano": event.timestamp, "severityNumber": event.level.severity(), "body": {"stringValue": scrub(&event.message, 16384)}, "attributes": attrs}]}]}]})).map_err(|_| Error::InvalidEvent("Could not serialize Bicho event"))?;
        if payload.len() > MAX_PAYLOAD {
            return Err(Error::PayloadTooLarge);
        }
        Ok(payload)
    }
    async fn deliver(&self, payload: &[u8]) -> Result<(), Error> {
        for attempt in 0..3 {
            let result = async {
                let mut response = self
                    .http
                    .post(self.endpoint.clone())
                    .header("Content-Type", "application/json")
                    .body(payload.to_vec())
                    .send()
                    .await
                    .map_err(|_| Error::Transport)?;
                let status = response.status().as_u16();
                if status != 200 {
                    return Err(Error::Http {
                        status,
                        retry_after: retry_after(
                            response
                                .headers()
                                .get(RETRY_AFTER)
                                .and_then(|value| value.to_str().ok()),
                        ),
                    });
                }
                if !response
                    .headers()
                    .get("Content-Type")
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| {
                        value
                            .split(';')
                            .next()
                            .unwrap_or_default()
                            .trim()
                            .eq_ignore_ascii_case("application/json")
                    })
                {
                    return Err(Error::InvalidResponse);
                }
                let mut body = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
                    if body.len() + chunk.len() > MAX_RESPONSE {
                        return Err(Error::InvalidResponse);
                    }
                    body.extend_from_slice(&chunk);
                }
                validate_response(&body)
            }
            .await;
            let error = match result {
                Ok(()) => return Ok(()),
                Err(error @ Error::Transport)
                | Err(
                    error @ Error::Http {
                        status: 429 | 502 | 503 | 504,
                        ..
                    },
                ) => error,
                Err(error) => return Err(error),
            };
            if attempt == 2 {
                return Err(error);
            }
            let delay = match error {
                Error::Http {
                    retry_after: Some(delay),
                    ..
                } => delay,
                _ => Duration::from_millis(250 * (1 << attempt)),
            };
            if delay >= self.config.timeout {
                return Err(error);
            }
            tokio::time::sleep(delay).await;
        }
        unreachable!()
    }
}
fn attribute(key: &str, value: Value) -> Value {
    let value = match value {
        Value::Bool(value) => json!({"boolValue": value}),
        Value::Number(value) if value.is_i64() => {
            json!({"intValue": value.to_string()})
        }
        Value::Number(value) if value.is_u64() => json!({"stringValue": value.to_string()}),
        Value::Number(value) => json!({"doubleValue": value}),
        _ => json!({"stringValue": value}),
    };
    json!({"key": key, "value": value})
}
fn reserved(key: &str) -> bool {
    key == "log.record.uid"
        || key.starts_with("exception.")
        || key.starts_with("sdk.")
        || key.starts_with("bicho.")
        || key.starts_with("service.")
        || key.starts_with("deployment.")
}
fn retry_after(value: Option<&str>) -> Option<Duration> {
    let value = value?.trim();
    value
        .parse::<u64>()
        .map(Duration::from_secs)
        .ok()
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()
                .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
        })
}
fn validate_response(bytes: &[u8]) -> Result<(), Error> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| Error::InvalidResponse)?;
    let object = value.as_object().ok_or(Error::InvalidResponse)?;
    if object.keys().any(|key| key != "partialSuccess") {
        return Err(Error::InvalidResponse);
    }
    if let Some(partial) = object.get("partialSuccess") {
        let partial = partial.as_object().ok_or(Error::InvalidResponse)?;
        if partial
            .keys()
            .any(|key| !["rejectedLogRecords", "errorMessage"].contains(&key.as_str()))
        {
            return Err(Error::InvalidResponse);
        }
        if let Some(message) = partial.get("errorMessage") {
            let message = message.as_str().ok_or(Error::InvalidResponse)?;
            if !message.is_empty() {
                return Err(Error::PartialSuccess);
            }
        }
        if let Some(rejected) = partial.get("rejectedLogRecords") {
            let count = match rejected {
                Value::String(value) => value.parse::<u64>().ok(),
                Value::Number(value) => value.as_u64(),
                _ => None,
            }
            .ok_or(Error::InvalidResponse)?;
            if count != 0 {
                return Err(Error::PartialSuccess);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numeric_tags_fit_protobuf_int64_without_losing_unsigned_precision() {
        assert_eq!(
            attribute("value", json!(i64::MAX))["value"]["intValue"],
            i64::MAX.to_string()
        );
        assert_eq!(
            attribute("value", json!(u64::MAX))["value"]["stringValue"],
            u64::MAX.to_string()
        );
        assert_eq!(
            attribute("value", json!(1.25))["value"]["doubleValue"],
            1.25
        );
    }
    #[test]
    fn validates_config_response_and_retry_dates() {
        for endpoint in [
            "http://example.test/v1/logs",
            "https://name:secret@example.test/v1/logs",
            "https://example.test/v1/logs?key=secret",
            "https://example.test/v1/logs#secret",
            "https://example.test/api",
            "https://example.test/extra/../v1/logs",
            "https:///example.test/v1/logs",
            "https://example.test\\v1/logs",
        ] {
            assert!(Client::new(Config::new(endpoint, "public")).is_err());
        }
        assert!(Client::new(Config::new("https://example.test/v1/logs", "bad\nkey")).is_err());
        assert!(
            Client::new(
                Config::new("https://example.test/v1/logs", "public").timeout(Duration::ZERO)
            )
            .is_err()
        );
        assert_eq!(validate_response(b"{}"), Ok(()));
        assert_eq!(
            validate_response(br#"{"partialSuccess":{"rejectedLogRecords":"0"}}"#),
            Ok(())
        );
        assert_eq!(
            validate_response(br#"{"partialSuccess":{"rejectedLogRecords":"1"}}"#),
            Err(Error::PartialSuccess)
        );
        assert_eq!(
            validate_response(br#"{"partialSuccess":{"errorMessage":"dropped"}}"#),
            Err(Error::PartialSuccess)
        );
        for invalid in [
            b"null".as_slice(),
            b"[]",
            b"",
            br#"{"error":"upstream-secret"}"#,
        ] {
            assert_eq!(validate_response(invalid), Err(Error::InvalidResponse));
        }
        assert_eq!(retry_after(Some("2")), Some(Duration::from_secs(2)));
        assert_eq!(
            retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")),
            Some(Duration::ZERO)
        );
        assert_eq!(retry_after(Some("invalid")), None);
    }
}
