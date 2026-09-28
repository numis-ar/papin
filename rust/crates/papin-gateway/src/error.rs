use std::fmt;

#[derive(Debug)]
pub enum GatewayError {
    Io(std::io::Error),
    Config(String),
    NotFound(String),
    AlreadyExists(String),
    InvalidInput(String),
    AgentUnavailable(String),
    Timeout(String),
    Closed(String),
}

impl fmt::Display for GatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GatewayError::Io(e) => write!(f, "io error: {e}"),
            GatewayError::Config(m) => write!(f, "config error: {m}"),
            GatewayError::NotFound(m) => write!(f, "not found: {m}"),
            GatewayError::AlreadyExists(m) => write!(f, "already exists: {m}"),
            GatewayError::InvalidInput(m) => write!(f, "invalid input: {m}"),
            GatewayError::AgentUnavailable(m) => write!(f, "agent unavailable: {m}"),
            GatewayError::Timeout(m) => write!(f, "timeout: {m}"),
            GatewayError::Closed(m) => write!(f, "closed: {m}"),
        }
    }
}

impl std::error::Error for GatewayError {}

impl From<std::io::Error> for GatewayError {
    fn from(e: std::io::Error) -> Self {
        GatewayError::Io(e)
    }
}

impl From<serde_json::Error> for GatewayError {
    fn from(e: serde_json::Error) -> Self {
        GatewayError::InvalidInput(e.to_string())
    }
}

impl From<toml::de::Error> for GatewayError {
    fn from(e: toml::de::Error) -> Self {
        GatewayError::Config(e.to_string())
    }
}

impl From<toml::ser::Error> for GatewayError {
    fn from(e: toml::ser::Error) -> Self {
        GatewayError::Config(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, GatewayError>;
