//! The server platform. The platform owns the listener and concurrency; it calls the app's
//! [`Handler`] for every request. Capabilities come from `ambit-caps`, handed to the app's `init`.
#![forbid(unsafe_code)]

pub use http;

pub type Request = http::Request<Vec<u8>>;
pub type Response = http::Response<Vec<u8>>;

/// The app's entry point. Called concurrently; `&self` is read-only, mutable state lives in
/// capabilities. A panic becomes a 500 where the backend can catch it.
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, req: Request) -> Response;
}
