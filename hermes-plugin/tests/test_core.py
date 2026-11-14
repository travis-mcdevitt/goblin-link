"""The plugin's core, with no Hermes: run with ``python -m unittest discover -s hermes-plugin/tests``."""

import base64
import json
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from goblin_link import core  # noqa: E402  (imports core alone: no Hermes needed)


class Clock:
    def __init__(self):
        self.now = 1_000_000.0

    def __call__(self):
        return self.now


def pair_frame(code, device="dev-1", protocol=core.PROTOCOL):
    return {"type": "pair", "protocol": protocol, "code": code, "device": {"id": device, "name": "Brick"}}


class StoreTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.clock = Clock()
        self.store = core.Store(Path(self.tmp.name) / "goblin-link", now=self.clock)

    def tearDown(self):
        self.tmp.cleanup()

    def test_a_code_pairs_once_and_the_token_is_kept_only_as_a_digest(self):
        code = self.store.new_code()
        self.assertEqual(len(code), 32)
        self.assertTrue(code.isalnum())
        device, token = self.store.pair(pair_frame(code))
        self.assertEqual(device, "dev-1")
        self.assertEqual(len(token), 64)
        saved = (self.store.dir / "devices.json").read_text()
        self.assertNotIn(token, saved)
        self.assertEqual(stat.S_IMODE(os.stat(self.store.dir / "devices.json").st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(os.stat(self.store.dir).st_mode), 0o700)
        hello = {"protocol": core.PROTOCOL, "device": "dev-1", "token": token}
        self.assertEqual(self.store.hello(hello), "dev-1")

    def test_a_code_used_twice_unpairs_the_handheld_it_paired(self):
        code = self.store.new_code()
        _, token = self.store.pair(pair_frame(code))
        with self.assertRaises(core.Refused) as again:
            self.store.pair(pair_frame(code, device="dev-2"))
        self.assertEqual(again.exception.revoked, "dev-1")
        with self.assertRaises(core.Refused) as gone:
            self.store.hello({"protocol": core.PROTOCOL, "device": "dev-1", "token": token})
        self.assertEqual(gone.exception.code, "unpaired")
        self.assertEqual(self.store.devices(), {})

    def test_wrong_expired_and_guessed_codes_are_refused(self):
        code = self.store.new_code()
        for _ in range(core.MAX_BAD_CODES):
            with self.assertRaises(core.Refused):
                self.store.pair(pair_frame("A" * 32))
        # Enough wrong guesses drop every waiting code, the right one too.
        with self.assertRaises(core.Refused):
            self.store.pair(pair_frame(code))
        late = self.store.new_code()
        self.clock.now += core.CODE_TTL_S + 1
        with self.assertRaises(core.Refused):
            self.store.pair(pair_frame(late))
        for bad in (
            {"protocol": core.PROTOCOL, "code": 7, "device": {"id": "d"}},
            {"protocol": core.PROTOCOL, "code": "x", "device": {"id": "../etc"}},
            {"protocol": core.PROTOCOL, "code": "x", "device": "d"},
            pair_frame(self.store.new_code(), protocol=2),
        ):
            with self.assertRaises(core.Refused):
                self.store.pair(bad)

    def test_hello_needs_the_right_token_for_that_device(self):
        _, token = self.store.pair(pair_frame(self.store.new_code()))
        _, other = self.store.pair(pair_frame(self.store.new_code(), device="dev-2"))
        for frame in (
            {"protocol": core.PROTOCOL, "device": "dev-1", "token": other},
            {"protocol": core.PROTOCOL, "device": "dev-3", "token": token},
            {"protocol": core.PROTOCOL, "device": "dev-1", "token": None},
            {"protocol": core.PROTOCOL, "device": ["dev-1"], "token": token},
            {"protocol": 99, "device": "dev-1", "token": token},
        ):
            with self.assertRaises(core.Refused, msg=frame):
                self.store.hello(frame)
        self.assertTrue(self.store.revoke("dev-2"))
        self.assertFalse(self.store.revoke("dev-2"))
        self.assertEqual(list(self.store.devices()), ["dev-1"])

    def test_a_damaged_device_list_is_not_read_as_empty(self):
        self.store.pair(pair_frame(self.store.new_code()))
        (self.store.dir / "devices.json").write_text("{not json")
        with self.assertRaises(core.Refused):
            self.store.pair(pair_frame(self.store.new_code(), device="dev-2"))
        self.assertEqual((self.store.dir / "devices.json").read_text(), "{not json")

    def test_the_certificate_is_made_once_and_its_key_is_private(self):
        cert, key, fingerprint = self.store.certificate(["192.168.1.20", "host.tail.ts.net"])
        self.assertEqual(len(fingerprint), 64)
        self.assertEqual(stat.S_IMODE(os.stat(key).st_mode), 0o600)
        # Other addresses later do not remake it: handhelds pinned this one.
        self.assertEqual(self.store.certificate(["10.0.0.1"])[2], fingerprint)
        link = self.store.link(["192.168.1.20", "bad host", "host.tail.ts.net"], 47832, fingerprint)
        self.assertRegex(link, r"^goblinlink://192\.168\.1\.20,host\.tail\.ts\.net:47832/\?fp=[0-9a-f]{64}&code=[A-Z2-7]{32}$")
        with self.assertRaises(core.Refused):
            self.store.link([], 47832, fingerprint)


class FrameTests(unittest.TestCase):
    def test_a_turn_is_text_or_a_wav_and_nothing_else(self):
        self.assertEqual(core.turn_text({"text": "hi"}), ("hi", None))
        wav = b"RIFF" + bytes(60)
        encoded = base64.b64encode(wav).decode()
        self.assertEqual(core.turn_text({"voice_wav_b64": encoded}), (None, wav))
        for bad in (
            {},
            {"text": "   "},
            {"text": "x" * (core.MAX_TEXT_CHARS + 1)},
            {"text": 5},
            {"voice_wav_b64": "not base64!"},
            {"voice_wav_b64": base64.b64encode(b"MP3 " + bytes(60)).decode()},
            {"voice_wav_b64": base64.b64encode(b"RIFF").decode()},
        ):
            self.assertEqual(core.turn_text(bad), (None, None), json.dumps(bad)[:60])

    def test_ids_hosts_and_shown_text(self):
        for good in ("dev-1", "t12", "a.b_c-d", "A" * 128):
            self.assertTrue(core.valid_id(good), good)
        for bad in ("", ".hidden", "a/b", "a b", "A" * 129, None, 7, "a\n"):
            self.assertFalse(core.valid_id(bad), repr(bad))
        self.assertEqual(core.hosts_from_env(" a.example , bad host,10.0.0.2,, "), ["a.example", "10.0.0.2"])
        self.assertEqual(core.clean("ok\x07‮\nnext", 10), "ok\nnext")
        self.assertEqual(core.clean("abcdef", 3), "abc")
        self.assertEqual(core.clean(None, 3), "")


if __name__ == "__main__":
    unittest.main()
