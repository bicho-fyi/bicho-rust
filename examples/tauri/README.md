# Tauri managed-state example

This library wires a Bicho client into a Tauri 2 builder and reports invalid settings JSON from an async command. It compiles without a webview runtime; it is not a standalone desktop application.

In an existing Tauri app, create a client and apply the same pattern before `run`:

```rust,ignore
let tracker = bicho::Client::new(
    bicho::Config::new("https://collector.example/v1/logs", "YOUR_PUBLIC_KEY")
        .service_name("my-desktop-app")
        .release(env!("CARGO_PKG_VERSION")),
)?;

bicho_tauri_example::configure(tauri::Builder::default(), tracker)
    .run(tauri::generate_context!())?;
```

Add this example library as a path dependency or copy its command/state registration into your existing application. The `parse_settings` command accepts bounded JSON text and returns a generic parse error; it does not send the raw input as custom context. Reporting failure does not replace the original command result.

Use public ingest keys in distributed apps. Do not package backend secrets or the core admin token. Keep command permissions and application capabilities scoped to your application's needs.

Check the integration with `cargo check --manifest-path examples/tauri/Cargo.toml` from the SDK root. Checking this library does not test desktop packaging, webview behavior, panic reporting or shutdown delivery.
