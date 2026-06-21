import asyncio
import os
from abc import ABC, abstractmethod
from typing import Optional

from .client import VeyronClient
from .veyron_protocol_pb2 import Envelope, PluginManifest


class Plugin(ABC):
    """Abstract base for Veyron plugins. Subclass and implement on_message."""

    plugin_id: str
    manifest: PluginManifest = PluginManifest()
    jwt_token: str = ""

    def __init__(self):
        socket_path = os.environ.get("VEYRON_SOCKET_PATH", "/tmp/veyron.sock")
        self._client = VeyronClient(socket_path)

    async def on_init(self) -> None:
        """Called once after successful registration."""

    @abstractmethod
    async def on_message(self, envelope: Envelope) -> None:
        """Called for every incoming message."""

    async def on_shutdown(self) -> None:
        """Called before the plugin exits."""

    async def run(self) -> None:
        await self._client.connect()
        ack = await self._client.register(
            self.plugin_id, self.manifest, self.jwt_token
        )
        if not ack.plugin_register_ack.accepted:
            raise RuntimeError(
                f"registration rejected: {ack.plugin_register_ack.reject_reason}"
            )
        await self.on_init()
        try:
            while True:
                env = await self._client.recv()
                if env.HasField("plugin_shutdown"):
                    break
                await self.on_message(env)
        finally:
            await self.on_shutdown()
            await self._client.close()
