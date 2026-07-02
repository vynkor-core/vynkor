try:
    from google.protobuf.runtime_version import VersionError as _ProtoVersionError
except ImportError:
    _ProtoVersionError = ImportError  # type: ignore[assignment,misc]

try:
    from .client import VeyronClient
    from .plugin import Plugin
except (ImportError, _ProtoVersionError) as _import_err:  # missing deps or protobuf version mismatch

    def _unavailable(name: str) -> type:
        def _raise(*_args: object, **_kwargs: object) -> None:
            raise ImportError(
                f"veyron.{name} unavailable: {_import_err}. "
                "Install the veyron SDK's declared dependencies (see pyproject.toml) to use it."
            ) from _import_err

        return type(name, (), {"__init__": _raise, "__init_subclass__": classmethod(_raise)})

    VeyronClient = _unavailable("VeyronClient")  # type: ignore[assignment,misc]
    Plugin = _unavailable("Plugin")  # type: ignore[assignment,misc]

from .framing import pack_frame, read_frame, async_read_frame

__all__ = ["VeyronClient", "Plugin", "pack_frame", "read_frame", "async_read_frame"]
