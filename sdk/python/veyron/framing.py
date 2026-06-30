import hashlib
import hmac as _hmac
import struct
from binascii import crc32
from typing import Optional

MAGIC = 0x5652
HEADER_FMT = ">HHI32sI"  # magic, flags, length, target, crc32
HEADER_SIZE = struct.calcsize(HEADER_FMT)  # 44
MAX_PAYLOAD = 1_048_576
FLAG_MAC_PRESENT = 0x0001
FLAG_RAW_BINARY  = 0x0010  # payload is raw bytes (PCM/Opus); router skips Protobuf decode


# ---------------------------------------------------------------------------
# HKDF-SHA256 (RFC 5869) — no external deps needed
# ---------------------------------------------------------------------------

def _hkdf_extract(salt: bytes, ikm: bytes) -> bytes:
    return _hmac.new(salt, ikm, hashlib.sha256).digest()


def _hkdf_expand(prk: bytes, info: bytes, length: int = 32) -> bytes:
    t = b""
    okm = b""
    counter = 1
    while len(okm) < length:
        h = _hmac.new(prk, digestmod=hashlib.sha256)
        h.update(t)
        h.update(info)
        h.update(bytes([counter]))
        t = h.digest()
        okm += t
        counter += 1
    return okm[:length]


def derive_session_key(secret: bytes, nonce: bytes, plugin_id: str) -> bytes:
    """HKDF-SHA256 session key. Mirrors Rust auth::frame_mac::derive_session_key."""
    prk = _hkdf_extract(salt=nonce, ikm=secret)
    info = b"veyron-frame-mac-v1|" + plugin_id.encode()
    return _hkdf_expand(prk, info, 32)


def compute_tag(key: bytes, header: bytes, payload: bytes) -> bytes:
    """HMAC-SHA256 over header || payload. Returns 32-byte tag."""
    h = _hmac.new(key, digestmod=hashlib.sha256)
    h.update(header)
    h.update(payload)
    return h.digest()


def verify_tag(key: bytes, header: bytes, payload: bytes, tag: bytes) -> bool:
    """Constant-time MAC verification."""
    expected = compute_tag(key, header, payload)
    return _hmac.compare_digest(expected, tag)


# ---------------------------------------------------------------------------
# Frame encoding / decoding
# ---------------------------------------------------------------------------

def pack_frame(
    target: str,
    payload: bytes,
    flags: int = 0,
    session_key: Optional[bytes] = None,
) -> bytes:
    if len(payload) > MAX_PAYLOAD:
        raise ValueError(f"payload too large: {len(payload)} > {MAX_PAYLOAD}")
    if session_key is not None:
        flags |= FLAG_MAC_PRESENT
    target_bytes = target.encode()[:32].ljust(32, b"\x00")[:32]
    checksum = crc32(payload) & 0xFFFFFFFF
    header = struct.pack(HEADER_FMT, MAGIC, flags, len(payload), target_bytes, checksum)
    frame = header + payload
    if session_key is not None:
        frame += compute_tag(session_key, header, payload)
    return frame


def read_frame(reader, session_key: Optional[bytes] = None) -> bytes:
    """Read one frame from a synchronous file-like reader. Returns payload bytes."""
    header_bytes = _read_exact(reader, HEADER_SIZE)
    magic, flags, length, _target, stored_crc = struct.unpack(HEADER_FMT, header_bytes)
    if magic != MAGIC:
        raise ValueError(f"bad magic: 0x{magic:04x}")
    if length > MAX_PAYLOAD:
        raise ValueError(f"payload too large: {length}")
    payload = _read_exact(reader, length) if length > 0 else b""
    computed = crc32(payload) & 0xFFFFFFFF
    if computed != stored_crc:
        raise ValueError(f"CRC mismatch: got 0x{computed:08x}, want 0x{stored_crc:08x}")
    if flags & FLAG_MAC_PRESENT:
        tag = _read_exact(reader, 32)
        if session_key is not None and not verify_tag(session_key, header_bytes, payload, tag):
            raise ValueError("MAC verification failed")
    return payload


async def async_read_frame(reader, session_key: Optional[bytes] = None) -> bytes:
    """Read one frame from an asyncio StreamReader. Returns payload bytes."""
    header_bytes = await reader.readexactly(HEADER_SIZE)
    magic, flags, length, _target, stored_crc = struct.unpack(HEADER_FMT, header_bytes)
    if magic != MAGIC:
        raise ValueError(f"bad magic: 0x{magic:04x}")
    if length > MAX_PAYLOAD:
        raise ValueError(f"payload too large: {length}")
    payload = await reader.readexactly(length) if length > 0 else b""
    computed = crc32(payload) & 0xFFFFFFFF
    if computed != stored_crc:
        raise ValueError(f"CRC mismatch: got 0x{computed:08x}, want 0x{stored_crc:08x}")
    if flags & FLAG_MAC_PRESENT:
        tag = await reader.readexactly(32)
        if session_key is not None and not verify_tag(session_key, header_bytes, payload, tag):
            raise ValueError("MAC verification failed")
    return payload


def _read_exact(reader, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        chunk = reader.read(n - len(buf))
        if not chunk:
            raise EOFError("connection closed")
        buf += chunk
    return buf
