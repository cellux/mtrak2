//! HTTP integration tests for the REPL service.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use mtrak_core::composition::Composition;
use mtrak_midi_io::TrackerProject;
use mtrak_repl::{handle_request, ReplService, ReplSession};
use mtrak_tui::App;

fn test_app() -> App {
    App::new(TrackerProject::new(Composition::new()))
}

/// A running service plus the application-thread handler that services it.
struct TestService {
    service: Option<ReplService>,
    port: u16,
    handle: Option<thread::JoinHandle<()>>,
}

impl TestService {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, receiver) = mpsc::channel();
        let service = ReplService::start(listener, sender).unwrap();
        let handle = thread::spawn(move || {
            let mut app = test_app();
            let mut session = ReplSession::new().unwrap();
            while let Ok(request) = receiver.recv() {
                let response = handle_request(&mut session, &mut app, request.kind);
                let _ = request.response.send(response);
            }
        });
        TestService {
            service: Some(service),
            port,
            handle: Some(handle),
        }
    }
}

impl Drop for TestService {
    fn drop(&mut self) {
        self.service.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn http_request(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .unwrap_or(0);
    let body = response.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

fn raw_request(port: u16, request: &[u8]) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(request).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .unwrap_or(0);
    let body = response.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

#[test]
fn chunked_transfer_encoding_is_rejected() {
    let service = TestService::start();
    let (status, body) = raw_request(
        service.port,
        b"POST /eval HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
    );
    assert_eq!(status, 400, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unsupported transfer-encoding"),
        "{body}"
    );
}

#[test]
fn truncated_body_is_rejected() {
    let service = TestService::start();
    let (status, body) = raw_request(
        service.port,
        b"POST /eval HTTP/1.1\r\nHost: x\r\nContent-Length: 40\r\n\r\n{\"form\": 1}",
    );
    assert_eq!(status, 400, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("request body incomplete"),
        "{body}"
    );
}

#[test]
fn malformed_request_line_is_rejected() {
    let service = TestService::start();
    let (status, body) = raw_request(service.port, b"BOGUS\r\n\r\n");
    assert_eq!(status, 400, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ok"], false);
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("malformed request line"),
        "{body}"
    );
}

#[test]
fn invalid_content_length_is_rejected() {
    let service = TestService::start();
    let (status, body) = raw_request(
        service.port,
        b"POST /eval HTTP/1.1\r\nHost: x\r\nContent-Length: abc\r\n\r\n",
    );
    assert_eq!(status, 400, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("invalid content-length"),
        "{body}"
    );
}

#[test]
fn oversized_headers_are_rejected() {
    let service = TestService::start();
    let mut request = b"POST /eval HTTP/1.1\r\nHost: x\r\nX-Pad: ".to_vec();
    request.extend(std::iter::repeat(b'A').take(32 * 1024));
    request.extend_from_slice(b"\r\nContent-Length: 0\r\n\r\n");
    let (status, body) = raw_request(service.port, &request);
    assert_eq!(status, 400, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("request headers too large"),
        "{body}"
    );
}

#[test]
fn eval_rejects_non_string_form() {
    let service = TestService::start();
    let (status, body) = http_request(service.port, "POST", "/eval", "{\"form\": 1}");
    assert_eq!(status, 400, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("form field must be a string"),
        "{body}"
    );
}

#[test]
fn eval_rejects_empty_form() {
    let service = TestService::start();
    let (status, body) = http_request(service.port, "POST", "/eval", "{\"form\": \"\"}");
    assert_eq!(status, 400, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("form field is empty"),
        "{body}"
    );
}

#[test]
fn help_lists_routes_and_namespaces() {
    let service = TestService::start();
    let (status, body) = http_request(service.port, "POST", "/help", "");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ok"], true);
    let printed = json["printed"].as_str().unwrap();
    assert!(printed.contains("/status"), "{printed}");
    assert!(printed.contains("patterns"), "{printed}");
}

#[test]
fn status_reports_playback_without_stopping_it() {
    let service = TestService::start();
    let (status, body) = http_request(service.port, "POST", "/play", "");
    assert_eq!(status, 200, "{body}");
    let (status, body) = http_request(service.port, "POST", "/status", "");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["value"]["playing"], true, "{body}");
    // A second status call still reports playback: reading must not stop it.
    let (_, body) = http_request(service.port, "POST", "/status", "");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["value"]["playing"], true, "{body}");
    let (_, body) = http_request(service.port, "POST", "/stop", "");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ok"], true, "{body}");
    let (_, body) = http_request(service.port, "POST", "/status", "");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["value"]["playing"], false, "{body}");
}

#[test]
fn eval_returns_json_success() {
    let service = TestService::start();
    let (status, body) = http_request(
        service.port,
        "POST",
        "/eval",
        r#"{"form":"(mtrak.patterns.list)"}"#,
    );
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["type"], "table");
}

#[test]
fn eval_compile_error_returns_structured_error() {
    let service = TestService::start();
    let (status, body) = http_request(
        service.port,
        "POST",
        "/eval",
        r#"{"form":"(unknown-global 1)"}"#,
    );
    assert_eq!(status, 400, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["kind"], "compile");
}

#[test]
fn unknown_route_returns_error() {
    let service = TestService::start();
    let (status, body) = http_request(service.port, "POST", "/nope", "{}");
    assert_eq!(status, 400, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ok"], false);
}

#[test]
fn malformed_json_returns_error() {
    let service = TestService::start();
    let (status, body) = http_request(service.port, "POST", "/eval", "{not json");
    assert_eq!(status, 400, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ok"], false);
}

#[test]
fn get_is_rejected() {
    let service = TestService::start();
    let (status, body) = http_request(service.port, "GET", "/eval", "");
    assert_eq!(status, 400, "{body}");
}

#[test]
fn play_and_stop_work() {
    let service = TestService::start();
    let (status, body) = http_request(service.port, "POST", "/play", "{}");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ok"], true);

    let (status, body) = http_request(service.port, "POST", "/stop", "");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ok"], true);
}

#[test]
fn reset_works() {
    let service = TestService::start();
    let (status, body) = http_request(service.port, "POST", "/repl/reset", "");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ok"], true);
}

#[test]
fn play_rejects_unknown_pattern() {
    let service = TestService::start();
    let (status, body) = http_request(service.port, "POST", "/play", r#"{"pattern":99}"#);
    assert_eq!(status, 400, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ok"], false);
}

#[test]
fn play_rejects_malformed_fields() {
    let service = TestService::start();
    for body in [
        r#"{"pattern":"x"}"#,
        r#"{"pattern":300}"#,
        r#"{"row":-1}"#,
        r#"{not json"#,
        r#"[1,2,3]"#,
    ] {
        let (status, response) = http_request(service.port, "POST", "/play", body);
        assert_eq!(status, 400, "body {body}: {response}");
        let json: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(json["ok"], false, "body {body}: {response}");
    }
}

#[test]
fn play_accepts_row_only() {
    let service = TestService::start();
    let (status, body) = http_request(service.port, "POST", "/play", r#"{"row":0}"#);
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ok"], true);
}

#[test]
fn oversized_request_is_rejected() {
    let service = TestService::start();
    let mut stream = TcpStream::connect(("127.0.0.1", service.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let request = "POST /eval HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 70000\r\nConnection: close\r\n\r\n";
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .unwrap_or(0);
    assert_eq!(status, 400, "{response}");
}
