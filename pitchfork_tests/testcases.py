"""Testcase base that starts its own pitchfork-server fixture (subprocess).

Discipline: never skip on a missing service we own. The binary is built via
`just build` (cargo build); if it is missing, the tests fail loudly.
"""

import os
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request

from django.test import TestCase

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _default_server_bin():
    return os.path.join(REPO_ROOT, "target", "debug", "pitchfork-server")


def _wait_for_health(url, timeout=30.0):
    deadline = time.time() + timeout
    last_error = None
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(url + "health", timeout=2) as response:
                if response.status == 200:
                    return
        except Exception as exc:  # noqa: BLE001 - probe until healthy
            last_error = exc
        time.sleep(0.1)
    raise RuntimeError(
        f"pitchfork-server fixture did not become healthy at {url}: {last_error}"
    )


class PitchforkServerFixture:
    """Starts a pitchfork-server subprocess on a free port."""

    def __init__(self, port=None):
        self.bin_path = os.environ.get("PITCHFORK_SERVER_BIN", _default_server_bin())
        if not os.path.exists(self.bin_path):
            raise RuntimeError(
                f"pitchfork-server binary not found at {self.bin_path}. "
                "Run `just build` (cargo build) first."
            )
        self.port = port or _free_port()
        self.url = f"http://127.0.0.1:{self.port}/"
        self.data_dir = tempfile.mkdtemp(prefix="pitchfork_fixture_")
        self.process = None

    def start(self):
        self.process = subprocess.Popen(
            [
                self.bin_path,
                self.data_dir,
                f"127.0.0.1:{self.port}",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            _wait_for_health(self.url)
        except Exception:
            self.stop()
            raise
        return self

    def stop(self):
        if self.process is not None:
            self.process.send_signal(signal.SIGTERM)
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
            self.process = None
        shutil.rmtree(self.data_dir, ignore_errors=True)


def _free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class PitchforkTestCase(TestCase):
    """Base test case with a live pitchfork-server fixture."""

    fixtures = ["base_data"]

    server_fixture = None

    @classmethod
    def setUpClass(cls):
        super().setUpClass()
        # Embedded mode (Strategy B) runs in-process; no server fixture and
        # no HTTP client to point anywhere.
        if os.environ.get("PITCHFORK_MODE") == "embedded":
            return
        cls.server_fixture = PitchforkServerFixture().start()
        # Point the backend's HTTP client at the live fixture (the settings
        # URL is a placeholder; the fixture picks a free port).
        from haystack import connections

        backend = connections["default"].get_backend()
        backend.client.base_url = cls.server_fixture.url.rstrip("/")

    @classmethod
    def tearDownClass(cls):
        if cls.server_fixture is not None:
            cls.server_fixture.stop()
            cls.server_fixture = None
        super().tearDownClass()
