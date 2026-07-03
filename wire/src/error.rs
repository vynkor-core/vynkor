use std::fmt;
use std::io;

#[derive(Debug)]
pub enum WireError {
    Io(io::Error),
    Proto(prost::DecodeError),
    FrameMagicMismatch,
    FrameCrcMismatch,
    FrameReadTimeout,
    PayloadTooLarge(usize),
    Timeout,
    PermissionDenied(String),
    Internal(String),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Io(e) => write!(f, "io error: {}", e),
            WireError::Proto(e) => write!(f, "proto decode error: {}", e),
            WireError::FrameMagicMismatch => write!(f, "frame magic mismatch"),
            WireError::FrameCrcMismatch => write!(f, "frame crc mismatch"),
            WireError::FrameReadTimeout => write!(f, "timed out reading frame body"),
            WireError::PayloadTooLarge(n) => write!(f, "payload too large: {} bytes", n),
            WireError::Timeout => write!(f, "operation timed out"),
            WireError::PermissionDenied(perm) => write!(f, "permission denied: {}", perm),
            WireError::Internal(msg) => write!(f, "internal error: {}", msg),
        }
    }
}

impl std::error::Error for WireError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WireError::Io(e) => Some(e),
            WireError::Proto(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for WireError {
    fn from(e: io::Error) -> Self {
        WireError::Io(e)
    }
}

impl From<prost::DecodeError> for WireError {
    fn from(e: prost::DecodeError) -> Self {
        WireError::Proto(e)
    }
}
