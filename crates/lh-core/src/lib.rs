//! Lightweight plugin hosting and routing; no inference or HTTP dependencies.
pub mod agent;
pub mod cascade;
pub use cascade::Session;
pub use config::CascadeConfig;
pub mod config;
pub mod host;
pub mod lifecycle;
pub mod registry;
pub mod router;
pub mod selfinfo;
pub mod skills;
pub mod tools;

pub use config::Config;
pub use host::Host;
pub use registry::Registry;
pub use router::Engine;

#[derive(Debug, Clone)]
pub struct Error {
    pub code: i64,
    pub message: String,
}
impl Error {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(-32603, e.to_string())
    }
}
impl From<lh_protocol::json::Error> for Error {
    fn from(e: lh_protocol::json::Error) -> Self {
        Self::new(-32603, e.to_string())
    }
}
pub type Result<T> = std::result::Result<T, Error>;
