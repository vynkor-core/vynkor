use prost::Message;
use std::fs;
use std::path::Path;
use vynkor::proto::veyron::{envelope, Envelope, PluginRegister};

#[test]
fn envelope_round_trip_serializes_and_deserializes() {
    let register = PluginRegister {
        plugin_id: "weather".to_string(),
        version: "1.0.0".to_string(),
        description: "Weather plugin".to_string(),
        manifest: None,
        ..Default::default()
    };

    let env = Envelope {
        message_id: "msg-001".to_string(),
        timestamp: 1_000_000,
        sender_id: "weather".to_string(),
        payload: Some(envelope::Payload::PluginRegister(register)),
    };

    let mut buf = Vec::new();
    env.encode(&mut buf).expect("encode must succeed");

    let decoded = Envelope::decode(buf.as_slice()).expect("decode must succeed");

    assert_eq!(decoded.message_id, "msg-001");
    assert_eq!(decoded.sender_id, "weather");

    match decoded.payload {
        Some(envelope::Payload::PluginRegister(r)) => {
            assert_eq!(r.plugin_id, "weather");
            assert_eq!(r.version, "1.0.0");
        }
        _ => panic!("wrong payload variant after round-trip"),
    }
}

/// T-16 guard: `ActionStatus::ActionOk`/`CommandStatus::CommandOk` are the zero
/// value, so any `ActionResponse { .. }`/`KernelCommandAck { .. }` literal that
/// omits `status` silently reads back as success. Real proto messages have no
/// `#[non_exhaustive]` gate to catch this at compile time (`..Default::default()`
/// happily fills in the gap), so until the next wire-breaking renumber (see
/// ROADMAP T-16) this scans construction sites and fails if one appears without
/// an explicit `status:` field.
#[test]
fn action_response_and_command_ack_literals_set_status_explicitly() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();

    for (marker, needle) in [
        ("ActionResponse {", "status"),
        ("KernelCommandAck {", "status"),
    ] {
        for (path, block) in find_struct_literals(repo_root, marker) {
            if !block.contains(needle) {
                offenders.push(format!(
                    "{}: {} literal missing explicit `status:`",
                    path, marker
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "found construction sites relying on the zero-value default status (T-16):\n{}",
        offenders.join("\n")
    );
}

/// Walks tracked `.rs` files under the repo, skipping build/dependency dirs and
/// commented-out lines, and returns `(file, literal-block)` for every occurrence
/// of `marker` (e.g. `"ActionResponse {"`) found as real code.
fn find_struct_literals(repo_root: &Path, marker: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let skip_dirs = ["target", ".git", "node_modules", ".worktrees"];

    fn visit(dir: &Path, skip_dirs: &[&str], marker: &str, out: &mut Vec<(String, String)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if skip_dirs.contains(&name) {
                        continue;
                    }
                }
                visit(&path, skip_dirs, marker, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let Ok(content) = fs::read_to_string(&path) else {
                    continue;
                };
                for line in content.lines() {
                    let trimmed = line.trim_start();
                    if trimmed.starts_with("//") {
                        continue; // doc comments / commented-out examples
                    }
                    if let Some(idx) = line.find(marker) {
                        // Skip the marker when it's a quoted string literal (e.g. this
                        // very lint's own `("ActionResponse {", ..)` table), not real code.
                        if idx > 0 && line.as_bytes()[idx - 1] == b'"' {
                            continue;
                        }
                    }
                    if let Some(start) = content_find_from_line(&content, line, marker) {
                        let block = extract_brace_block(&content, start);
                        out.push((path.display().to_string(), block));
                    }
                }
            }
        }
    }

    fn content_find_from_line(content: &str, line: &str, marker: &str) -> Option<usize> {
        if !line.contains(marker) {
            return None;
        }
        let line_offset = content.find(line)?;
        Some(line_offset + line.find(marker)? + marker.len())
    }

    /// Given an offset just past the opening `{`, returns the text up to (and
    /// excluding) its matching closing `}`, respecting nested braces.
    fn extract_brace_block(content: &str, start: usize) -> String {
        let mut depth = 1i32;
        let mut end = start;
        for (i, ch) in content[start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        content[start..end].to_string()
    }

    visit(repo_root, &skip_dirs, marker, &mut out);
    out
}

#[test]
fn empty_envelope_encodes_to_nonempty_bytes() {
    let env = Envelope::default();
    let mut buf = Vec::new();
    env.encode(&mut buf).expect("encode must not fail");
    // protobuf default fields may produce 0 bytes — that's valid
    let decoded = Envelope::decode(buf.as_slice()).expect("decode must succeed");
    assert_eq!(decoded.message_id, "");
}
