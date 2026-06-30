use crate::auth::frame_mac::{compute_tag, verify_tag};
use crate::ipc::framing::{
    parse_frag_header, read_frame, serialize_header, write_frame_raw, Frame, FLAG_FRAGMENTED,
    FLAG_MAC_PRESENT, FRAG_HEADER_SIZE,
};
use crate::ipc::messages::IncomingMessage;
use crate::utils::errors::VeyronError;
use metrics::counter;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// How long an incomplete fragment set is retained before it is silently
/// discarded. Prevents fragment-based memory exhaustion on a long-lived
/// connection where a sender opens a stream and never finishes it.
const FRAGMENT_TIMEOUT: Duration = Duration::from_secs(30);

struct ReassemblyBuf {
    fragments: HashMap<u16, Vec<u8>>,
    total: u16,
    /// Target field from the first fragment — carried into the reassembled frame.
    target: [u8; 32],
    first_seen: tokio::time::Instant,
}

impl ReassemblyBuf {
    fn is_complete(&self) -> bool {
        self.fragments.len() == self.total as usize
    }

    /// Consumes the buffer and returns fragments concatenated in sequence order.
    fn reassemble(mut self) -> Vec<u8> {
        let mut seqs: Vec<u16> = self.fragments.keys().copied().collect();
        seqs.sort_unstable();
        let mut out = Vec::new();
        for seq in seqs {
            out.extend_from_slice(&self.fragments.remove(&seq).unwrap());
        }
        out
    }
}

fn prune_stale_reassembly(map: &mut HashMap<u32, ReassemblyBuf>, conn_id: u64) {
    let now = tokio::time::Instant::now();
    map.retain(|stream_id, buf| {
        if now.duration_since(buf.first_seen) >= FRAGMENT_TIMEOUT {
            warn!(
                conn_id = conn_id,
                stream_id = stream_id,
                "fragment reassembly timed out — buffer discarded"
            );
            counter!("ipc_fragment_timeout_total").increment(1);
            false
        } else {
            true
        }
    });
}

/// Per-connection MAC key, shared between the read loop (which verifies inbound
/// frames) and the router (which installs the key on successful registration).
pub type SessionKeyCell = Arc<Mutex<Option<[u8; 32]>>>;

/// Items on a connection's write channel. `EnableMac` is an ordered control
/// item: it both activates outbound tagging and installs the inbound key on the
/// shared `SessionKeyCell` — guaranteeing inbound MAC verification is never
/// armed before the registration ack has been written to the socket (VULN-020).
pub enum Outbound {
    Frame(Box<Frame>),
    EnableMac([u8; 32], SessionKeyCell),
}

pub fn out_frame(f: Frame) -> Outbound {
    Outbound::Frame(Box::new(f))
}

pub struct ConnectionHandler {
    conn_id: u64,
    read_half: OwnedReadHalf,
    write_tx: mpsc::Sender<Outbound>,
    incoming_tx: mpsc::Sender<IncomingMessage>,
    disconnect_tx: mpsc::Sender<u64>,
    session_key: SessionKeyCell,
}

impl ConnectionHandler {
    pub fn new(
        conn_id: u64,
        stream: UnixStream,
        incoming_tx: mpsc::Sender<IncomingMessage>,
        disconnect_tx: mpsc::Sender<u64>,
    ) -> (Self, mpsc::Sender<Outbound>) {
        let (read_half, write_half) = stream.into_split();
        let (write_tx, write_rx) = mpsc::channel::<Outbound>(64);

        tokio::spawn(write_loop(write_half, write_rx));

        let handler = ConnectionHandler {
            conn_id,
            read_half,
            write_tx: write_tx.clone(),
            incoming_tx,
            disconnect_tx,
            session_key: Arc::new(Mutex::new(None)),
        };

        (handler, write_tx)
    }

    /// Shared key cell for this connection. The router stores the derived key
    /// here on registration so the read loop can verify subsequent frames.
    #[allow(dead_code)]
    pub fn session_key_cell(&self) -> SessionKeyCell {
        self.session_key.clone()
    }

    pub async fn run(mut self) {
        info!(conn_id = self.conn_id, "connection opened");
        let mut reassembly_map: HashMap<u32, ReassemblyBuf> = HashMap::new();
        loop {
            match read_frame(&mut self.read_half).await {
                Ok(frame) => {
                    let key = *self.session_key.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(k) = key {
                        // Secured connection: every frame must carry a valid MAC.
                        // A well-formed frame with a bad/missing tag is active
                        // tampering — drop the connection immediately.
                        let valid = frame.flags & FLAG_MAC_PRESENT != 0
                            && match &frame.mac {
                                Some(tag) => {
                                    let header = serialize_header(&frame);
                                    verify_tag(&k, &header, &frame.payload, tag)
                                }
                                None => false,
                            };
                        if !valid {
                            warn!(
                                conn_id = self.conn_id,
                                "frame MAC invalid — dropping connection"
                            );
                            counter!("ipc_frame_errors_total", "error" => "mac").increment(1);
                            break;
                        }
                    }

                    // Prune stale reassembly buffers on every frame arrival so
                    // an attacker cannot exhaust memory by opening streams and
                    // never sending the final fragment.
                    prune_stale_reassembly(&mut reassembly_map, self.conn_id);

                    if frame.flags & FLAG_FRAGMENTED != 0 {
                        let frag_hdr = match parse_frag_header(&frame.payload) {
                            Some(h) => h,
                            None => {
                                warn!(
                                    conn_id = self.conn_id,
                                    "fragment header too short — dropping connection"
                                );
                                counter!("ipc_frame_errors_total", "error" => "fragment_header")
                                    .increment(1);
                                break;
                            }
                        };
                        if frag_hdr.total == 0 || frag_hdr.sequence >= frag_hdr.total {
                            warn!(
                                conn_id = self.conn_id,
                                seq = frag_hdr.sequence,
                                total = frag_hdr.total,
                                "fragment header invalid — dropping connection"
                            );
                            counter!("ipc_frame_errors_total", "error" => "fragment_invalid")
                                .increment(1);
                            break;
                        }

                        let frag_data = frame.payload[FRAG_HEADER_SIZE..].to_vec();
                        let entry =
                            reassembly_map
                                .entry(frag_hdr.stream_id)
                                .or_insert_with(|| ReassemblyBuf {
                                    fragments: HashMap::new(),
                                    total: frag_hdr.total,
                                    target: frame.target,
                                    first_seen: tokio::time::Instant::now(),
                                });
                        entry.fragments.insert(frag_hdr.sequence, frag_data);

                        if entry.is_complete() {
                            let buf = reassembly_map.remove(&frag_hdr.stream_id).unwrap();
                            let target = buf.target;
                            let payload = buf.reassemble();
                            let crc = crc32fast::hash(&payload);
                            let assembled = Frame {
                                magic: frame.magic,
                                flags: frame.flags & !FLAG_FRAGMENTED,
                                length: payload.len() as u32,
                                target,
                                crc32: crc,
                                payload,
                                mac: None,
                            };
                            let msg = IncomingMessage {
                                conn_id: self.conn_id,
                                frame: assembled,
                                write_tx: self.write_tx.clone(),
                                session_key: self.session_key.clone(),
                            };
                            if self.incoming_tx.send(msg).await.is_err() {
                                break;
                            }
                        }
                    } else {
                        let msg = IncomingMessage {
                            conn_id: self.conn_id,
                            frame,
                            write_tx: self.write_tx.clone(),
                            session_key: self.session_key.clone(),
                        };
                        if self.incoming_tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                }
                Err(VeyronError::FrameCrcMismatch) => {
                    warn!(conn_id = self.conn_id, "CRC mismatch on incoming frame");
                    counter!("ipc_frame_errors_total", "error" => "crc").increment(1);
                    break;
                }
                Err(VeyronError::FrameMagicMismatch) => {
                    warn!(conn_id = self.conn_id, "frame magic mismatch");
                    counter!("ipc_frame_errors_total", "error" => "magic").increment(1);
                    break;
                }
                Err(VeyronError::PayloadTooLarge(n)) => {
                    warn!(
                        conn_id = self.conn_id,
                        bytes = n,
                        "oversized frame rejected"
                    );
                    counter!("ipc_frame_errors_total", "error" => "oversized").increment(1);
                    break;
                }
                Err(VeyronError::FrameReadTimeout) => {
                    warn!(
                        conn_id = self.conn_id,
                        "frame read timed out — dropping connection"
                    );
                    counter!("ipc_frame_errors_total", "error" => "timeout").increment(1);
                    break;
                }
                Err(_) => break, // IO errors / EOF — normal disconnect
            }
        }
        info!(conn_id = self.conn_id, "connection closed");
        let _ = self.disconnect_tx.send(self.conn_id).await;
    }
}

async fn write_loop(mut write_half: OwnedWriteHalf, mut rx: mpsc::Receiver<Outbound>) {
    let mut key: Option<[u8; 32]> = None;
    while let Some(item) = rx.recv().await {
        let mut frame = match item {
            Outbound::EnableMac(k, cell) => {
                key = Some(k);
                *cell.lock().unwrap_or_else(|p| p.into_inner()) = Some(k);
                continue;
            }
            Outbound::Frame(frame) => *frame,
        };

        if let Some(k) = &key {
            // Tag outbound frames once MAC is enabled for this connection.
            frame.flags |= FLAG_MAC_PRESENT;
            let header = serialize_header(&frame);
            frame.mac = Some(compute_tag(k, &header, &frame.payload));
        }

        match write_frame_raw(&mut write_half, &frame).await {
            Ok(()) => {}
            // An oversized frame is a kernel-side fault — drop that frame but keep
            // the connection; tearing it down would punish the plugin for our bug.
            Err(VeyronError::PayloadTooLarge(n)) => {
                warn!(bytes = n, "dropping oversized outbound frame");
                counter!("ipc_frame_errors_total", "error" => "oversized_outbound").increment(1);
            }
            // Real I/O error (peer gone): stop the write loop.
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::frame_mac::{derive_session_key, verify_tag};
    use crate::ipc::framing::{read_frame, write_frame_raw, FLAG_MAC_PRESENT};

    /// Build a fragmented-frame payload: 10-byte header + data.
    fn frag_payload(fragment_id: u16, seq: u16, total: u16, stream_id: u32, data: &[u8]) -> Vec<u8> {
        let mut p = Vec::with_capacity(FRAG_HEADER_SIZE + data.len());
        p.extend_from_slice(&fragment_id.to_be_bytes());
        p.extend_from_slice(&seq.to_be_bytes());
        p.extend_from_slice(&total.to_be_bytes());
        p.extend_from_slice(&stream_id.to_be_bytes());
        p.extend_from_slice(data);
        p
    }

    fn frag_frame(target: &str, fragment_id: u16, seq: u16, total: u16, stream_id: u32, data: &[u8]) -> Frame {
        let payload = frag_payload(fragment_id, seq, total, stream_id, data);
        let crc = crc32fast::hash(&payload);
        let mut t = [0u8; 32];
        t[..target.len()].copy_from_slice(target.as_bytes());
        Frame {
            magic: 0x5652,
            flags: FLAG_FRAGMENTED,
            length: payload.len() as u32,
            target: t,
            crc32: crc,
            payload,
            mac: None,
        }
    }

    fn plain(target: &str, payload: &[u8]) -> Frame {
        let crc = crc32fast::hash(payload);
        let mut t = [0u8; 32];
        t[..target.len()].copy_from_slice(target.as_bytes());
        Frame {
            magic: 0x5652,
            flags: 0,
            length: payload.len() as u32,
            target: t,
            crc32: crc,
            payload: payload.to_vec(),
            mac: None,
        }
    }

    #[tokio::test]
    async fn write_loop_tags_frames_only_after_enable_mac() {
        let (a, b) = UnixStream::pair().unwrap();
        let (_ra, wa) = a.into_split();
        let (tx, rx) = mpsc::channel::<Outbound>(8);
        tokio::spawn(write_loop(wa, rx));

        let key = derive_session_key(b"s", b"nonce-aaaaaaaaaa", "p");
        let cell: SessionKeyCell = Arc::new(Mutex::new(None));
        tx.send(out_frame(plain("plugin", b"ack"))).await.unwrap(); // pre-enable
        tx.send(Outbound::EnableMac(key, cell.clone()))
            .await
            .unwrap();
        tx.send(out_frame(plain("plugin", b"secured")))
            .await
            .unwrap();

        let (mut rb, _wb) = b.into_split();
        let f1 = read_frame(&mut rb).await.unwrap();
        assert_eq!(f1.mac, None, "pre-enable frame must be untagged");

        let f2 = read_frame(&mut rb).await.unwrap();
        let tag = f2.mac.expect("post-enable frame must be tagged");
        let header = serialize_header(&f2);
        assert!(verify_tag(&key, &header, &f2.payload, &tag));
    }

    #[tokio::test]
    async fn read_loop_drops_connection_on_bad_mac() {
        let (client, server) = UnixStream::pair().unwrap();
        let (incoming_tx, mut incoming_rx) = mpsc::channel(8);
        let (disc_tx, mut disc_rx) = mpsc::channel(8);

        let (handler, _wtx) = ConnectionHandler::new(1, server, incoming_tx, disc_tx);
        let key = derive_session_key(b"s", b"nonce-aaaaaaaaaa", "p");
        handler.session_key_cell().lock().unwrap().replace(key);
        tokio::spawn(handler.run());

        // frame claims MAC_PRESENT but carries a bogus tag
        let (_rc, mut wc) = client.into_split();
        let mut frame = plain("kernel", b"forge");
        frame.flags = FLAG_MAC_PRESENT;
        frame.mac = Some([0u8; 32]);
        write_frame_raw(&mut wc, &frame).await.unwrap();

        let disc = tokio::time::timeout(std::time::Duration::from_secs(1), disc_rx.recv())
            .await
            .expect("must signal disconnect")
            .unwrap();
        assert_eq!(disc, 1);
        assert!(
            incoming_rx.try_recv().is_err(),
            "forged frame must not be dispatched"
        );
    }

    #[tokio::test]
    async fn fragments_out_of_order_reassemble_correctly() {
        let (client, server) = UnixStream::pair().unwrap();
        let (incoming_tx, mut incoming_rx) = mpsc::channel(8);
        let (disc_tx, _disc_rx) = mpsc::channel(8);
        let (handler, _wtx) = ConnectionHandler::new(42, server, incoming_tx, disc_tx);
        tokio::spawn(handler.run());

        let (_, mut wc) = client.into_split();
        // Send 3 fragments out-of-order: seq 2, 0, 1
        write_frame_raw(&mut wc, &frag_frame("kernel", 1, 2, 3, 99, b"third")).await.unwrap();
        write_frame_raw(&mut wc, &frag_frame("kernel", 1, 0, 3, 99, b"first")).await.unwrap();
        write_frame_raw(&mut wc, &frag_frame("kernel", 1, 1, 3, 99, b"second")).await.unwrap();

        let msg = tokio::time::timeout(std::time::Duration::from_secs(1), incoming_rx.recv())
            .await
            .expect("reassembled message must arrive within 1s")
            .unwrap();

        assert_eq!(msg.frame.payload, b"firstsecondthird", "fragments must be reassembled in sequence order");
        assert_eq!(msg.frame.flags & FLAG_FRAGMENTED, 0, "FLAG_FRAGMENTED must be cleared on reassembled frame");
        assert_eq!(msg.conn_id, 42);
    }

    #[tokio::test(start_paused = true)]
    async fn incomplete_fragment_set_pruned_after_timeout() {
        let (client, server) = UnixStream::pair().unwrap();
        let (incoming_tx, mut incoming_rx) = mpsc::channel(8);
        let (disc_tx, _disc_rx) = mpsc::channel(8);
        let (handler, _wtx) = ConnectionHandler::new(7, server, incoming_tx, disc_tx);
        tokio::spawn(handler.run());

        let (_, mut wc) = client.into_split();
        // Send only 1 of 2 fragments — never complete the set.
        write_frame_raw(&mut wc, &frag_frame("kernel", 1, 0, 2, 55, b"half")).await.unwrap();

        // Advance time past FRAGMENT_TIMEOUT (30s) to make the buffer stale.
        tokio::time::advance(std::time::Duration::from_secs(31)).await;

        // Send a normal frame to trigger stale buffer pruning.
        write_frame_raw(&mut wc, &plain("kernel", b"trigger")).await.unwrap();

        // Only the plain frame arrives — the incomplete fragment is never dispatched.
        let msg = tokio::time::timeout(std::time::Duration::from_secs(1), incoming_rx.recv())
            .await
            .expect("plain frame must arrive")
            .unwrap();
        assert_eq!(msg.frame.payload, b"trigger");
        assert!(incoming_rx.try_recv().is_err(), "incomplete fragment must not be dispatched");
    }
}
