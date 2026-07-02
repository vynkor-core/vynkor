import asyncio
import os
from abc import ABC, abstractmethod
from typing import Optional

from .client import VeyronClient
from .veyron_protocol_pb2 import Envelope, PluginManifest


def _default_socket_path() -> str:
    """Per-user socket location, mirroring the kernel's default_socket_path():
    XDG_RUNTIME_DIR → /run/user/<uid> → ~/.veyron/run. Never the world-writable
    shared /tmp (BUG-006)."""
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR")
    if runtime_dir:
        return os.path.join(runtime_dir, "veyron.sock")
    run_user = f"/run/user/{os.getuid()}"
    if os.path.isdir(run_user):
        return os.path.join(run_user, "veyron.sock")
    return os.path.join(os.path.expanduser("~"), ".veyron", "run", "veyron.sock")


class Plugin(ABC):
    """Abstract base for Veyron plugins. Subclass and implement on_message."""

    plugin_id: str
    manifest: PluginManifest = PluginManifest()
    jwt_token: str = ""

    def __init__(self):
        socket_path = os.environ.get("VEYRON_SOCKET_PATH") or _default_socket_path()
        if not self.jwt_token:
            self.jwt_token = os.environ.get("VEYRON_JWT_TOKEN", "")
        secret_env = os.environ.get("VEYRON_JWT_SECRET")
        secret = secret_env.encode() if secret_env else None
        self._client = VeyronClient(socket_path, secret=secret)

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
