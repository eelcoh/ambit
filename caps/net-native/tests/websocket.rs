//! NativeWebSocket against a local server: scope first, then real connections.

use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use ambit_caps_net::{HttpErrorKind, Scope, WebSocket, WsMessage};
use ambit_caps_net_native::{NativeWebSocket, WsOptions};
use tungstenite::Message;

/// Accepts one connection and runs `server` on it.
fn serve_once(
    server: impl FnOnce(tungstenite::WebSocket<std::net::TcpStream>) + Send + 'static,
) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (conn, _) = listener.accept().unwrap();
        server(tungstenite::accept(conn).unwrap());
    });
    (port, handle)
}

fn client(port: u16, options: WsOptions) -> NativeWebSocket {
    let scope = Scope::parse([format!("ws://127.0.0.1:{port}").as_str()]).unwrap();
    NativeWebSocket::new(scope, options)
}

#[test]
fn outside_the_scope_nothing_connects() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let ws = client(port + 1, WsOptions::default());
    for url in [
        format!("ws://127.0.0.1:{port}/"),
        format!("ws://localhost:{}/", port + 1),
    ] {
        let e = ws.connect(&url).map(drop).unwrap_err();
        assert_eq!(e.kind, HttpErrorKind::Denied, "{url}: {e}");
    }
    let e = ws
        .connect(&format!("http://127.0.0.1:{}/", port + 1))
        .map(drop)
        .unwrap_err();
    assert_eq!(e.kind, HttpErrorKind::Invalid);
    assert!(listener.accept().is_err(), "nothing may have connected");
}

#[test]
fn messages_both_ways_pings_answered_close_seen() {
    let (port, server) = serve_once(|mut ws| {
        let Message::Text(start) = ws.read().unwrap() else {
            panic!("expected text")
        };
        assert_eq!(start.as_str(), "START:book.epub:5:/");
        ws.send(Message::Ping(b"are you there".to_vec().into()))
            .unwrap();
        ws.send(Message::Text("READY".into())).unwrap();
        // The client's pong to our ping comes first.
        assert!(matches!(ws.read().unwrap(), Message::Pong(_)));
        let Message::Binary(data) = ws.read().unwrap() else {
            panic!("expected binary")
        };
        assert_eq!(&data[..], b"12345");
        ws.send(Message::Binary(vec![1, 2, 3].into())).unwrap();
        ws.close(None).unwrap();
        // Until the client's close answer arrives.
        while !matches!(ws.read(), Ok(Message::Close(_)) | Err(_)) {}
    });
    let ws = client(port, WsOptions::default());
    let mut conn = ws.connect(&format!("ws://127.0.0.1:{port}/")).unwrap();
    conn.send(WsMessage::Text("START:book.epub:5:/".into()))
        .unwrap();
    assert_eq!(conn.recv().unwrap(), WsMessage::Text("READY".into()));
    conn.send(WsMessage::Binary(b"12345".to_vec())).unwrap();
    assert_eq!(conn.recv().unwrap(), WsMessage::Binary(vec![1, 2, 3]));
    assert_eq!(conn.recv().unwrap_err().kind, HttpErrorKind::Closed);
    server.join().unwrap();
}

#[test]
fn oversized_messages_and_silence_fail() {
    let (port, server) = serve_once(|mut ws| {
        ws.send(Message::Binary(vec![0; 2048].into())).unwrap();
        let _ = ws.read();
    });
    let options = WsOptions {
        max_message: 1024,
        ..WsOptions::default()
    };
    let mut conn = client(port, options)
        .connect(&format!("ws://127.0.0.1:{port}/"))
        .unwrap();
    assert_eq!(conn.recv().unwrap_err().kind, HttpErrorKind::TooLarge);
    drop(conn);
    server.join().unwrap();

    let (port, server) = serve_once(|mut ws| {
        let _ = ws.read();
    });
    let options = WsOptions {
        io_timeout: Duration::from_millis(200),
        ..WsOptions::default()
    };
    let mut conn = client(port, options)
        .connect(&format!("ws://127.0.0.1:{port}/"))
        .unwrap();
    assert_eq!(conn.recv().unwrap_err().kind, HttpErrorKind::Timeout);
    conn.close();
    server.join().unwrap();
}
