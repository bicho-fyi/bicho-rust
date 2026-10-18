# Bicho Rust

Rust SDK for explicit error reporting to Bicho's OTLP/HTTP JSON endpoint. Keep Tauri integration optional and separate from the SDK's dependency tree.

- Read README.md, src/ and relevant tests before editing.
- Use reqwest with rustls and Tokio; do not add a second transport or background queue without a concrete requirement.
- Ingest keys authorize capture only. Never accept or ship an admin token. Distributed desktop apps cannot protect embedded secrets.
- Preserve occurrence identity and serialized payloads across retries. Never retry populated OTLP partial success or follow redirects with credentials.
- Filter evidence before transport. Keep credentials out of errors, debug output, fixtures and Git.
- Run cargo fmt --check, cargo clippy --all-targets -- -D warnings and cargo test. Verify the Tauri example separately.
- Keep scratch files in ignored _temp/. No commits/pushes without "yolo" for the task. Publishing needs separate approval.
- Preserve the full Apache-2.0 text and attribution. No sibling checkout should be required for the normal crate suite.
