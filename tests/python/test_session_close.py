"""P7-03: VeyronClient.recv() distinguishes SessionClose from
ActionStreamAbort — mirrors sdk/rust/tests/protocol.rs's
recv_distinguishes_session_close_from_stream_abort."""
import asyncio
import os
import socket
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "../../../veyron-sdk-python"))

from veyron import VeyronClient
from veyron.framing import pack_frame
from veyron.veyron_protocol_pb2 import Envelope


def _send_kernel_side(sock, env: Envelope) -> None:
    frame = pack_frame("plugin", env.SerializeToString())
    sock.sendall(frame)


async def _make_client():
    sock_a, sock_b = socket.socketpair()
    reader, writer = await asyncio.open_connection(sock=sock_a)
    client = VeyronClient("unused")
    client._reader = reader
    client._writer = writer
    return client, sock_b


@pytest.mark.asyncio
async def test_recv_returns_session_close():
    client, kernel_sock = await _make_client()

    close_env = Envelope()
    close_env.session_close.action_id = "act-1"
    close_env.session_close.reason = "client closed"
    _send_kernel_side(kernel_sock, close_env)

    received = await client.recv()
    assert received.HasField("session_close")
    assert received.session_close.action_id == "act-1"
    assert received.session_close.reason == "client closed"


@pytest.mark.asyncio
async def test_recv_distinguishes_from_action_stream_abort():
    client, kernel_sock = await _make_client()

    close_env = Envelope()
    close_env.session_close.action_id = "act-1"
    close_env.session_close.reason = "client closed"
    _send_kernel_side(kernel_sock, close_env)

    received_close = await client.recv()
    assert received_close.HasField("session_close")

    abort_env = Envelope()
    abort_env.action_stream_abort.action_id = "act-1"
    abort_env.action_stream_abort.reason = "idle timeout"
    _send_kernel_side(kernel_sock, abort_env)

    received_abort = await client.recv()
    assert received_abort.HasField("action_stream_abort")
    assert received_abort.action_stream_abort.action_id == "act-1"
    assert received_abort.action_stream_abort.reason == "idle timeout"
    assert not received_abort.HasField("session_close")
