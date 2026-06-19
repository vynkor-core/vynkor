#![allow(async_fn_in_trait)]

use crate::client::VeyronClient;
use std::env;
use veyron::proto::veyron::{Envelope, PluginManifest};
use veyron::utils::errors::VeyronError;

pub trait Plugin {
    fn id(&self) -> &str;
    fn manifest(&self) -> PluginManifest;

    async fn on_init(&mut self, client: &mut VeyronClient) -> Result<(), VeyronError>;
    async fn on_message(
        &mut self,
        envelope: Envelope,
    ) -> Result<Option<Envelope>, VeyronError>;
    async fn on_shutdown(&mut self) -> Result<(), VeyronError>;

    async fn run(&mut self) -> Result<(), VeyronError> {
        let socket_path =
            env::var("VEYRON_SOCKET_PATH").unwrap_or_else(|_| "/tmp/veyron.sock".to_string());
        self.run_with(&socket_path).await
    }

    async fn run_with(&mut self, socket_path: &str) -> Result<(), VeyronError> {
        let mut client = VeyronClient::connect(socket_path).await?;
        let _ack = client.register(self.id(), self.manifest()).await?;
        self.on_init(&mut client).await?;
        loop {
            match client.recv().await {
                Ok(env) => {
                    if let Ok(Some(resp)) = self.on_message(env).await {
                        let _ = client.send("kernel", resp).await;
                    }
                }
                Err(_) => break,
            }
        }
        self.on_shutdown().await
    }
}
