//! A bounded, blocking HTTP/1.1 server.
//!
//! Resources are fixed up front: `threads` workers with a fixed stack size, and at most `queue`
//! accepted connections waiting for one. When the queue is full, new connections get an
//! immediate 503. Every connection has per-read timeouts and an overall deadline, carries one
//! request, and is closed after the response. Bodies need `Content-Length`; chunked is rejected.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{self, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use ambit_server::http::{self, Method, StatusCode, header};
use ambit_server::{Handler, Request, Response};

/// Requests with a larger body get 413.
pub const MAX_BODY: usize = 1024 * 1024;
/// Request line plus headers; larger gets 431.
pub const MAX_HEAD: usize = 16 * 1024;
const MAX_HEADERS: usize = 64;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const DEADLINE: Duration = Duration::from_secs(30);
const STACK_SIZE: usize = 256 * 1024;

pub struct Listen {
    pub addr: SocketAddr,
    /// Worker threads: the maximum number of requests handled at once.
    pub threads: usize,
    /// Accepted connections waiting for a worker; beyond this, 503.
    pub queue: usize,
}

/// Serves `handler` until the listener fails.
pub fn serve(handler: impl Handler, listen: Listen) -> io::Result<()> {
    let listener = TcpListener::bind(listen.addr)?;
    let (tx, rx) = mpsc::sync_channel::<TcpStream>(listen.queue);
    let rx = Arc::new(Mutex::new(rx));
    let handler = Arc::new(handler);

    for i in 0..listen.threads.max(1) {
        let rx = Arc::clone(&rx);
        let handler = Arc::clone(&handler);
        thread::Builder::new()
            .name(format!("http-{i}"))
            .stack_size(STACK_SIZE)
            .spawn(move || {
                loop {
                    // Hold the lock only to take the next connection.
                    let next = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    match next {
                        Ok(stream) => connection(&*handler, stream),
                        Err(_) => return,
                    }
                }
            })?;
    }
    tracing::info!(addr = %listen.addr, threads = listen.threads, queue = listen.queue, "listening");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(TrySendError::Full(stream)) = tx.try_send(stream) {
                    overloaded(stream);
                }
            }
            Err(e) => tracing::warn!(error = %e, "accept failed"),
        }
    }
    Ok(())
}

/// Answers 503 without blocking the accept loop.
fn overloaded(mut stream: TcpStream) {
    tracing::warn!("queue full, rejecting connection");
    let _ = stream.set_nonblocking(true);
    let _ = stream.write_all(
        b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
    );
}

fn connection(handler: &impl Handler, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let deadline = Instant::now() + DEADLINE;

    let (response, head_only) = match read_request(&mut stream, deadline) {
        Ok(request) => {
            let head_only = request.method() == Method::HEAD;
            (call(handler, request), head_only)
        }
        Err(Failure::Closed) => return,
        Err(Failure::Status(status)) => (status_only(status), false),
    };
    if let Err(e) = write_response(&mut stream, response, head_only) {
        tracing::debug!(error = %e, "failed to send response");
    }
    // Drain briefly so unread input doesn't turn the close into a reset that loses the response.
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = io::copy(&mut (&mut stream).take(MAX_BODY as u64), &mut io::sink());
}

fn call(handler: &impl Handler, request: Request) -> Response {
    let uri = request.uri().clone();
    panic::catch_unwind(AssertUnwindSafe(|| handler.handle(request))).unwrap_or_else(|_| {
        tracing::error!(%uri, "handler panicked");
        status_only(StatusCode::INTERNAL_SERVER_ERROR)
    })
}

#[derive(Debug, PartialEq)]
enum Failure {
    /// The client went away or timed out before sending a request: nothing to answer.
    Closed,
    Status(StatusCode),
}

impl From<StatusCode> for Failure {
    fn from(status: StatusCode) -> Self {
        Failure::Status(status)
    }
}

fn read_request(stream: &mut (impl Read + Write), deadline: Instant) -> Result<Request, Failure> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];

    let head_len = loop {
        let n = read_some(stream, &mut chunk, deadline)?;
        if n == 0 {
            return Err(if buf.is_empty() {
                Failure::Closed
            } else {
                StatusCode::BAD_REQUEST.into()
            });
        }
        buf.extend_from_slice(&chunk[..n]);
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        match httparse::Request::new(&mut headers).parse(&buf) {
            Ok(httparse::Status::Complete(len)) => break len,
            Ok(httparse::Status::Partial) if buf.len() < MAX_HEAD => continue,
            Ok(httparse::Status::Partial) | Err(httparse::Error::TooManyHeaders) => {
                return Err(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE.into());
            }
            Err(_) => return Err(StatusCode::BAD_REQUEST.into()),
        }
    };
    if head_len > MAX_HEAD {
        return Err(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE.into());
    }

    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut parsed = httparse::Request::new(&mut headers);
    parsed
        .parse(&buf[..head_len])
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let mut builder = http::Request::builder()
        .method(parsed.method.unwrap_or_default())
        .uri(parsed.path.unwrap_or_default())
        .version(if parsed.version == Some(0) {
            http::Version::HTTP_10
        } else {
            http::Version::HTTP_11
        });
    let mut length: Option<usize> = None;
    let mut expect_continue = false;
    for h in parsed.headers.iter() {
        if h.name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(StatusCode::NOT_IMPLEMENTED.into());
        }
        if h.name.eq_ignore_ascii_case("content-length") {
            let n = std::str::from_utf8(h.value)
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .ok_or(StatusCode::BAD_REQUEST)?;
            if length.is_some_and(|l| l != n) {
                return Err(StatusCode::BAD_REQUEST.into());
            }
            length = Some(n);
        }
        if h.name.eq_ignore_ascii_case("expect") && h.value.eq_ignore_ascii_case(b"100-continue") {
            expect_continue = true;
        }
        builder = builder.header(h.name, h.value);
    }

    let length = length.unwrap_or(0);
    if length > MAX_BODY {
        return Err(StatusCode::PAYLOAD_TOO_LARGE.into());
    }
    let mut body = buf.split_off(head_len);
    body.truncate(length);
    if body.len() < length && expect_continue {
        stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .map_err(|_| Failure::Closed)?;
    }
    while body.len() < length {
        let want = (length - body.len()).min(chunk.len());
        let n = read_some(stream, &mut chunk[..want], deadline)?;
        if n == 0 {
            return Err(StatusCode::BAD_REQUEST.into());
        }
        body.extend_from_slice(&chunk[..n]);
    }
    builder
        .body(body)
        .map_err(|_| StatusCode::BAD_REQUEST.into())
}

fn read_some(stream: &mut impl Read, buf: &mut [u8], deadline: Instant) -> Result<usize, Failure> {
    if Instant::now() > deadline {
        return Err(StatusCode::REQUEST_TIMEOUT.into());
    }
    match stream.read(buf) {
        Ok(n) => Ok(n),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Err(StatusCode::REQUEST_TIMEOUT.into())
        }
        Err(_) => Err(Failure::Closed),
    }
}

fn write_response(stream: &mut impl Write, response: Response, head_only: bool) -> io::Result<()> {
    let (parts, body) = response.into_parts();
    let mut out = Vec::with_capacity(256 + body.len());
    let reason = parts.status.canonical_reason().unwrap_or("");
    write!(out, "HTTP/1.1 {} {}\r\n", parts.status.as_u16(), reason)?;
    for (name, value) in &parts.headers {
        // Framing is the server's job.
        if name == header::CONTENT_LENGTH
            || name == header::CONNECTION
            || name == header::TRANSFER_ENCODING
        {
            continue;
        }
        out.extend_from_slice(name.as_str().as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    write!(
        out,
        "content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )?;
    if !head_only {
        out.extend_from_slice(&body);
    }
    stream.write_all(&out)?;
    stream.flush()
}

fn status_only(status: StatusCode) -> Response {
    let mut response = Response::new(Vec::new());
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client that has sent `input`; collects what the server writes.
    struct Client {
        input: io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Client {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Client {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn parse(input: impl Into<Vec<u8>>) -> (Result<Request, Failure>, Vec<u8>) {
        let mut client = Client {
            input: io::Cursor::new(input.into()),
            output: Vec::new(),
        };
        let result = read_request(&mut client, Instant::now() + DEADLINE);
        (result, client.output)
    }

    fn status(input: impl Into<Vec<u8>>) -> Failure {
        parse(input).0.expect_err("should fail")
    }

    #[test]
    fn get_with_headers() {
        let (req, _) = parse("GET /a?b=c HTTP/1.1\r\nHost: x\r\nX-Y: z\r\n\r\n");
        let req = req.unwrap();
        assert_eq!(
            (req.method(), req.uri().to_string().as_str()),
            (&Method::GET, "/a?b=c")
        );
        assert_eq!(req.headers()["x-y"], "z");
        assert!(req.body().is_empty());
    }

    #[test]
    fn body_by_content_length() {
        let (req, _) = parse("POST /l HTTP/1.1\r\nContent-Length: 5\r\n\r\nhelloEXTRA");
        assert_eq!(req.unwrap().body(), b"hello");
    }

    #[test]
    fn expect_continue_is_answered() {
        let (req, out) =
            parse("POST /l HTTP/1.1\r\nContent-Length: 2\r\nExpect: 100-continue\r\n\r\nhi");
        // Body arrived with the head here, so no 100 needed.
        assert_eq!(req.unwrap().body(), b"hi");
        assert!(out.is_empty());
    }

    #[test]
    fn limits_and_rejections() {
        assert_eq!(status(""), Failure::Closed);
        assert_eq!(
            status("GARBAGE\r\n\r\n"),
            Failure::Status(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            status("GET / HTTP/1.1\r\nHost: x"),
            Failure::Status(StatusCode::BAD_REQUEST)
        );
        let big = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(MAX_HEAD));
        assert_eq!(
            status(big),
            Failure::Status(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE)
        );
        let many: String = (0..100).map(|i| format!("H{i}: v\r\n")).collect();
        let many = format!("GET / HTTP/1.1\r\n{many}\r\n");
        assert_eq!(
            status(many),
            Failure::Status(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE)
        );
        let len = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert_eq!(status(len), Failure::Status(StatusCode::PAYLOAD_TOO_LARGE));
        let chunked = "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert_eq!(
            status(chunked),
            Failure::Status(StatusCode::NOT_IMPLEMENTED)
        );
        let conflict = "POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab";
        assert_eq!(status(conflict), Failure::Status(StatusCode::BAD_REQUEST));
        let short = "POST / HTTP/1.1\r\nContent-Length: 10\r\n\r\nabc";
        assert_eq!(status(short), Failure::Status(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn response_framing_is_the_servers() {
        let mut response = Response::new(b"hi".to_vec());
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, "999".parse().unwrap());
        response.headers_mut().insert("x-a", "b".parse().unwrap());
        let mut out = Vec::new();
        write_response(&mut out, response, false).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert_eq!(
            out,
            "HTTP/1.1 200 OK\r\nx-a: b\r\ncontent-length: 2\r\nconnection: close\r\n\r\nhi"
        );
    }

    #[test]
    fn head_has_no_body() {
        let mut out = Vec::new();
        write_response(&mut out, Response::new(b"hi".to_vec()), true).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .ends_with("content-length: 2\r\nconnection: close\r\n\r\n")
        );
    }
}
