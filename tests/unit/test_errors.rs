use std::io;
use vynkor::utils::errors::VynkorError;

#[test]
fn error_display_shows_message() {
    let e = VynkorError::PluginNotFound("weather".to_string());
    let s = format!("{}", e);
    assert!(s.contains("weather"), "display must include plugin id");
}

#[test]
fn from_io_error_converts() {
    let io_err = io::Error::new(io::ErrorKind::BrokenPipe, "pipe broken");
    let e: VynkorError = VynkorError::from(io_err);
    assert!(matches!(e, VynkorError::Io(_)));
}

#[test]
fn from_prost_decode_error_converts() {
    let decode_err = prost::DecodeError::new("bad bytes");
    let e: VynkorError = VynkorError::from(decode_err);
    assert!(matches!(e, VynkorError::Proto(_)));
}

#[test]
fn all_variants_constructible() {
    let _io = VynkorError::Io(io::Error::other("x"));
    let _proto = VynkorError::Proto(prost::DecodeError::new("x"));
    let _magic = VynkorError::FrameMagicMismatch;
    let _crc = VynkorError::FrameCrcMismatch;
    let _large = VynkorError::PayloadTooLarge(2_000_000);
    let _not_found = VynkorError::PluginNotFound("x".to_string());
    let _dup = VynkorError::PluginAlreadyRegistered("x".to_string());
    let _denied = VynkorError::PermissionDenied("x".to_string());
    let _timeout = VynkorError::Timeout;
    let _internal = VynkorError::Internal("x".to_string());
}

#[test]
fn error_is_std_error() {
    fn assert_std_error<E: std::error::Error>(_: &E) {}
    let e = VynkorError::Timeout;
    assert_std_error(&e);
}
