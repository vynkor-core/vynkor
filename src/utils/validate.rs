use crate::utils::errors::VeyronError;

/// ma-17: one shape gate shared by slugs and plugin ids — non-empty ASCII
/// `[A-Za-z0-9._-]`, bounded length, never a bare path component. callers
/// layer their own extras (reserved names like `kernel`, tighter limits);
/// duplicating the charset here is how the two validators drifted apart
pub fn validate_identifier(id: &str, max_len: usize) -> Result<(), VeyronError> {
    if id.is_empty() {
        return Err(VeyronError::InvalidPluginId("must not be empty".into()));
    }
    if id.len() > max_len {
        return Err(VeyronError::InvalidPluginId(format!(
            "too long ({} bytes, max {max_len})",
            id.len()
        )));
    }
    if id == "." || id == ".." {
        return Err(VeyronError::InvalidPluginId(
            "'.' and '..' are reserved path components".into(),
        ));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(VeyronError::InvalidPluginId(
            "only ASCII letters, digits, '.', '-', '_' are allowed".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_identifiers_up_to_limit() {
        assert!(validate_identifier("my-plugin_1.2", 32).is_ok());
        assert!(validate_identifier(&"a".repeat(64), 64).is_ok());
        assert!(validate_identifier(&"a".repeat(65), 64).is_err());
    }

    #[test]
    fn rejects_empty_overlong_and_path_components() {
        assert!(validate_identifier("", 32).is_err());
        assert!(validate_identifier(".", 32).is_err());
        assert!(validate_identifier("..", 32).is_err());
        assert!(validate_identifier("a/b", 32).is_err());
        assert!(validate_identifier("a b", 32).is_err());
        assert!(validate_identifier("плагин", 32).is_err());
    }
}
