# Bicho Rust

[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)
[![crates.io](https://img.shields.io/crates/v/bicho)](https://crates.io/crates/bicho)
[![OTLP/HTTP JSON](https://img.shields.io/badge/OTLP-HTTP%20JSON-4de034.svg)](https://opentelemetry.io/docs/specs/otlp/)

Report Rust errors and messages to [Bicho](https://bicho.fyi) over OTLP/HTTP JSON. Use it from a Tokio application or the Rust side of a Tauri app. The webview can use the separate [JavaScript SDK](https://github.com/bicho-fyi/bicho-javascript).

## Add to your project

This crate is not published yet. Use a local path dependency:

```toml
[dependencies]
bicho = { path = "../bicho-rust" }
```

Create a client using the `otlp.endpoint` and `X-Bicho-Key` from project registration:

```rust,no_run
use bicho::{Client, Config};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let tracker = Client::new(
    Config::new("https://collector.example/v1/logs", "YOUR_PUBLIC_KEY")
        .service_name("desktop-app")
        .environment("production")
        .release(env!("CARGO_PKG_VERSION")),
)?;

if let Err(error) = std::fs::read_to_string("settings.json") {
    tracker.capture_error(&error).await?;
}
# Ok(())
# }
```

Sending requires a Tokio runtime. Awaiting capture waits for delivery or returns an error; there is no background queue and nothing to flush. Capture failure should not replace the application's original error. Handle or log delivery failures separately.

`Client` is cloneable and can be shared across async tasks. Each client has independent configuration. Clones reuse the HTTP connection pool; capture data belongs to each event, not a global scope.

## Tauri

Create one client during setup, register it with `app.manage(tracker)`, and use `tauri::State<'_, bicho::Client>` in async commands. Build the event before spawning background work; an owned event can move into `tauri::async_runtime::spawn` with a cloned client. If you spawn a task, the app can exit before it completes.

The source checkout includes `examples/tauri`, a small integration library showing managed state, an async command and explicit error reporting. This separate example crate is not included in the packaged SDK. It does not require the SDK to depend on Tauri. Tauri uses Tokio, so no extra runtime is needed inside an async command. The example does not install panic hooks, intercept every command or change crash handling.

A distributed desktop app cannot keep an embedded secret confidential. Use public-key-only projects for Tauri clients. Backend services can opt into `Config::secret(...)` for secret-enabled projects; never embed the core admin token.

## Capture an event

```rust,no_run
use bicho::{Client, Config, Event, Level};
use serde_json::json;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let tracker = Client::new(Config::new("https://collector.example/v1/logs", "YOUR_PUBLIC_KEY"))?;
let event = Event::exception("SettingsError", "Could not load settings")
    .with_level(Level::Error)
    .with_tag("operation", "load_settings")
    .with_context("settings", json!({"format": "json"}));

let uid = tracker.send(&event).await?;
assert_eq!(uid, event.uid());
# Ok(())
# }
```

`capture_message(...)` creates an ERROR event. `capture_error(&error)` captures the error's concrete Rust type name, display message and up to eight source messages in `bicho.contexts`. For trait objects, the type name can be the trait rather than the concrete error.

Use `Event::message(...)` or `Event::exception(...)` when you need tags, contexts, severity or an explicit stacktrace via `with_stacktrace(...)`. Rust errors do not generally carry a backtrace; the SDK does not invent one. Attach a backtrace captured at the failure site when available. Bicho preserves its text; derived frames depend on the server's supported stack formats. Native symbolication and automatic source-map linkage are not included.

Levels are Debug, Info, Warning, Error and Fatal. Plain messages must be Error or Fatal; exceptions may use any level. Tags accept scalar strings, numbers and booleans. Protocol-owned attributes (`log.record.uid`, `exception.*`, `sdk.*`, `bicho.*`, `service.*`, `deployment.*`) cannot be overridden through tags.

## Delivery and retries

Requests use the exact `/v1/logs` endpoint. HTTPS is required except on loopback for local tests. URL credentials, query strings and fragments are rejected. HTTP redirects are not followed.

The SDK sends one event per request. Network failures and HTTP 429/502/503/504 responses allow at most two retries, with the same serialized payload and occurrence UID. Retry-After supports seconds and HTTP dates. Backoff cannot exceed the overall delivery deadline; long delays return an error instead of retrying early. Requests time out after five seconds, and the overall deadline defaults to 15 seconds. `Config::timeout(...)` sets the overall deadline, up to 60 seconds.

A successful call returns the occurrence UID, not Bicho's stored event ID. Bicho derives its event ID from that UID and the authenticated project. If you explicitly retry after an ambiguous failure, reuse the same `Event` with `send(&event)`. Calling `capture_error` or `capture_message` again creates a new occurrence.

HTTP 200 must contain a valid OTLP JSON acknowledgment. Populated partial-success responses are terminal and return `Error::PartialSuccess`; do not retry them. Other HTTP failures return `Error::Http`, which includes a parsed retry delay when supplied. SDK errors never include upstream response bodies or credentials.

There is no persistent offline queue or delivery guarantee. Await capture before exiting; abrupt termination can lose in-flight requests. The SDK does not collect logs, console output, network requests, tracing spans, panic data or user identity automatically.

## Privacy and limits

Credential-like keys in tags and contexts, Basic/Bearer strings and HTTP(S) URL credentials/query/fragment values are filtered before sending. Config and client debug output omit credentials; event debug output includes only its UID. Filtering is best effort, not a guarantee that arbitrary prose or source code contains no secrets. Send only evidence you intend administrators to read.

Events are limited to 60 KiB serialized. Messages and stack text stop at 16 KiB; metadata and tag values at 512 bytes. Context normalization stops at five nested levels, 50 entries per collection and 300 visited values, with string values limited to 2048 bytes. At most 100 tags and 50 contexts are accepted. String clipping preserves UTF-8 boundaries. Responses are limited to 64 KiB. Contexts appear in captured OTLP evidence as the `bicho.contexts` JSON string.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo check --manifest-path examples/tauri/Cargo.toml
cargo package --allow-dirty
```

Tests use loopback HTTP servers, not hosted Bicho installations. The core interoperability probe is kept separately from the portable crate tests. Publication, a packaged Tauri application's delivery and platform-specific runtime testing are separate checks.

## License

Copyright 2026 Alex Zappa (FreshJuice). Licensed under Apache-2.0. See [LICENSE](LICENSE) and [ACKNOWLEDGEMENTS.md](ACKNOWLEDGEMENTS.md).
