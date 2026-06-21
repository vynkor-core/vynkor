"""Integration tests for the Python Veyron SDK.

Requires a running kernel (vyn start --foreground) at /tmp/veyron.sock.
Run with: pytest tests/python/ -v
"""
import asyncio
import os
import sys

import pytest
import pytest_asyncio

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "../../sdk/python"))

from veyron import VeyronClient

SOCKET = os.environ.get("VEYRON_SOCKET_PATH", "/tmp/veyron.sock")
pytestmark = pytest.mark.skipif(
    not os.path.exists(SOCKET),
    reason=f"no kernel socket at {SOCKET}",
)


@pytest.fixture(scope="module")
def event_loop():
    loop = asyncio.new_event_loop()
    yield loop
    loop.close()


@pytest_asyncio.fixture(scope="function")
async def client():
    c = VeyronClient(SOCKET)
    await c.connect()
    yield c
    await c.close()


@pytest.mark.asyncio
async def test_connect_and_register(client):
    ack = await client.register("py-test-plugin")
    assert ack.HasField("plugin_register_ack"), f"expected register ack, got {ack}"
    assert ack.plugin_register_ack.accepted, (
        f"registration rejected: {ack.plugin_register_ack.reject_reason}"
    )


@pytest.mark.asyncio
async def test_ping_pong_round_trip(client):
    await client.register("py-ping-plugin")
    elapsed = await client.ping()
    assert elapsed < 1.0, f"ping took too long: {elapsed:.3f}s"


@pytest.mark.asyncio
async def test_subscribe_completes_without_error(client):
    await client.register("py-sub-plugin")
    await client.subscribe(["system.plugin_joined", "system.plugin_died"])
