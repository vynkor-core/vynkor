use crate::framing::{read_frame, write_frame};
use prost::Message;
use std::time::{Duration, Instant};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use veyron::proto::veyron::{
    envelope, Envelope, Ping, PluginManifest, PluginRegister, PluginRegisterAck, Subscribe,
};
use veyron::utils::errors::VeyronError;

pub struct VeyronClient {
    read: OwnedReadHalf,
    write: OwnedWriteHalf,
}

impl VeyronClient {
    pub async fn connect(socket_path: &str) -> Result<Self, VeyronError> {
        let stream = UnixStream::connect(socket_path)
            .await
            .map_err(VeyronError::Io)?;
        let (read, write) = stream.into_split();
        Ok(Self { read, write })
    }

    pub async fn register(
        &mut self,
        plugin_id: &str,
        manifest: PluginManifest,
    ) -> Result<PluginRegisterAck, VeyronError> {
        let env = Envelope {
            payload: Some(envelope::Payload::PluginRegister(PluginRegister {
                plugin_id: plugin_id.to_string(),
                version: "1.0.0".to_string(),
                manifest: Some(manifest),
                ..Default::default()
            })),
            ..Default::default()
        };
        self.send("kernel", env).await?;
        let response = self.recv().await?;
        match response.payload {
            Some(envelope::Payload::PluginRegisterAck(ack)) => Ok(ack),
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
        write_frame(&mut self.write, target, 0, &payload).await
    }

    pub async fn recv(&mut self) -> Result<Envelope, VeyronError> {
        let frame = read_frame(&mut self.read).await?;
        Envelope::decode(frame.payload.as_slice()).map_err(VeyronError::Proto)
    }

    pub async fn subscribe(&mut self, event_types: Vec<String>) -> Result<(), VeyronError> {
        let env = Envelope {
            payload: Some(envelope::Payload::Subscribe(Subscribe { event_types })),
            ..Default::default()
        };
        self.send("kernel", env).await
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
