//! action specs for dormant plugins, read from raw `plugin.json`.
//! the typed wire manifest keeps only name/permission/input/output, but
//! `get_manifest` consumers need description and risk before the plugin ever runs.

use crate::proto::vynkor::{ActionRisk, ActionSpec};

pub fn action_specs_from_manifest_json(raw: &serde_json::Value) -> Vec<ActionSpec> {
    let Some(actions) = raw.get("actions").and_then(|a| a.as_array()) else {
        return Vec::new();
    };
    actions.iter().filter_map(spec_from_entry).collect()
}

fn spec_from_entry(entry: &serde_json::Value) -> Option<ActionSpec> {
    // legacy manifests list bare action names
    if let Some(name) = entry.as_str() {
        return Some(ActionSpec {
            name: name.to_string(),
            ..Default::default()
        });
    }
    let name = entry.get("name")?.as_str()?.to_string();
    let text = |key: &str| {
        entry
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    Some(ActionSpec {
        name,
        description: text("description"),
        params_schema: entry
            .get("input")
            .map(|v| v.to_string())
            .unwrap_or_default(),
        risk: risk_from_str(&text("risk")) as i32,
        requires_confirmation: entry
            .get("requires_confirmation")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

// unknown strings stay Unknown, same as an unset proto field
fn risk_from_str(s: &str) -> ActionRisk {
    match s {
        "low" => ActionRisk::Low,
        "medium" => ActionRisk::Medium,
        "high" => ActionRisk::High,
        "critical" => ActionRisk::Critical,
        _ => ActionRisk::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_telegram_style_actions() {
        let raw = json!({
            "plugin_id": "telegram",
            "actions": [
                {
                    "name": "telegram.send",
                    "description": "send a message",
                    "risk": "high",
                    "requires_confirmation": true
                },
                {
                    "name": "telegram.read",
                    "input": {"type": "object", "properties": {"chat": {"type": "string"}}}
                },
                "telegram.legacy"
            ]
        });
        let specs = action_specs_from_manifest_json(&raw);
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].name, "telegram.send");
        assert_eq!(specs[0].description, "send a message");
        assert_eq!(specs[0].risk, ActionRisk::High as i32);
        assert!(specs[0].requires_confirmation);
        assert_eq!(specs[1].name, "telegram.read");
        assert_eq!(specs[1].risk, ActionRisk::Unknown as i32);
        assert!(!specs[1].requires_confirmation);
        let schema: serde_json::Value = serde_json::from_str(&specs[1].params_schema).unwrap();
        assert_eq!(schema["properties"]["chat"]["type"], "string");
        assert_eq!(specs[2].name, "telegram.legacy");
        assert!(specs[2].params_schema.is_empty());
    }

    #[test]
    fn unknown_risk_and_missing_actions_are_tolerated() {
        let raw = json!({"actions": [{"name": "a", "risk": "bogus"}]});
        assert_eq!(
            action_specs_from_manifest_json(&raw)[0].risk,
            ActionRisk::Unknown as i32
        );
        assert!(action_specs_from_manifest_json(&json!({})).is_empty());
    }
}
