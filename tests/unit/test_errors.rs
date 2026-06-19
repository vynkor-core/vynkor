use std::io;
use veyron::utils::errors::VeyronError;

#[test]
fn error_display_shows_message() {
    let e = VeyronError::PluginNotFound("weather".to_string());
    let s = format!("{}", e);
    assert!(s.contains("weather"), "display must include plugin id");
}

#[test]
fn from_io_error_converts() {
    let io_err = io::Error::new(io::ErrorKind::BrokenPipe, "pipe broken");
    let e: VeyronError = VeyronError::from(io_err);
    assert!(matches!(e, VeyronError::Io(_)));
}

#[test]
fn from_prost_decode_error_converts() {
    let decode_err = prost::DecodeError::new("bad bytes");
    let e: VeyronError = VeyronError::from(decode_err);
    assert!(matches!(e, VeyronError::Proto(_)));
}

#[test]
fn all_variants_constructible() {
    let _io = VeyronError::Io(io::Error::new(io::ErrorKind::Other, "x"));
    let _proto = VeyronError::Proto(prost::DecodeError::new("x"));
    let _magic = VeyronError::FrameMagicMismatch;
    let _crc = VeyronError::FrameCrcMismatch;
    let _large = VeyronError::PayloadTooLarge(2_000_000);
    let _not_found = VeyronError::PluginNotFound("x".to_string());
    let _dup = VeyronError::PluginAlreadyRegistered("x".to_string());
    let _denied = VeyronError::PermissionDenied("x".to_string());
    let _timeout = VeyronError::Timeout;
    let _internal = VeyronError::Internal("x".to_string());
}

#[test]
fn error_is_std_error() {
    fn assert_std_error<E: std::error::Error>(_: &E) {}
    let e = VeyronError::Timeout;
    assert_std_error(&e);
}
