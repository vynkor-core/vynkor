import asyncio
import time
from typing import Optional

from .framing import pack_frame, async_read_frame
from .veyron_protocol_pb2 import (
    Envelope,
    PluginManifest,
    PluginRegister,
    Ping,
    Subscribe,
)


class VeyronClient:
    """Async client for the Veyron kernel IPC protocol."""

    def __init__(self, socket_path: str):
        self.socket_path = socket_path
        self._reader: Optional[asyncio.StreamReader] = None
        self._writer: Optional[asyncio.StreamWriter] = None
        self.plugin_id: Optional[str] = None

    async def connect(self) -> None:
        self._reader, self._writer = await asyncio.open_unix_connection(self.socket_path)

    async def close(self) -> None:
        if self._writer:
            self._writer.close()
            await self._writer.wait_closed()

    async def register(
        self,
        plugin_id: str,
        manifest: Optional[PluginManifest] = None,
        jwt_token: str = "",
    ) -> Envelope:
        self.plugin_id = plugin_id
        reg = PluginRegister(
            plugin_id=plugin_id,
            jwt_token=jwt_token,
        )
        if manifest is not None:
            reg.manifest.CopyFrom(manifest)
        env = Envelope()
        env.plugin_register.CopyFrom(reg)
        await self._send_envelope("kernel", env)
        return await self.recv()

    async def send(self, target: str, envelope: Envelope) -> None:
        await self._send_envelope(target, envelope)

    async def recv(self) -> Envelope:
        payload = await async_read_frame(self._reader)
        env = Envelope()
        env.ParseFromString(payload)
        return env

    async def subscribe(self, event_types: list) -> None:
        sub = Subscribe(event_types=event_types)
        env = Envelope()
        env.subscribe.CopyFrom(sub)
        await self._send_envelope("kernel", env)

    async def ping(self) -> float:
        ts = int(time.time() * 1000)
        ping_msg = Ping(timestamp=ts)
        env = Envelope()
        env.ping.CopyFrom(ping_msg)
        t0 = time.monotonic()
        await self._send_envelope("kernel", env)
        await self.recv()
        return time.monotonic() - t0

    async def _send_envelope(self, target: str, envelope: Envelope) -> None:
        payload = envelope.SerializeToString()
        frame = pack_frame(target, payload)
        self._writer.write(frame)
        await self._writer.drain()
