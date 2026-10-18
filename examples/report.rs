use bicho::{Client, Config, Event};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(
        Config::new(
            std::env::var("BICHO_ENDPOINT")?,
            std::env::var("BICHO_PUBLIC_KEY")?,
        )
        .service_name("rust-sdk-example")
        .environment("test")
        .release(concat!("rust-sdk-example@", env!("CARGO_PKG_VERSION"))),
    )?;
    let event = Event::exception("ExampleError", "Explicit Rust SDK test")
        .with_stacktrace("at src/main.rs:42:7")
        .with_tag("operation", "sdk-example")
        .with_context(
            "example",
            json!({"password": "private-fixture", "safe": true}),
        );
    client.send(&event).await?;
    client.send(&event).await?;
    println!("{}", event.uid());
    Ok(())
}
