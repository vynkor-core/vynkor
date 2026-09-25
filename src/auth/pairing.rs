//! CD-01: pairing payloads and the ticket-exchange service behind
//! `POST /devices/pair` / `POST /devices/consume`.
//!
//! `vyn device connect` (LAN admin, reads the store directly) and the ticket
//! flow produce the *same* `PairPayload` v2, so the agent has one parser.
//! The master jwt_secret is only ever used here to sign per-device tokens —
//! it is never serialized.

use std::io::Write;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use serde::{Deserialize, Serialize};

use crate::auth::device_store::DeviceStore;
use crate::auth::ticket_store::TicketStore;
use crate::utils::errors::VynkorError;

pub const DEFAULT_TICKET_TTL_SECS: u64 = 300;
pub const MAX_TICKET_TTL_SECS: u64 = 3600;
const MAX_NAME_LEN: usize = 64;
// one scan = one consume; this only caps disk churn from junk requests
const CONSUME_PER_SEC: u32 = 2;
const CONSUME_BURST: u32 = 10;

/// The JSON document encoded (deflate-compressed, base64url) as
/// `vynkor://pair?d=...&z=1`. The app decodes it into a host profile and
/// connects. `cert_pem` is present only when the kernel serves TLS (D-07
/// default), so the agent can pin it and use `wss://` against a self-signed
/// cert instead of falling back to `tls: false`.
#[derive(Debug, Serialize, Deserialize)]
pub struct PairPayload {
    pub v: u32,
    pub name: String,
    pub host_url: String,
    pub device_id: String,
    pub jwt_token: String,
    /// per-device secret issued by THIS host — never the master jwt_secret
    pub device_secret: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_pem: Option<String>,
}

/// QR/link variant for the ticket flow: `ticket` replaces
/// `jwt_token`+`device_secret`; the agent consumes it over HTTP.
#[derive(Debug, Serialize, Deserialize)]
pub struct TicketLinkPayload {
    pub v: u32,
    pub ws: String,
    pub ticket: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_pem: Option<String>,
}

/// Body of `POST /devices/pair` — every field optional.
#[derive(Debug, Default, Deserialize)]
pub struct PairRequest {
    pub ttl_secs: Option<u64>,
    pub name: Option<String>,
    /// advertise host override (LAN ip, tailscale name, or full url);
    /// default auto-detects the LAN address
    pub host: Option<String>,
}

/// Response of `POST /devices/pair`.
#[derive(Debug, Serialize, Deserialize)]
pub struct TicketView {
    pub v: u32,
    pub ticket: String,
    pub ws: String,
    pub ttl_secs: u64,
    /// unix secs
    pub expires_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_pem: Option<String>,
}

/// `vynkor://pair?z=1&d=<base64url(zlib(json))>` — deflate because the cert
/// dominates the payload and the in-app scanner chokes past QR v~33.
pub fn encode_pair_link<T: Serialize>(payload: &T) -> Result<String, VynkorError> {
    let json = serde_json::to_string(payload).map_err(|e| VynkorError::Internal(e.to_string()))?;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(json.as_bytes())?;
    let compressed = encoder.finish()?;
    Ok(format!(
        "vynkor://pair?z=1&d={}",
        URL_SAFE_NO_PAD.encode(compressed)
    ))
}

/// What a device agent needs to call host actions.
pub fn default_device_permissions() -> Vec<String> {
    vec![
        "PERMISSION_IPC_SEND".to_string(),
        "PERMISSION_EVENT_PUBLISH".to_string(),
    ]
}

pub struct PairingConfig {
    /// signs per-device tokens; never serialized
    pub jwt_secret: String,
    pub audience: String,
    pub port: u16,
    pub tls: bool,
    /// served cert to pin in the payload (`None` → client uses system roots)
    pub cert_path: Option<PathBuf>,
    /// lifetime of the credential minted on consume
    pub device_ttl_secs: u64,
}

pub struct PairingService {
    cfg: PairingConfig,
    devices: Arc<DeviceStore>,
    tickets: TicketStore,
    consume_limiter: DefaultDirectRateLimiter,
}

impl PairingService {
    pub fn new(data_dir: &Path, devices: Arc<DeviceStore>, cfg: PairingConfig) -> Self {
        Self {
            cfg,
            devices,
            tickets: TicketStore::new(data_dir),
            consume_limiter: direct_limiter(CONSUME_PER_SEC, CONSUME_BURST),
        }
    }

    pub fn with_consume_quota(mut self, per_sec: u32, burst: u32) -> Self {
        self.consume_limiter = direct_limiter(per_sec, burst);
        self
    }

    pub fn tickets(&self) -> &TicketStore {
        &self.tickets
    }

    /// Global (not per-ip) budget: the server has no peer address behind a
    /// reverse proxy anyway, and 256-bit tickets aren't brute-forceable —
    /// this only bounds file churn.
    pub fn allow_consume(&self) -> bool {
        self.consume_limiter.check().is_ok()
    }

    pub fn create_ticket(&self, req: PairRequest) -> Result<TicketView, VynkorError> {
        let ttl_secs = req
            .ttl_secs
            .unwrap_or(DEFAULT_TICKET_TTL_SECS)
            .clamp(1, MAX_TICKET_TTL_SECS);
        let name = req
            .name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty());
        if name
            .as_ref()
            .is_some_and(|n| n.chars().count() > MAX_NAME_LEN)
        {
            return Err(VynkorError::InvalidInput(format!(
                "name longer than {MAX_NAME_LEN} chars"
            )));
        }
        let ws = crate::utils::url::resolve_advertise_url(
            self.cfg.port,
            self.cfg.tls,
            req.host.as_deref(),
        )?;
        let issued = self.tickets.issue(ttl_secs, name, ws.clone())?;
        Ok(TicketView {
            v: 2,
            ticket: issued.ticket,
            ws,
            ttl_secs,
            expires_at: issued.record.expires_at,
            cert_pem: self.cert_pem()?,
        })
    }

    /// Trade a ticket for a fresh per-device credential. The ticket is burned
    /// before anything is minted, so a failure afterwards fails closed.
    pub fn consume(&self, ticket: &str) -> Result<PairPayload, VynkorError> {
        let record = self.tickets.consume(ticket)?;
        let device_id = self.fresh_device_id()?;
        let name = record.name.unwrap_or_else(|| device_id.clone());
        let device_secret = self
            .devices
            .issue(&device_id, &name, self.cfg.device_ttl_secs)?;
        let jwt_token = crate::auth::jwt::mint_device_token(
            self.cfg.jwt_secret.as_bytes(),
            &device_id,
            default_device_permissions(),
            vec![],
            self.cfg.device_ttl_secs,
            &self.cfg.audience,
        )?;
        tracing::info!(device_id = %device_id, "paired device via ticket");
        Ok(PairPayload {
            v: 2,
            name,
            host_url: record.ws,
            device_id,
            jwt_token,
            device_secret,
            cert_pem: self.cert_pem()?,
        })
    }

    fn cert_pem(&self) -> Result<Option<String>, VynkorError> {
        if !self.cfg.tls {
            return Ok(None);
        }
        match &self.cfg.cert_path {
            Some(p) if p.exists() => Ok(Some(std::fs::read_to_string(p)?)),
            _ => Ok(None),
        }
    }

    // issue() on an existing id rotates that device — never collide
    fn fresh_device_id(&self) -> Result<String, VynkorError> {
        loop {
            let id = random_device_id();
            if self.devices.get(&id)?.is_none() {
                return Ok(id);
            }
        }
    }
}

fn direct_limiter(per_sec: u32, burst: u32) -> DefaultDirectRateLimiter {
    let per_sec = NonZeroU32::new(per_sec.max(1)).expect("max(1) is non-zero");
    let burst = NonZeroU32::new(burst.max(1)).expect("max(1) is non-zero");
    RateLimiter::direct(Quota::per_second(per_sec).allow_burst(burst))
}

/// 48 random bits — wider than `vyn device connect`'s ids since ticket
/// devices are minted unattended.
fn random_device_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 6];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!(
        "dev-{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_link_round_trips_through_zlib() {
        let link = encode_pair_link(&TicketLinkPayload {
            v: 2,
            ws: "wss://h:1/ws".into(),
            ticket: "t".repeat(43),
            cert_pem: None,
        })
        .unwrap();
        let enc = link.strip_prefix("vynkor://pair?z=1&d=").unwrap();
        let compressed = URL_SAFE_NO_PAD.decode(enc).unwrap();
        let mut json = String::new();
        use std::io::Read;
        flate2::read::ZlibDecoder::new(&compressed[..])
            .read_to_string(&mut json)
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["v"], 2);
        assert_eq!(v["ticket"].as_str().unwrap().len(), 43);
        assert!(v.get("jwt_token").is_none());
        assert!(v.get("cert_pem").is_none());
    }

    #[test]
    fn random_device_id_is_prefixed_and_wide() {
        let id = random_device_id();
        assert!(id.starts_with("dev-"));
        assert_eq!(id.len(), 4 + 12);
    }
}
