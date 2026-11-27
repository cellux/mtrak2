//! A minimal localhost-only HTTP/1.1 server for the REPL.

use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::error::ReplError;
use crate::request::{ReplRequest, ReplRequestKind, ReplResponse};
use crate::service::error_response;

/// Maximum request body size.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// Maximum serialized response size.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Maximum size of the request line and headers.
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// How long an HTTP worker waits for the application thread.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(35);
/// How long a worker waits for more request bytes before giving up.
const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a worker waits to write a response before giving up.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// A running HTTP listener.
pub struct ReplService {
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ReplService {
    /// Starts the listener on an already-bound socket.
    pub fn start(listener: TcpListener, requests: Sender<ReplRequest>) -> std::io::Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = shutdown.clone();
        let handle = thread::spawn(move || {
            let _ = listener.set_nonblocking(true);
            while !thread_shutdown.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // Bounded I/O keeps a stalled client from pinning a
                        // worker thread forever.
                        let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
                        let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
                        let requests = requests.clone();
                        thread::spawn(move || {
                            let _ = handle_connection(stream, requests);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(ReplService {
            shutdown,
            handle: Some(handle),
        })
    }
}

impl Drop for ReplService {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_connection(mut stream: TcpStream, requests: Sender<ReplRequest>) -> std::io::Result<()> {
    let read = read_request(&mut stream);
    let rejected = !matches!(read, Ok(Ok(_)));
    let response = match read {
        Ok(Ok(request)) => dispatch(&request, &requests),
        Ok(Err(message)) => error_response(&ReplError::Protocol(message)),
        Err(error) => error_response(&ReplError::Io(error)),
    };
    write_response(&mut stream, response)?;
    if rejected {
        // A client that is still sending an oversized or malformed request
        // would otherwise see a connection reset instead of the response.
        drain(&mut stream);
    }
    Ok(())
}

/// Discards unread request bytes so a rejected request gets a clean close.
fn drain(stream: &mut TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut buffer = [0u8; 4096];
    let mut total = 0usize;
    while total < MAX_REQUEST_BYTES {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => total += read,
            Err(_) => break,
        }
    }
}

struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Result<HttpRequest, String>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    // Read the request line and headers byte by byte so the buffer stays
    // bounded even when a client sends an enormous header line.
    let mut head = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        if reader.read(&mut byte)? == 0 {
            break;
        }
        head.push(byte[0]);
        if head.len() > MAX_HEADER_BYTES {
            return Ok(Err("request headers too large".to_string()));
        }
        if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.lines();
    let mut parts = lines.next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    if method.is_empty() || path.is_empty() {
        return Ok(Err("malformed request line".to_string()));
    }
    let mut content_length: Option<usize> = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                let value = value.trim();
                match value.parse::<usize>() {
                    Ok(parsed) => content_length = Some(parsed),
                    Err(_) => return Ok(Err(format!("invalid content-length: {value}"))),
                }
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                let value = value.trim();
                // The server reads bodies by `Content-Length` only; pretending
                // to support chunked framing would silently mis-parse them.
                if !value.eq_ignore_ascii_case("identity") {
                    return Ok(Err(format!("unsupported transfer-encoding: {value}")));
                }
            }
        }
    }
    let content_length = content_length.unwrap_or(0);
    if content_length > MAX_REQUEST_BYTES {
        return Ok(Err("request body too large".to_string()));
    }
    let mut body = vec![0u8; content_length];
    if let Err(error) = reader.read_exact(&mut body) {
        return Ok(Err(match error.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                "request body incomplete".to_string()
            }
            std::io::ErrorKind::UnexpectedEof => "request body incomplete".to_string(),
            _ => return Err(error),
        }));
    }
    Ok(Ok(HttpRequest { method, path, body }))
}

/// Parses a `/play` body into optional pattern and row.
///
/// An empty body means "play the selected main from the current row". Any
/// supplied field must be a non-negative integer in range; malformed bodies are
/// rejected instead of silently ignored.
fn parse_play_body(body: &[u8]) -> Result<(Option<u8>, Option<usize>), String> {
    let text = std::str::from_utf8(body).map_err(|_| "request body is not UTF-8".to_string())?;
    if text.trim().is_empty() {
        return Ok((None, None));
    }
    let json: serde_json::Value =
        serde_json::from_str(text).map_err(|error| format!("invalid JSON: {error}"))?;
    let object = json
        .as_object()
        .ok_or_else(|| "request body must be a JSON object".to_string())?;
    let pattern = match object.get("pattern") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => {
            let raw = value
                .as_u64()
                .ok_or_else(|| "pattern must be a non-negative integer".to_string())?;
            Some(u8::try_from(raw).map_err(|_| "pattern must be between 0 and 255".to_string())?)
        }
    };
    let row = match object.get("row") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => {
            let raw = value
                .as_u64()
                .ok_or_else(|| "row must be a non-negative integer".to_string())?;
            Some(usize::try_from(raw).map_err(|_| "row is too large".to_string())?)
        }
    };
    Ok((pattern, row))
}

fn dispatch(request: &HttpRequest, requests: &Sender<ReplRequest>) -> ReplResponse {
    if request.method != "POST" {
        return error_response(&ReplError::Protocol(
            "only POST requests are supported".to_string(),
        ));
    }
    let kind = match request.path.as_str() {
        "/eval" => match serde_json::from_slice::<serde_json::Value>(&request.body) {
            Ok(json) => match json.get("form") {
                None | Some(serde_json::Value::Null) => {
                    return error_response(&ReplError::Protocol("missing form field".to_string()))
                }
                Some(serde_json::Value::String(form)) if !form.is_empty() => {
                    ReplRequestKind::Eval(form.clone())
                }
                Some(serde_json::Value::String(_)) => {
                    return error_response(&ReplError::Protocol("form field is empty".to_string()))
                }
                Some(_) => {
                    return error_response(&ReplError::Protocol(
                        "form field must be a string".to_string(),
                    ))
                }
            },
            Err(error) => {
                return error_response(&ReplError::Protocol(format!("invalid JSON: {error}")))
            }
        },
        "/play" => match parse_play_body(&request.body) {
            Ok((pattern, row)) => ReplRequestKind::Play { pattern, row },
            Err(error) => return error_response(&ReplError::Protocol(error)),
        },
        "/stop" => ReplRequestKind::Stop,
        "/repl/reset" => ReplRequestKind::Reset,
        "/status" => ReplRequestKind::Status,
        "/help" => ReplRequestKind::Help,
        _ => {
            return error_response(&ReplError::Protocol(format!(
                "unknown route {}",
                request.path
            )))
        }
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    if requests
        .send(ReplRequest {
            kind,
            response: sender,
            cancel: cancel.clone(),
        })
        .is_err()
    {
        return error_response(&ReplError::Protocol("repl service unavailable".to_string()));
    }
    match receiver.recv_timeout(RESPONSE_TIMEOUT) {
        Ok(response) => response,
        Err(_) => {
            // The application loop may not have reached this request yet; mark
            // it so an abandoned mutation does not run after the timeout.
            cancel.store(true, Ordering::SeqCst);
            error_response(&ReplError::Timeout)
        }
    }
}

fn write_response(stream: &mut TcpStream, response: ReplResponse) -> std::io::Result<()> {
    let serialized = serde_json::to_vec(&response.body).unwrap_or_default();
    let body = if serialized.len() > MAX_RESPONSE_BYTES {
        serde_json::to_vec(&serde_json::json!({
            "ok": false,
            "error": {
                "kind": "protocol",
                "message": "response exceeds 1 MiB",
            }
        }))
        .unwrap_or_default()
    } else {
        serialized
    };
    let status = if body.len() > MAX_RESPONSE_BYTES {
        500
    } else {
        response.status
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        status_text(status),
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(&body)?;
    stream.flush()
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}
