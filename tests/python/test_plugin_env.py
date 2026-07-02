"""Unit tests: Plugin base class picks up JWT credentials from env vars
(R5-05). No live kernel required — these only check construction wiring."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "../../sdk/python"))

import pytest

from veyron.plugin import Plugin


class _NoopPlugin(Plugin):
    plugin_id = "env-test-plugin"

    async def on_message(self, envelope):
        pass


@pytest.fixture(autouse=True)
def _clean_env(monkeypatch):
    monkeypatch.delenv("VEYRON_JWT_TOKEN", raising=False)
    monkeypatch.delenv("VEYRON_JWT_SECRET", raising=False)
    monkeypatch.delenv("VEYRON_SOCKET_PATH", raising=False)


def test_jwt_token_defaults_from_env(monkeypatch):
    monkeypatch.setenv("VEYRON_JWT_TOKEN", "tok-123")
    plugin = _NoopPlugin()
    assert plugin.jwt_token == "tok-123"


def test_jwt_token_defaults_to_empty_without_env():
    plugin = _NoopPlugin()
    assert plugin.jwt_token == ""


def test_jwt_secret_from_env_reaches_client(monkeypatch):
    monkeypatch.setenv("VEYRON_JWT_SECRET", "shh-secret")
    plugin = _NoopPlugin()
    assert plugin._client._secret == b"shh-secret"


def test_no_secret_env_leaves_client_secret_none():
    plugin = _NoopPlugin()
    assert plugin._client._secret is None
