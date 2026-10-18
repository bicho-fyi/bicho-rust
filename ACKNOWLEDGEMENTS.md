# Acknowledgements

Bicho Rust uses the [OpenTelemetry Protocol](https://opentelemetry.io/docs/specs/otlp/) JSON mapping and [exception log conventions](https://opentelemetry.io/docs/specs/semconv/exceptions/exceptions-logs/). It is an error-reporting client, not an implementation of the complete OpenTelemetry SDK.

Runtime dependencies include reqwest, Tokio, serde_json, uuid, regex and httpdate. Their licenses and those of transitive dependencies remain applicable; Cargo resolves and distributes their source packages independently. No OpenTelemetry schema files or Sentry implementation are vendored in this crate.
