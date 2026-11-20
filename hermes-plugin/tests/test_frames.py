"""The frame limit over a real connection: run with
``python hermes-plugin/tests/test_frames.py``. It starts the adapter's websockets server on
127.0.0.1 with Hermes stood in for, connects to it as a handheld would, and sends frames
larger than a stranger may send, before and after hello. It needs the ``websockets`` and
``cryptography`` packages, and is skipped without them. The limit is kept under a different
name from websockets 16 on, so run it under each version the plugin supports, for example:

    for v in 13.1 15.0.1 16.0 17.4; do docker run --rm -v "$PWD/hermes-plugin:/p:ro" \\
      python:3.12-slim sh -c "pip -q install websockets==$v cryptography && \\
      python /p/tests/test_frames.py"; done
"""

import asyncio
import json
import logging
import ssl
import sys
import tempfile
import types
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_adapter import Approvals, stand_in  # noqa: E402

try:
    import cryptography  # noqa: F401
    import websockets
    from websockets.asyncio.client import connect
    from websockets.exceptions import ConnectionClosed
except ImportError:
    websockets = None

TOKEN = "t" * 43
LARGE = 300 * 1024  # over the 256 KiB a stranger may send, far under the 16 MiB after hello


@unittest.skipIf(websockets is None, "needs the websockets and cryptography packages")
class FrameLimitTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        stand_in(Approvals())
        from goblin_link import adapter, core

        class Store(core.Store):
            """Pairing is tested elsewhere: here one handheld, ``dev-1``, is known."""

            def hello(self, frame):
                if frame.get("device") == "dev-1" and frame.get("token") == TOKEN:
                    return "dev-1"
                raise core.Refused("not paired")

        self.module = adapter
        self.home = tempfile.TemporaryDirectory()
        self.adapter = object.__new__(adapter.GoblinLinkAdapter)
        self.adapter.store = Store(Path(self.home.name))
        self.adapter.hosts, self.adapter.bind, self.adapter.port = ["127.0.0.1"], "127.0.0.1", 0
        self.adapter._server, self.adapter._handhelds, self.adapter._strangers = None, {}, 0
        self.adapter._mark_connected = lambda: None
        self.frames = asyncio.Queue()

        async def on_frame(handheld, frame):
            await self.frames.put(frame)

        self.adapter._on_frame = on_frame
        self.assertTrue(await self.adapter.connect())
        port = self.adapter._server.sockets[0].getsockname()[1]
        self.url = f"wss://127.0.0.1:{port}/goblin-link"
        self.tls = ssl.create_default_context()
        self.tls.check_hostname, self.tls.verify_mode = False, ssl.CERT_NONE

    async def asyncTearDown(self):
        await self.adapter.disconnect()
        self.home.cleanup()

    def handheld(self):
        return connect(self.url, ssl=self.tls, max_size=None)

    async def say_hello(self, socket):
        await socket.send(json.dumps({"type": "hello", "protocol": 1, "device": "dev-1", "token": TOKEN}))
        self.assertEqual(json.loads(await asyncio.wait_for(socket.recv(), 5))["type"], "hello.ok")

    async def closed_with(self, socket):
        with self.assertRaises(ConnectionClosed) as closed:
            await asyncio.wait_for(socket.recv(), 5)
        return closed.exception.rcvd.code if closed.exception.rcvd else None

    async def test_a_large_frame_after_hello_arrives(self):
        async with self.handheld() as socket:
            await self.say_hello(socket)
            await socket.send(json.dumps({"type": "probe", "data": "a" * LARGE}))
            frame = await asyncio.wait_for(self.frames.get(), 5)
            self.assertEqual(len(frame["data"]), LARGE, websockets.__version__)

    async def test_the_commands_hermes_offers_follow_hello(self):
        from test_adapter import Registry

        Registry().install(self)
        async with self.handheld() as socket:
            await self.say_hello(socket)
            said = json.loads(await asyncio.wait_for(socket.recv(), 5))
            self.assertEqual(said["type"], "commands")
            self.assertIn("model", [item["name"] for item in said["items"]])

    async def test_a_large_first_frame_closes_the_connection(self):
        async with self.handheld() as socket:
            await socket.send(json.dumps({"type": "hello", "pad": "a" * LARGE}))
            self.assertEqual(await self.closed_with(socket), 1009)

    async def test_a_large_frame_after_a_refused_hello_is_not_read(self):
        # A stranger whose hello is refused gets its answer and the connection ends there:
        # the larger limit is never given to it.
        async with self.handheld() as socket:
            await socket.send(json.dumps({"type": "hello", "protocol": 1, "device": "dev-2", "token": TOKEN}))
            self.assertEqual(json.loads(await asyncio.wait_for(socket.recv(), 5))["type"], "error")
            try:
                await socket.send(json.dumps({"type": "probe", "data": "a" * LARGE}))
            except ConnectionClosed:
                pass
            await self.closed_with(socket)
        self.assertTrue(self.frames.empty())


class UnsupportedTests(unittest.TestCase):
    def test_a_websockets_that_keeps_the_limit_elsewhere_is_refused_plainly(self):
        stand_in(Approvals())
        from goblin_link import adapter

        server = types.ModuleType("websockets.server")
        server.ServerProtocol = lambda max_size: types.SimpleNamespace(limits=max_size)
        asyncio_server = types.ModuleType("websockets.asyncio.server")
        asyncio_server.serve = None
        names = ("websockets.server", "websockets.asyncio.server")
        kept = {name: sys.modules.get(name) for name in names}
        sys.modules.update({"websockets.server": server, "websockets.asyncio.server": asyncio_server})
        try:
            with self.assertLogs(adapter.logger, logging.ERROR) as said:
                self.assertFalse(adapter._available())
        finally:
            for name, module in kept.items():
                if module is None:
                    sys.modules.pop(name, None)
                else:
                    sys.modules[name] = module
        self.assertIn("needs websockets 13 to 17", said.output[0])


if __name__ == "__main__":
    unittest.main()
