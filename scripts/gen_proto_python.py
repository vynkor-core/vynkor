#!/usr/bin/env python3
"""Generate Python protobuf bindings from proto/veyron_protocol.proto."""
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).parent.parent
PROTO = ROOT / "proto" / "veyron_protocol.proto"
OUT = ROOT / "sdk" / "python" / "veyron"

if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    result = subprocess.run(
        [
            sys.executable, "-m", "grpc_tools.protoc",
            f"-I{ROOT / 'proto'}",
            f"--python_out={OUT}",
            str(PROTO),
        ],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(result.stderr, file=sys.stderr)
        sys.exit(1)
    print(f"Generated {OUT}/veyron_protocol_pb2.py")
