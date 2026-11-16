//! K3: per-device grants. A paired device's authority over the host lives in
//! its `DeviceRecord`, not in its JWT, so it can be changed or revoked at any
//! time without re-pairing.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Grant {
    PluginsView,
    PluginsLogs,
    PluginsControl,
    PluginsInstall,
    DevicesManage,
}

pub type Grants = BTreeSet<Grant>;

impl Grant {
    pub const ALL: [Grant; 5] = [
        Grant::PluginsView,
        Grant::PluginsLogs,
        Grant::PluginsControl,
        Grant::PluginsInstall,
        Grant::DevicesManage,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Grant::PluginsView => "plugins.view",
            Grant::PluginsLogs => "plugins.logs",
            Grant::PluginsControl => "plugins.control",
            Grant::PluginsInstall => "plugins.install",
            Grant::DevicesManage => "devices.manage",
        }
    }

    /// Comma-separated list of every valid grant name, for error messages.
    pub fn valid_names() -> String {
        Grant::ALL
            .iter()
            .map(Grant::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl fmt::Display for Grant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Grant {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Grant::ALL
            .into_iter()
            .find(|g| g.as_str() == s)
            .ok_or_else(|| format!("unknown grant '{s}' (valid: {})", Grant::valid_names()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// agent only, no host control
    Guest,
    User,
    Owner,
}

impl Preset {
    pub fn grants(&self) -> Grants {
        match self {
            Preset::Guest => Grants::new(),
            Preset::User => [Grant::PluginsView, Grant::PluginsLogs, Grant::PluginsControl]
                .into_iter()
                .collect(),
            Preset::Owner => Grant::ALL.into_iter().collect(),
        }
    }
}

impl FromStr for Preset {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "guest" => Ok(Preset::Guest),
            "user" => Ok(Preset::User),
            "owner" => Ok(Preset::Owner),
            other => Err(format!(
                "unknown preset '{other}' (valid: guest, user, owner)"
            )),
        }
    }
}

/// Parse stored grant strings; unknown ones are skipped with a warning so one
/// bad entry can't lock the device out of the rest of its grants.
pub fn parse_lenient(raw: &[String]) -> Grants {
    raw.iter()
        .filter_map(|s| match s.parse::<Grant>() {
            Ok(g) => Some(g),
            Err(_) => {
                tracing::warn!(grant = %s, "ignoring unknown grant in device store");
                None
            }
        })
        .collect()
}

pub fn to_strings(grants: &Grants) -> Vec<String> {
    grants.iter().map(|g| g.as_str().to_string()).collect()
}
