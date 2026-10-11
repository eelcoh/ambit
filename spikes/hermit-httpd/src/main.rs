#[cfg(target_os = "hermit")]
use hermit as _;

fn main() {
    let server = tiny_http::Server::http("0.0.0.0:9975").expect("bind 0.0.0.0:9975");
    println!("listening on :9975");
    for request in server.incoming_requests() {
        let body = format!("Hello from a unikernel! You asked for {}\n", request.url());
        let _ = request.respond(tiny_http::Response::from_string(body));
    }
}
