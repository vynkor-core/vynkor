import struct
from binascii import crc32

MAGIC = 0x5652
HEADER_FMT = ">HHI32sI"  # magic, flags, length, target, crc32
HEADER_SIZE = struct.calcsize(HEADER_FMT)  # 44
MAX_PAYLOAD = 1_048_576


def pack_frame(target: str, payload: bytes, flags: int = 0) -> bytes:
    if len(payload) > MAX_PAYLOAD:
        raise ValueError(f"payload too large: {len(payload)} > {MAX_PAYLOAD}")
    target_bytes = target.encode()[:32].ljust(32, b"\x00")[:32]
    checksum = crc32(payload) & 0xFFFFFFFF
    header = struct.pack(HEADER_FMT, MAGIC, flags, len(payload), target_bytes, checksum)
    return header + payload


def read_frame(reader) -> bytes:
    """Read one frame from a synchronous file-like reader. Returns payload bytes."""
    header = _read_exact(reader, HEADER_SIZE)
    magic, flags, length, target_bytes, stored_crc = struct.unpack(HEADER_FMT, header)
    if magic != MAGIC:
        raise ValueError(f"bad magic: 0x{magic:04x}")
    if length > MAX_PAYLOAD:
        raise ValueError(f"payload too large: {length}")
    payload = _read_exact(reader, length) if length > 0 else b""
    computed = crc32(payload) & 0xFFFFFFFF
    if computed != stored_crc:
        raise ValueError(f"CRC mismatch: got 0x{computed:08x}, want 0x{stored_crc:08x}")
    return payload


async def async_read_frame(reader) -> bytes:
    """Read one frame from an asyncio StreamReader. Returns payload bytes."""
    header = await reader.readexactly(HEADER_SIZE)
    magic, flags, length, target_bytes, stored_crc = struct.unpack(HEADER_FMT, header)
    if magic != MAGIC:
        raise ValueError(f"bad magic: 0x{magic:04x}")
    if length > MAX_PAYLOAD:
        raise ValueError(f"payload too large: {length}")
    payload = await reader.readexactly(length) if length > 0 else b""
    computed = crc32(payload) & 0xFFFFFFFF
    if computed != stored_crc:
        raise ValueError(f"CRC mismatch: got 0x{computed:08x}, want 0x{stored_crc:08x}")
    return payload


def _read_exact(reader, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        chunk = reader.read(n - len(buf))
        if not chunk:
            raise EOFError("connection closed")
        buf += chunk
    return buf
