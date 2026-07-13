"""P7-01: VeyronClient.publish_event — mirrors sdk/rust/src/client.rs's
publish_event tests (OK/PERMISSION_DENY ack passthrough, kernel error raises,
deadline timeout raises, unrelated traffic discarded)."""
import asyncio
import os
import socket
import sys
import threading
import time

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "../../sdk/python"))

from veyron import VeyronClient
from veyron.framing import pack_frame, read_frame
from veyron.veyron_protocol_pb2 import (
    Envelope,
    EVENT_PUBLISH_OK,
    EVENT_PUBLISH_PERMISSION_DENY,
)


def _recv_kernel_side(sock) -> Envelope:
    payload = read_frame(sock.makefile("rb"))
    env = Envelope()
    env.ParseFromString(payload)
    return env


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
async def test_ok_ack_returned():
    client, kernel_sock = await _make_client()

    def kernel():
        req = _recv_kernel_side(kernel_sock)
        assert req.HasField("event_publish")
        assert req.event_publish.event_type == "my.event"

        resp = Envelope()
        resp.event_publish_ack.event_id = "evt-1"
        resp.event_publish_ack.status = EVENT_PUBLISH_OK
        _send_kernel_side(kernel_sock, resp)

    t = threading.Thread(target=kernel)
    t.start()
    ack = await client.publish_event("my.event", b"{}", 1000)
    t.join()

    assert ack.event_id == "evt-1"
    assert ack.status == EVENT_PUBLISH_OK


@pytest.mark.asyncio
async def test_permission_deny_ack_returned_not_raised():
    client, kernel_sock = await _make_client()

    def kernel():
        _recv_kernel_side(kernel_sock)
        resp = Envelope()
        resp.event_publish_ack.event_id = "evt-2"
        resp.event_publish_ack.status = EVENT_PUBLISH_PERMISSION_DENY
        _send_kernel_side(kernel_sock, resp)

    t = threading.Thread(target=kernel)
    t.start()
    ack = await client.publish_event("my.event", b"{}", 1000)
    t.join()

    assert ack.status == EVENT_PUBLISH_PERMISSION_DENY


@pytest.mark.asyncio
async def test_kernel_error_envelope_raises():
    client, kernel_sock = await _make_client()

    def kernel():
        _recv_kernel_side(kernel_sock)
        resp = Envelope()
        resp.error.message = "boom"
        resp.error.details = "detail"
        _send_kernel_side(kernel_sock, resp)

    t = threading.Thread(target=kernel)
    t.start()
    with pytest.raises(RuntimeError):
        await client.publish_event("my.event", b"{}", 1000)
    t.join()


@pytest.mark.asyncio
async def test_times_out_when_no_response():
    client, kernel_sock = await _make_client()

    def kernel():
        _recv_kernel_side(kernel_sock)
        time.sleep(0.5)

    t = threading.Thread(target=kernel)
    t.start()
    start = time.monotonic()
    with pytest.raises(TimeoutError):
        await client.publish_event("my.event", b"{}", 150)
    elapsed = time.monotonic() - start
    assert elapsed < 2.0
    t.join()
    kernel_sock.close()


@pytest.mark.asyncio
async def test_unrelated_envelope_discarded_then_ack_returned():
    client, kernel_sock = await _make_client()

    def kernel():
        _recv_kernel_side(kernel_sock)

        ping_env = Envelope()
        ping_env.ping.timestamp = 1
        _send_kernel_side(kernel_sock, ping_env)

        resp = Envelope()
        resp.event_publish_ack.event_id = "evt-3"
        resp.event_publish_ack.status = EVENT_PUBLISH_OK
        _send_kernel_side(kernel_sock, resp)

    t = threading.Thread(target=kernel)
    t.start()
    ack = await client.publish_event("my.event", b"{}", 1000)
    t.join()

    assert ack.event_id == "evt-3"
