//! WebSocket connections on Linux/Mac (tungstenite over a TCP connection this crate opens),
//! apart from HTTP so a program without them doesn't carry a WebSocket client.
#![forbid(unsafe_code)]

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use ambit_caps_net::{HttpError, HttpErrorKind, Scope, WebSocket, WsConnection, WsMessage};
use ambit_lockdown::{Grant, Grants};

/// Each origin's port, and name lookups. Which hosts on those ports is the scope's job: the lock
/// allows TCP by port only.
impl Grants for NativeWebSocket {
    fn grants(&self) -> Vec<Grant> {
        let mut grants = vec![Grant::Resolve];
        grants.extend(
            self.scope
                .origins()
                .iter()
                .map(|o| Grant::Connect(o.port())),
        );
        grants
    }
}

pub struct WsOptions {
    /// For the TCP connection, per address the host resolves to.
    pub connect_timeout: Duration,
    /// For each read and write, the handshake included: `recv` waits at most this long.
    pub io_timeout: Duration,
    /// Larger incoming messages fail with `TooLarge`.
    pub max_message: usize,
}

impl Default for WsOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            io_timeout: Duration::from_secs(30),
            max_message: 64 * 1024,
        }
    }
}

pub struct NativeWebSocket {
    scope: Scope,
    options: WsOptions,
}

impl NativeWebSocket {
    pub fn new(scope: Scope, options: WsOptions) -> Self {
        Self { scope, options }
    }
}

impl WebSocket for NativeWebSocket {
    fn connect(&self, url: &str) -> Result<Box<dyn WsConnection>, HttpError> {
        let invalid = |m: String| HttpError::new(HttpErrorKind::Invalid, m);
        let network = |e: std::io::Error| HttpError::new(HttpErrorKind::Network, e.to_string());
        let uri: ambit_caps_net::http::Uri =
            url.parse().map_err(|e| invalid(format!("{url}: {e}")))?;
        self.scope.check_ws(&uri)?;
        let authority = uri
            .authority()
            .ok_or_else(|| invalid(format!("{url}: no host")))?;
        let host = authority
            .host()
            .trim_start_matches('[')
            .trim_end_matches(']');
        let port = authority.port_u16().unwrap_or(80);
        let mut stream = None;
        let mut last = None;
        for addr in (host, port).to_socket_addrs().map_err(network)? {
            match TcpStream::connect_timeout(&addr, self.options.connect_timeout) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let stream = stream.ok_or_else(|| match last {
            Some(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                HttpError::new(HttpErrorKind::Timeout, format!("{url}: {e}"))
            }
            Some(e) => HttpError::new(HttpErrorKind::Network, format!("{url}: {e}")),
            None => HttpError::new(HttpErrorKind::Network, format!("{url}: no address")),
        })?;
        stream
            .set_read_timeout(Some(self.options.io_timeout))
            .and_then(|()| stream.set_write_timeout(Some(self.options.io_timeout)))
            .and_then(|()| stream.set_nodelay(true))
            .map_err(network)?;
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(self.options.max_message))
            .max_frame_size(Some(self.options.max_message));
        let (ws, _) = tungstenite::client::client_with_config(url, stream, Some(config)).map_err(
            |e| match e {
                tungstenite::HandshakeError::Interrupted(_) => HttpError::new(
                    HttpErrorKind::Timeout,
                    format!("{url}: handshake timed out"),
                ),
                tungstenite::HandshakeError::Failure(e) => ws_error(e),
            },
        )?;
        Ok(Box::new(NativeWsConnection(ws)))
    }
}

struct NativeWsConnection(tungstenite::WebSocket<TcpStream>);

impl WsConnection for NativeWsConnection {
    fn send(&mut self, message: WsMessage) -> Result<(), HttpError> {
        let message = match message {
            WsMessage::Text(t) => tungstenite::Message::Text(t.into()),
            WsMessage::Binary(b) => tungstenite::Message::Binary(b.into()),
        };
        self.0.send(message).map_err(ws_error)
    }

    fn recv(&mut self) -> Result<WsMessage, HttpError> {
        loop {
            match self.0.read().map_err(ws_error)? {
                tungstenite::Message::Text(t) => {
                    return Ok(WsMessage::Text(t.as_str().to_string()));
                }
                tungstenite::Message::Binary(b) => return Ok(WsMessage::Binary(b.to_vec())),
                // tungstenite queues the pong; flushing sends it.
                tungstenite::Message::Ping(_) => self.0.flush().map_err(ws_error)?,
                tungstenite::Message::Pong(_) | tungstenite::Message::Frame(_) => {}
                tungstenite::Message::Close(_) => {
                    // tungstenite queues the close answer; flushing sends it.
                    let _ = self.0.flush();
                    return Err(HttpError::new(
                        HttpErrorKind::Closed,
                        "closed by the other side",
                    ));
                }
            }
        }
    }

    fn close(&mut self) {
        let _ = self.0.close(None);
        let _ = self.0.flush();
    }
}

fn ws_error(e: tungstenite::Error) -> HttpError {
    use tungstenite::Error;
    let kind = match &e {
        Error::ConnectionClosed
        | Error::AlreadyClosed
        | Error::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake) => {
            HttpErrorKind::Closed
        }
        Error::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            HttpErrorKind::Timeout
        }
        Error::Capacity(_) => HttpErrorKind::TooLarge,
        Error::Url(_) => HttpErrorKind::Invalid,
        _ => HttpErrorKind::Network,
    };
    HttpError::new(kind, e.to_string())
}
