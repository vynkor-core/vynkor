use std::fmt;
use std::io;

#[derive(Debug)]
#[allow(dead_code)]
pub enum VeyronError {
    Io(io::Error),
    Proto(prost::DecodeError),
    FrameMagicMismatch,
    FrameCrcMismatch,
    FrameReadTimeout,
    PayloadTooLarge(usize),
    PluginNotFound(String),
    PluginAlreadyRegistered(String),
    PermissionDenied(String),
    Timeout,
    Internal(String),
}

impl fmt::Display for VeyronError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VeyronError::Io(e) => write!(f, "io error: {}", e),
            VeyronError::Proto(e) => write!(f, "proto decode error: {}", e),
            VeyronError::FrameMagicMismatch => write!(f, "frame magic mismatch"),
            VeyronError::FrameCrcMismatch => write!(f, "frame crc mismatch"),
            VeyronError::FrameReadTimeout => write!(f, "timed out reading frame body"),
            VeyronError::PayloadTooLarge(n) => write!(f, "payload too large: {} bytes", n),
            VeyronError::PluginNotFound(id) => write!(f, "plugin not found: {}", id),
            VeyronError::PluginAlreadyRegistered(id) => {
                write!(f, "plugin already registered: {}", id)
            }
            VeyronError::PermissionDenied(perm) => write!(f, "permission denied: {}", perm),
            VeyronError::Timeout => write!(f, "operation timed out"),
            VeyronError::Internal(msg) => write!(f, "internal error: {}", msg),
        }
    }
}

impl std::error::Error for VeyronError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VeyronError::Io(e) => Some(e),
            VeyronError::Proto(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for VeyronError {
    fn from(e: io::Error) -> Self {
        VeyronError::Io(e)
    }
}

impl From<prost::DecodeError> for VeyronError {
    fn from(e: prost::DecodeError) -> Self {
        VeyronError::Proto(e)
    }
}
