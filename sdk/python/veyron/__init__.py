try:
    from google.protobuf.runtime_version import VersionError as _ProtoVersionError
except ImportError:
    _ProtoVersionError = ImportError  # type: ignore[assignment,misc]

try:
    from .client import VeyronClient
    from .plugin import Plugin
except (ImportError, _ProtoVersionError):  # missing deps or protobuf version mismatch
    VeyronClient = None  # type: ignore[assignment,misc]
    Plugin = None  # type: ignore[assignment,misc]

from .framing import pack_frame, read_frame, async_read_frame

__all__ = ["VeyronClient", "Plugin", "pack_frame", "read_frame", "async_read_frame"]
