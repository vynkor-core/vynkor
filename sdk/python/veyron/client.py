import asyncio
import time
from typing import Optional

from .framing import (
    async_read_frame,
    derive_session_key,
    pack_frame,
)
from .veyron_protocol_pb2 import (
    Envelope,
    PluginManifest,
    PluginRegister,
    Ping,
    Subscribe,
)


class VeyronClient:
    """Async client for the Veyron kernel IPC protocol."""

    def __init__(self, socket_path: str, secret: Optional[bytes] = None):
        self.socket_path = socket_path
        self._secret = secret
        self.session_key: Optional[bytes] = None
        self._reader: Optional[asyncio.StreamReader] = None
        self._writer: Optional[asyncio.StreamWriter] = None
        self.plugin_id: Optional[str] = None

    def _apply_session_nonce(self, plugin_id: str, nonce: bytes) -> None:
        """Derive and store session_key from a registration nonce."""
        if self._secret and nonce:
            self.session_key = derive_session_key(self._secret, nonce, plugin_id)

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
        reg = PluginRegister(plugin_id=plugin_id, jwt_token=jwt_token)
        if manifest is not None:
            reg.manifest.CopyFrom(manifest)
        env = Envelope()
        env.plugin_register.CopyFrom(reg)
        # Registration frame is always CRC-only (session_key not yet derived)
        await self._send_envelope("kernel", env, force_no_mac=True)
        ack = await self.recv(force_no_mac=True)
        if ack.HasField("plugin_register_ack"):
            # session_nonce was added in proto v1.2; use getattr for forward compat.
            nonce = getattr(ack.plugin_register_ack, "session_nonce", b"")
            if nonce:
                self._apply_session_nonce(plugin_id, nonce)
        return ack

    async def send(self, target: str, envelope: Envelope) -> None:
        await self._send_envelope(target, envelope)

    async def recv(self, force_no_mac: bool = False) -> Envelope:
        key = None if force_no_mac else self.session_key
        payload = await async_read_frame(self._reader, session_key=key)
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

    async def _send_envelope(
        self,
        target: str,
        envelope: Envelope,
        force_no_mac: bool = False,
    ) -> None:
        payload = envelope.SerializeToString()
        key = None if force_no_mac else self.session_key
        frame = pack_frame(target, payload, session_key=key)
        self._writer.write(frame)
        await self._writer.drain()
