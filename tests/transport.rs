use bicho::{Client, Config, Error, Event, Level};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
    task::JoinHandle,
};

struct Request {
    line: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    received: Instant,
}

struct Server {
    endpoint: String,
    requests: mpsc::Receiver<Request>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    async fn start(responses: Vec<Option<String>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/logs", listener.local_addr().unwrap());
        let (sender, requests) = mpsc::channel(16);
        let task = tokio::spawn(async move {
            let mut responses = responses.into_iter();
            loop {
                let (mut stream, _) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let request = tokio::time::timeout(Duration::from_secs(3), async {
                    let mut bytes = Vec::new();
                    let end = loop {
                        let mut chunk = [0; 4096];
                        let count = stream.read(&mut chunk).await.unwrap();
                        assert_ne!(count, 0, "connection ended before request headers");
                        bytes.extend_from_slice(&chunk[..count]);
                        assert!(bytes.len() <= 256 * 1024);
                        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                            break end + 4;
                        }
                    };
                    let header = std::str::from_utf8(&bytes[..end]).unwrap();
                    let mut lines = header.split("\r\n");
                    let line = lines.next().unwrap().to_owned();
                    let headers: BTreeMap<_, _> = lines
                        .filter_map(|line| line.split_once(':'))
                        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
                        .collect();
                    let length: usize = headers.get("content-length").unwrap().parse().unwrap();
                    assert!(end + length <= 256 * 1024);
                    while bytes.len() < end + length {
                        let mut chunk = [0; 4096];
                        let count = stream.read(&mut chunk).await.unwrap();
                        assert_ne!(count, 0, "connection ended before request body");
                        bytes.extend_from_slice(&chunk[..count]);
                    }
                    Request {
                        line,
                        headers,
                        body: bytes[end..end + length].to_vec(),
                        received: Instant::now(),
                    }
                })
                .await
                .unwrap();
                if sender.send(request).await.is_err() {
                    break;
                }
                if let Some(response) = responses
                    .next()
                    .unwrap_or_else(|| Some(reply(200, "{}", "")))
                {
                    stream.write_all(response.as_bytes()).await.unwrap();
                    stream.shutdown().await.unwrap();
                }
            }
        });
        Self {
            endpoint,
            requests,
            task,
        }
    }

    fn client(&self) -> Client {
        Client::new(Config::new(&self.endpoint, "public-key").timeout(Duration::from_secs(4)))
            .unwrap()
    }

    async fn next(&mut self) -> Request {
        tokio::time::timeout(Duration::from_secs(4), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn quiet(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(100), self.requests.recv())
                .await
                .is_err(),
            "unexpected extra HTTP request"
        );
        assert!(
            !self.task.is_finished(),
            "loopback server stopped unexpectedly"
        );
    }
}

fn reply(status: u16, body: &str, extra: &str) -> String {
    format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
        body.len()
    )
}

fn attribute<'a>(attributes: &'a Value, key: &str) -> &'a Value {
    &attributes
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["key"] == key)
        .unwrap_or_else(|| panic!("missing attribute {key}"))["value"]
}

#[tokio::test]
async fn truncated_acknowledgment_retries_the_identical_event() {
    let truncated = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 20\r\nConnection: close\r\n\r\n{".to_owned();
    let mut server = Server::start(vec![Some(truncated), Some(reply(200, "{}", ""))]).await;
    let event = Event::message("truncated acknowledgment");
    assert_eq!(server.client().send(&event).await.unwrap(), event.uid());
    let first = server.next().await;
    let second = server.next().await;
    assert_eq!(first.body, second.body);
    server.quiet().await;
}

#[tokio::test]
async fn sends_actual_otlp_headers_metadata_and_exception() {
    let mut server = Server::start(vec![]).await;
    let client = Client::new(
        Config::new(&server.endpoint, "public-key")
            .secret("ingest-secret")
            .service_name("desktop/rust")
            .environment("test")
            .release("1.2.3")
            .timeout(Duration::from_secs(2)),
    )
    .unwrap();
    let event = Event::exception("DatabaseError", "database unavailable")
        .with_level(Level::Warning)
        .with_stacktrace("main at src/main.rs:42")
        .with_tag("attempt", 2)
        .with_tag("retryable", true)
        .with_context("window", json!({"name": "main"}));
    assert_eq!(client.send(&event).await.unwrap(), event.uid());
    let request = server.next().await;
    assert_eq!(request.line, "POST /v1/logs HTTP/1.1");
    assert_eq!(request.headers["content-type"], "application/json");
    assert_eq!(request.headers["x-bicho-key"], "public-key");
    assert_eq!(request.headers["x-bicho-secret"], "ingest-secret");
    assert!(!request.headers.contains_key("authorization"));
    let payload: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(payload["resourceLogs"].as_array().unwrap().len(), 1);
    let resource = &payload["resourceLogs"][0];
    for (key, expected) in [
        ("service.name", "desktop/rust"),
        ("deployment.environment.name", "test"),
        ("service.version", "1.2.3"),
    ] {
        assert_eq!(
            attribute(&resource["resource"]["attributes"], key)["stringValue"],
            expected
        );
    }
    let scope = &resource["scopeLogs"][0];
    assert_eq!(scope["scope"]["name"], "bicho.rust");
    assert_eq!(scope["logRecords"].as_array().unwrap().len(), 1);
    let record = &scope["logRecords"][0];
    assert!(
        record["timeUnixNano"]
            .as_str()
            .unwrap()
            .parse::<u128>()
            .unwrap()
            > 0
    );
    assert_eq!(record["severityNumber"], 13);
    assert_eq!(record["body"]["stringValue"], "database unavailable");
    let attrs = &record["attributes"];
    for (key, expected) in [
        ("log.record.uid", event.uid()),
        ("exception.type", "DatabaseError"),
        ("exception.message", "database unavailable"),
        ("exception.stacktrace", "main at src/main.rs:42"),
        ("sdk.name", "bicho.rust"),
        ("sdk.version", bicho::VERSION),
    ] {
        assert_eq!(attribute(attrs, key)["stringValue"], expected);
    }
    assert_eq!(attribute(attrs, "attempt")["intValue"], "2");
    assert_eq!(attribute(attrs, "retryable")["boolValue"], true);
    let contexts: Value = serde_json::from_str(
        attribute(attrs, "bicho.contexts")["stringValue"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(contexts, json!({"window": {"name": "main"}}));
    server.quiet().await;
}

#[tokio::test]
async fn lost_ack_and_manual_retry_preserve_exact_payload_and_uid() {
    let mut server = Server::start(vec![None, Some(reply(200, "{}", ""))]).await;
    let client = server.client();
    let event = Event::message("one occurrence");
    assert_eq!(client.send(&event).await.unwrap(), event.uid());
    assert_eq!(client.send(&event).await.unwrap(), event.uid());
    let first = server.next().await;
    for _ in 0..2 {
        let retry = server.next().await;
        assert_eq!(retry.body, first.body);
        assert_eq!(retry.headers["x-bicho-key"], first.headers["x-bicho-key"]);
    }
    server.quiet().await;
}

#[tokio::test]
async fn populated_partial_success_is_terminal_without_retry() {
    for body in [
        r#"{"partialSuccess":{"rejectedLogRecords":"1"}}"#,
        r#"{"partialSuccess":{"errorMessage":"some records dropped"}}"#,
    ] {
        let mut server = Server::start(vec![Some(reply(200, body, ""))]).await;
        assert_eq!(
            server.client().capture_message("partial").await,
            Err(Error::PartialSuccess)
        );
        server.next().await;
        server.quiet().await;
    }
}

#[tokio::test]
async fn forbidden_and_malformed_success_are_terminal() {
    for (status, body, expected) in [
        (
            403,
            "forbidden",
            Error::Http {
                status: 403,
                retry_after: None,
            },
        ),
        (200, "not json", Error::InvalidResponse),
        (200, "null", Error::InvalidResponse),
        (200, "", Error::InvalidResponse),
        (
            200,
            r#"{"error":"not an OTLP acknowledgement"}"#,
            Error::InvalidResponse,
        ),
    ] {
        let mut server = Server::start(vec![Some(reply(status, body, ""))]).await;
        assert_eq!(
            server.client().capture_message("terminal").await,
            Err(expected)
        );
        server.next().await;
        server.quiet().await;
    }
}

#[tokio::test]
async fn redirect_never_forwards_ingest_credentials() {
    let mut target = Server::start(vec![]).await;
    let mut origin = Server::start(vec![Some(reply(
        302,
        "",
        &format!("Location: {}\r\n", target.endpoint),
    ))])
    .await;
    let client =
        Client::new(Config::new(&origin.endpoint, "private-key").secret("private-secret")).unwrap();
    assert_eq!(
        client.capture_message("redirect").await,
        Err(Error::Http {
            status: 302,
            retry_after: None
        })
    );
    let request = origin.next().await;
    assert_eq!(request.headers["x-bicho-key"], "private-key");
    assert_eq!(request.headers["x-bicho-secret"], "private-secret");
    origin.quiet().await;
    target.quiet().await;
}

#[tokio::test]
async fn retryable_statuses_honor_retry_after_and_preserve_payload() {
    for status in [503, 429] {
        let mut server = Server::start(vec![Some(reply(status, "", "Retry-After: 1\r\n"))]).await;
        let client = server.client();
        let event = Event::message("retry me");
        assert_eq!(client.send(&event).await.unwrap(), event.uid());
        let first = server.next().await;
        let second = server.next().await;
        assert!(second.received.duration_since(first.received) >= Duration::from_millis(950));
        assert_eq!(first.body, second.body);
        server.quiet().await;
    }
}

#[tokio::test]
async fn retry_attempts_are_bounded() {
    let mut server = Server::start(
        (0..4)
            .map(|_| Some(reply(503, "", "Retry-After: 0\r\n")))
            .collect(),
    )
    .await;
    assert_eq!(
        server.client().capture_message("bounded").await,
        Err(Error::Http {
            status: 503,
            retry_after: Some(Duration::ZERO)
        })
    );
    let first = server.next().await;
    for _ in 0..2 {
        assert_eq!(server.next().await.body, first.body);
    }
    server.quiet().await;
}

#[tokio::test]
async fn total_deadline_bounds_retry_sleep() {
    let mut server = Server::start(vec![
        Some(reply(503, "", "Retry-After: 1\r\n")),
        Some(reply(429, "", "Retry-After: 1\r\n")),
    ])
    .await;
    let client = Client::new(
        Config::new(&server.endpoint, "public-key").timeout(Duration::from_millis(1200)),
    )
    .unwrap();
    let started = Instant::now();
    assert_eq!(
        client.capture_message("deadline").await,
        Err(Error::Timeout)
    );
    assert!(started.elapsed() >= Duration::from_millis(1100));
    assert!(started.elapsed() < Duration::from_millis(2200));
    server.next().await;
    server.next().await;
    server.quiet().await;
}

#[tokio::test]
async fn total_deadline_bounds_waiting_for_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client::new(
        Config::new(
            format!("http://{}/v1/logs", listener.local_addr().unwrap()),
            "public-key",
        )
        .timeout(Duration::from_millis(150)),
    )
    .unwrap();
    let started = Instant::now();
    let delivery = tokio::spawn(async move { client.capture_message("no ack").await });
    let (stream, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), delivery)
            .await
            .unwrap()
            .unwrap(),
        Err(Error::Timeout)
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(stream);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cloned_and_separate_clients_are_send_sync_and_independent() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Client>();
    send_sync::<Config>();
    send_sync::<Event>();
    send_sync::<Error>();
    let mut first = Server::start(vec![]).await;
    let mut second = Server::start(vec![]).await;
    let client =
        Client::new(Config::new(&first.endpoint, "first-key").service_name("first")).unwrap();
    let clone = client.clone();
    let other =
        Client::new(Config::new(&second.endpoint, "second-key").service_name("second")).unwrap();
    let one = tokio::spawn(async move { client.capture_message("one").await.unwrap() });
    let two = tokio::spawn(async move {
        clone
            .capture_error(&std::io::Error::other("two"))
            .await
            .unwrap()
    });
    let three = tokio::spawn(async move {
        other
            .send(&Event::from_error(&std::io::Error::other("three")))
            .await
            .unwrap()
    });
    let ids = [one.await.unwrap(), two.await.unwrap(), three.await.unwrap()];
    assert_ne!(ids[0], ids[1]);
    assert_ne!(ids[1], ids[2]);
    assert_ne!(ids[0], ids[2]);
    for _ in 0..2 {
        let request = first.next().await;
        assert_eq!(request.headers["x-bicho-key"], "first-key");
        assert!(!request.headers.contains_key("x-bicho-secret"));
        let payload: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            attribute(
                &payload["resourceLogs"][0]["resource"]["attributes"],
                "service.name"
            )["stringValue"],
            "first"
        );
    }
    let request = second.next().await;
    assert_eq!(request.headers["x-bicho-key"], "second-key");
    let payload: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(
        attribute(
            &payload["resourceLogs"][0]["resource"]["attributes"],
            "service.name"
        )["stringValue"],
        "second"
    );
    first.quiet().await;
    second.quiet().await;
}

#[tokio::test]
async fn oversized_event_is_rejected_before_network_io() {
    let mut server = Server::start(vec![]).await;
    let mut event = Event::message("oversized");
    for index in 0..100 {
        event = event.with_tag(format!("field-{index}"), "\u{1}".repeat(512));
    }
    assert_eq!(
        server.client().send(&event).await,
        Err(Error::PayloadTooLarge)
    );
    server.quiet().await;
}
