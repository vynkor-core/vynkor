use crate::framing::{read_frame, write_frame};
use prost::Message;
use std::time::{Duration, Instant};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use veyron::auth::frame_mac::{compute_tag, derive_session_key, verify_tag};
use veyron::ipc::framing::{serialize_header, write_frame_raw, Frame, FLAG_MAC_PRESENT};
use veyron::proto::veyron::{
    envelope, Envelope, KernelCommand, KernelCommandAck, Ping, PluginManifest, PluginRegister,
    PluginRegisterAck, Subscribe,
};
use veyron::utils::errors::VeyronError;

pub struct VeyronClient {
    read: OwnedReadHalf,
    write: OwnedWriteHalf,
    /// Shared JWT secret, needed to derive the frame-MAC key. None => no MAC.
    secret: Option<Vec<u8>>,
    /// Per-connection MAC key, set after a secured registration.
    session_key: Option<[u8; 32]>,
}

impl VeyronClient {
    pub async fn connect(socket_path: &str) -> Result<Self, VeyronError> {
        Self::connect_inner(socket_path, None).await
    }

    /// Connect with the shared JWT secret so the client can derive the frame-MAC
    /// key after registration (required to talk to a kernel started with auth).
    pub async fn connect_with_secret(
        socket_path: &str,
        secret: &[u8],
    ) -> Result<Self, VeyronError> {
        Self::connect_inner(socket_path, Some(secret.to_vec())).await
    }

    async fn connect_inner(
        socket_path: &str,
        secret: Option<Vec<u8>>,
    ) -> Result<Self, VeyronError> {
        let stream = UnixStream::connect(socket_path)
            .await
            .map_err(VeyronError::Io)?;
        let (read, write) = stream.into_split();
        Ok(Self {
            read,
            write,
            secret,
            session_key: None,
        })
    }

    pub async fn register(
        &mut self,
        plugin_id: &str,
        manifest: PluginManifest,
    ) -> Result<PluginRegisterAck, VeyronError> {
        self.register_with_token(plugin_id, manifest, "").await
    }

    /// Register presenting a JWT. On a secured kernel the ack carries a
    /// `session_nonce`; combined with the shared secret and plugin id it yields
    /// the frame-MAC key used for all subsequent frames.
    pub async fn register_with_token(
        &mut self,
        plugin_id: &str,
        manifest: PluginManifest,
        jwt_token: &str,
    ) -> Result<PluginRegisterAck, VeyronError> {
        let env = Envelope {
            payload: Some(envelope::Payload::PluginRegister(PluginRegister {
                plugin_id: plugin_id.to_string(),
                version: "1.0.0".to_string(),
                manifest: Some(manifest),
                jwt_token: jwt_token.to_string(),
                ..Default::default()
            })),
            ..Default::default()
        };
        self.send("kernel", env).await?;
        let response = self.recv().await?;
        match response.payload {
            Some(envelope::Payload::PluginRegisterAck(ack)) => {
                if let Some(secret) = &self.secret {
                    if !ack.session_nonce.is_empty() {
                        self.session_key =
                            Some(derive_session_key(secret, &ack.session_nonce, plugin_id));
                    }
                }
                Ok(ack)
            }
            _ => Err(VeyronError::Internal("expected PluginRegisterAck".into())),
        }
    }

    pub async fn send(&mut self, target: &str, envelope: Envelope) -> Result<(), VeyronError> {
        let mut payload = Vec::new();
        envelope
            .encode(&mut payload)
            .map_err(|_| VeyronError::Internal("encode failed".into()))?;
        self.send_raw(target, payload).await
    }

    pub async fn send_raw(&mut self, target: &str, payload: Vec<u8>) -> Result<(), VeyronError> {
        match &self.session_key {
            // Secured: tag the frame with the connection's MAC key.
            Some(key) => {
                let crc = crc32fast::hash(&payload);
                let mut t = [0u8; 32];
                let b = target.as_bytes();
                let n = b.len().min(32);
                t[..n].copy_from_slice(&b[..n]);
                let mut frame = Frame {
                    magic: 0x5652,
                    flags: FLAG_MAC_PRESENT,
                    length: payload.len() as u32,
                    target: t,
                    crc32: crc,
                    payload,
                    mac: None,
                };
                let header = serialize_header(&frame);
                frame.mac = Some(compute_tag(key, &header, &frame.payload));
                write_frame_raw(&mut self.write, &frame).await
            }
            None => write_frame(&mut self.write, target, 0, &payload).await,
        }
    }

    /// Send a raw payload with explicit extra flags ORed into the frame header.
    /// Used by tests that need to set FLAG_RAW_BINARY without MAC involvement.
    pub async fn send_raw_with_flags(
        &mut self,
        target: &str,
        extra_flags: u16,
        payload: Vec<u8>,
    ) -> Result<(), VeyronError> {
        let crc = crc32fast::hash(&payload);
        let mut t = [0u8; 32];
        let b = target.as_bytes();
        t[..b.len().min(32)].copy_from_slice(&b[..b.len().min(32)]);
        let base_flags = if self.session_key.is_some() {
            FLAG_MAC_PRESENT
        } else {
            0
        };
        let mut frame = Frame {
            magic: 0x5652,
            flags: base_flags | extra_flags,
            length: payload.len() as u32,
            target: t,
            crc32: crc,
            payload,
            mac: None,
        };
        if let Some(key) = &self.session_key {
            let header = serialize_header(&frame);
            frame.mac = Some(compute_tag(key, &header, &frame.payload));
        }
        write_frame_raw(&mut self.write, &frame).await
    }

    pub async fn recv(&mut self) -> Result<Envelope, VeyronError> {
        let frame = read_frame(&mut self.read).await?;
        if let Some(key) = &self.session_key {
            let valid = frame.flags & FLAG_MAC_PRESENT != 0
                && match &frame.mac {
                    Some(tag) => {
                        let header = serialize_header(&frame);
                        verify_tag(key, &header, &frame.payload, tag)
                    }
                    None => false,
                };
            if !valid {
                return Err(VeyronError::Internal(
                    "frame MAC verification failed".into(),
                ));
            }
        }
        Envelope::decode(frame.payload.as_slice()).map_err(VeyronError::Proto)
    }

    pub async fn subscribe(&mut self, event_types: Vec<String>) -> Result<(), VeyronError> {
        let env = Envelope {
            payload: Some(envelope::Payload::Subscribe(Subscribe { event_types })),
            ..Default::default()
        };
        self.send("kernel", env).await
    }

    pub async fn send_command(
        &mut self,
        command_id: &str,
        command: &str,
        params_json: &[u8],
    ) -> Result<KernelCommandAck, VeyronError> {
        let env = Envelope {
            payload: Some(envelope::Payload::KernelCommand(KernelCommand {
                command_id: command_id.to_string(),
                command: command.to_string(),
                params_json: params_json.to_vec(),
            })),
            ..Default::default()
        };
        self.send("kernel", env).await?;
        let response = self.recv().await?;
        match response.payload {
            Some(envelope::Payload::KernelCommandAck(ack)) => Ok(ack),
            _ => Err(VeyronError::Internal("expected KernelCommandAck".into())),
        }
    }

    pub async fn ping(&mut self) -> Result<Duration, VeyronError> {
        let start = Instant::now();
        let env = Envelope {
            payload: Some(envelope::Payload::Ping(Ping {
                timestamp: start.elapsed().as_millis() as u64,
            })),
            ..Default::default()
        };
        self.send("kernel", env).await?;
        let response = self.recv().await?;
        match response.payload {
            Some(envelope::Payload::Pong(_)) => Ok(start.elapsed()),
            _ => Err(VeyronError::Internal("expected Pong".into())),
        }
    }
}
