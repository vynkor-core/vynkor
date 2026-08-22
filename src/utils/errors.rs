use std::fmt;
use std::io;

#[derive(Debug)]
pub enum VynkorError {
    Io(io::Error),
    Proto(prost::DecodeError),
    FrameMagicMismatch,
    FrameCrcMismatch,
    FrameReadTimeout,
    PayloadTooLarge(usize),
    PluginNotFound(String),
    PluginAlreadyRunning(String),
    PluginAlreadyRegistered(String),
    InvalidPluginId(String),
    PermissionDenied(String),
    Timeout,
    Internal(String),
    Incompatible(String),
    NetworkError(String),
    CacheError(String),
}

impl fmt::Display for VynkorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VynkorError::Io(e) => write!(f, "io error: {}", e),
            VynkorError::Proto(e) => write!(f, "proto decode error: {}", e),
            VynkorError::FrameMagicMismatch => write!(f, "frame magic mismatch"),
            VynkorError::FrameCrcMismatch => write!(f, "frame crc mismatch"),
            VynkorError::FrameReadTimeout => write!(f, "timed out reading frame body"),
            VynkorError::PayloadTooLarge(n) => write!(f, "payload too large: {} bytes", n),
            VynkorError::PluginNotFound(id) => write!(f, "plugin not found: {}", id),
            VynkorError::PluginAlreadyRunning(id) => write!(f, "plugin already running: {}", id),
            VynkorError::PluginAlreadyRegistered(id) => {
                write!(f, "plugin already registered: {}", id)
            }
            VynkorError::InvalidPluginId(reason) => {
                write!(f, "invalid plugin id: {}", reason)
            }
            VynkorError::PermissionDenied(perm) => write!(f, "permission denied: {}", perm),
            VynkorError::Timeout => write!(f, "operation timed out"),
            VynkorError::Internal(msg) => write!(f, "internal error: {}", msg),
            VynkorError::Incompatible(msg) => write!(f, "incompatible: {}", msg),
            VynkorError::NetworkError(msg) => write!(f, "network error: {}", msg),
            VynkorError::CacheError(msg) => write!(f, "cache error: {}", msg),
        }
    }
}

impl std::error::Error for VynkorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VynkorError::Io(e) => Some(e),
            VynkorError::Proto(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for VynkorError {
    fn from(e: io::Error) -> Self {
        VynkorError::Io(e)
    }
}

impl From<prost::DecodeError> for VynkorError {
    fn from(e: prost::DecodeError) -> Self {
        VynkorError::Proto(e)
    }
}

impl From<vynkor_wire::WireError> for VynkorError {
    fn from(e: vynkor_wire::WireError) -> Self {
        use vynkor_wire::WireError as W;
        match e {
            W::Io(e) => VynkorError::Io(e),
            W::Proto(e) => VynkorError::Proto(e),
            W::FrameMagicMismatch => VynkorError::FrameMagicMismatch,
            W::FrameCrcMismatch => VynkorError::FrameCrcMismatch,
            W::FrameReadTimeout => VynkorError::FrameReadTimeout,
            W::PayloadTooLarge(n) => VynkorError::PayloadTooLarge(n),
            W::Timeout => VynkorError::Timeout,
            W::PermissionDenied(p) => VynkorError::PermissionDenied(p),
            W::Internal(m) => VynkorError::Internal(m),
        }
    }
}
