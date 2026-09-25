//! CD-01: single-use pairing tickets.
//!
//! A ticket is 32 CSPRNG bytes (base64url, 43 chars) that an admin hands to a
//! device out of band (QR/link). The device trades it once, via
//! `POST /devices/consume`, for a per-device credential. Only the SHA-256 of
//! the ticket is persisted — `tickets.json` leaking yields nothing
//! redeemable. Timestamps are unix secs so a kernel restart neither resets
//! nor extends a ticket's TTL.
//!
//! Only the kernel process touches this file (the CLI goes through the API),
//! so an in-process mutex is enough to make consume atomic.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::auth::device_store::write_private_atomic;
use crate::utils::errors::{TicketRejection, VynkorError};

const STORE_FILE: &str = "tickets.json";
const TICKET_BYTES: usize = 32;
/// base64url (no pad) length of a 32-byte ticket.
pub const TICKET_LEN: usize = 43;
/// used/expired rows linger this long so a late retry still gets 409/410
/// instead of a misleading 404
pub const STALE_RETENTION_SECS: u64 = 3600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketRecord {
    /// hex sha-256 of the raw ticket — the raw value never touches disk
    pub ticket_sha256: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub used_at: Option<u64>,
    /// display name given to the device minted from this ticket
    pub name: Option<String>,
    /// advertise url chosen at pair time; echoed into the consume payload
    pub ws: String,
}

impl TicketRecord {
    fn is_stale(&self, now: u64) -> bool {
        let last = self.expires_at.max(self.used_at.unwrap_or(0));
        now >= last.saturating_add(STALE_RETENTION_SECS)
    }
}

/// Returned once at issue time — the only place the raw ticket exists.
pub struct IssuedTicket {
    pub ticket: String,
    pub record: TicketRecord,
}

pub struct TicketStore {
    path: PathBuf,
    // serializes read-modify-write so two consumes can't both see "unused"
    lock: Mutex<()>,
}

impl TicketStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(STORE_FILE),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn issue(
        &self,
        ttl_secs: u64,
        name: Option<String>,
        ws: String,
    ) -> Result<IssuedTicket, VynkorError> {
        self.issue_at(now_secs(), ttl_secs, name, ws)
    }

    /// `issue` with an explicit clock — lets tests mint already-expired
    /// tickets without sleeping.
    pub fn issue_at(
        &self,
        now: u64,
        ttl_secs: u64,
        name: Option<String>,
        ws: String,
    ) -> Result<IssuedTicket, VynkorError> {
        let ticket = random_ticket();
        let record = TicketRecord {
            ticket_sha256: hash_ticket(&ticket),
            created_at: now,
            expires_at: now.saturating_add(ttl_secs),
            used_at: None,
            name,
            ws,
        };
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows = self.read_all()?;
        // lazy sweep: the file only grows on issue, so pruning here bounds it
        rows.retain(|r| !r.is_stale(now));
        rows.push(record.clone());
        self.write_all(&rows)?;
        Ok(IssuedTicket { ticket, record })
    }

    pub fn consume(&self, ticket: &str) -> Result<TicketRecord, VynkorError> {
        self.consume_at(ticket, now_secs())
    }

    /// Atomically mark the ticket used. Exactly one caller ever gets `Ok` for
    /// a given ticket.
    pub fn consume_at(&self, ticket: &str, now: u64) -> Result<TicketRecord, VynkorError> {
        // wrong shape can't be a real ticket — skip the disk read entirely
        if ticket.len() != TICKET_LEN {
            return Err(VynkorError::Ticket(TicketRejection::Unknown));
        }
        let hash = hash_ticket(ticket);
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows = self.read_all()?;
        // lookup by hash: no byte-wise compare against the raw secret happens
        let row = rows
            .iter_mut()
            .find(|r| r.ticket_sha256 == hash)
            .ok_or(VynkorError::Ticket(TicketRejection::Unknown))?;
        if row.used_at.is_some() {
            return Err(VynkorError::Ticket(TicketRejection::AlreadyUsed));
        }
        if now >= row.expires_at {
            return Err(VynkorError::Ticket(TicketRejection::Expired));
        }
        row.used_at = Some(now);
        let consumed = row.clone();
        self.write_all(&rows)?;
        Ok(consumed)
    }

    /// Drop rows past expiry/use plus `STALE_RETENTION_SECS`. Returns how
    /// many were removed.
    pub fn sweep_expired(&self, now: u64) -> Result<usize, VynkorError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows = self.read_all()?;
        let before = rows.len();
        rows.retain(|r| !r.is_stale(now));
        let removed = before - rows.len();
        if removed > 0 {
            self.write_all(&rows)?;
        }
        Ok(removed)
    }

    fn read_all(&self) -> Result<Vec<TicketRecord>, VynkorError> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(VynkorError::Io(e)),
        };
        if raw.trim().is_empty() {
            return Ok(Vec::new());
        }
        serde_json::from_str(&raw).map_err(|e| VynkorError::Internal(e.to_string()))
    }

    fn write_all(&self, rows: &[TicketRecord]) -> Result<(), VynkorError> {
        let json =
            serde_json::to_string_pretty(rows).map_err(|e| VynkorError::Internal(e.to_string()))?;
        write_private_atomic(&self.path, json.as_bytes())
    }
}

fn random_ticket() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; TICKET_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn hash_ticket(ticket: &str) -> String {
    Sha256::digest(ticket.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_is_43_char_base64url() {
        let t = random_ticket();
        assert_eq!(t.len(), TICKET_LEN);
        assert!(t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(t, random_ticket());
    }

    #[test]
    fn expiry_is_checked_before_marking_used() {
        let dir = tempfile::tempdir().unwrap();
        let store = TicketStore::new(dir.path());
        let t = store.issue_at(100, 10, None, "ws://h/ws".into()).unwrap();
        assert!(matches!(
            store.consume_at(&t.ticket, 110),
            Err(VynkorError::Ticket(TicketRejection::Expired))
        ));
        // an expired attempt must not burn it into "used"
        assert!(matches!(
            store.consume_at(&t.ticket, 110),
            Err(VynkorError::Ticket(TicketRejection::Expired))
        ));
        assert!(store.consume_at(&t.ticket, 109).is_ok());
    }
}
