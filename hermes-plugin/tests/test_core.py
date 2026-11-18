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

    def test_wrong_and_expired_codes_are_refused_and_cost_the_owner_nothing(self):
        code = self.store.new_code()
        # A stranger guessing does not stop the owner from pairing.
        for _ in range(50):
            with self.assertRaises(core.Refused):
                self.store.pair(pair_frame("A" * 32))
        self.assertEqual(self.store.pair(pair_frame(code))[0], "dev-1")
        late = self.store.new_code()
        self.clock.now += core.CODE_TTL_S + 1
        with self.assertRaises(core.Refused):
            self.store.pair(pair_frame(late))
        for bad in (
            {"protocol": core.PROTOCOL, "code": 7, "device": {"id": "d"}},
            {"protocol": core.PROTOCOL, "code": "x", "device": {"id": "../etc"}},
            {"protocol": core.PROTOCOL, "code": "x", "device": "d"},
            {"protocol": core.PROTOCOL, "code": "\ud800", "device": {"id": "d"}},
            pair_frame(self.store.new_code(), protocol=2),
        ):
            with self.assertRaises(core.Refused):
                self.store.pair(bad)

    def test_an_old_code_cannot_unpair_a_handheld_paired_again_since(self):
        first = self.store.new_code()
        self.store.pair(pair_frame(first))
        second = self.store.new_code()
        _, token = self.store.pair(pair_frame(second))
        # Someone who saw the first link replays it: the present pairing is not theirs to end.
        with self.assertRaises(core.Refused) as replay:
            self.store.pair(pair_frame(first, device="dev-9"))
        self.assertIsNone(replay.exception.revoked)
        self.assertEqual(self.store.hello({"protocol": core.PROTOCOL, "device": "dev-1", "token": token}), "dev-1")

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

    def test_a_command_list_is_bounded_before_the_agent_sees_it(self):
        good = {"file.read": {"description": "Read a file"}}
        self.assertEqual(core.commands(good), good)
        self.assertEqual(core.commands({"a b": {}}), {})
        self.assertEqual(core.commands({f"c{n}": {} for n in range(core.MAX_COMMANDS + 1)}), {})
        self.assertEqual(core.commands({"big": {"description": "x" * core.MAX_COMMANDS_CHARS}}), {})
        self.assertEqual(core.commands(["file.read"]), {})

    def test_ids_hosts_and_shown_text(self):
        for good in ("dev-1", "t12", "a.b_c-d", "A" * 128):
            self.assertTrue(core.valid_id(good), good)
        for bad in ("", ".hidden", "a/b", "a b", "A" * 129, None, 7, "a\n"):
            self.assertFalse(core.valid_id(bad), repr(bad))
        self.assertEqual(core.hosts_from_env(" a.example , bad host,10.0.0.2,, "), ["a.example", "10.0.0.2"])
        self.assertEqual(core.clean("ok\x07‮\nnext", 10), "ok\nnext")
        self.assertEqual(core.clean("abcdef", 3), "abc")
        self.assertEqual(core.clean(None, 3), "")

    def test_spacing_reads_as_written(self):
        # The same cases as hermes-link's tests of `text` and `reply_text`.
        for sent, shown in (
            ("10\u00a0000 km\tok", "10 000 km    ok"),
            ("Prix\u00a0: 3\u2009000\u00a0\u20ac\u202f!", "Prix : 3 000 \u20ac !"),
            ("\u3000x\u1680y\u205fz", " x y z"),
            ("a\u2028b\u2029c", "a\nb\nc"),
            ("if x:\n\treturn", "if x:\n    return"),
        ):
            self.assertEqual(core.clean(sent, 100), shown, repr(sent))
            self.assertEqual(core.clean(sent, 100, joined=True), shown, repr(sent))
        # The limit counts what is shown.
        self.assertEqual(core.clean("\tx", 3), "   ")

    def test_joined_emoji_stay_joined_in_a_reply_and_not_on_a_card(self):
        family = "\U0001F468\u200d\U0001F469\u200d\U0001F467 ok"
        self.assertEqual(core.clean(family, 100, joined=True), family)
        self.assertEqual(core.clean(family, 100), "\U0001F468\U0001F469\U0001F467 ok")
        self.assertEqual(core.clean("a\u200cb", 100, joined=True), "a\u200cb")

    def test_what_must_stay_out_still_does(self):
        refused = "\x07\r\x1b\x7f\x85\u00ad\u061c\u200b\u200e\u202e\u2060\u2066\ufeff\U000E0001\U000E0041\U000E007F"
        for c in refused:
            self.assertEqual(core.clean(f"a{c}b", 100), "ab", f"U+{ord(c):04X}")
            self.assertEqual(core.clean(f"a{c}b", 100, joined=True), "ab", f"U+{ord(c):04X}")

    def test_open_questions_are_found_by_what_they_are(self):
        prompts = {
            "p-1": {"kind": "approval", "chat": "main", "request_id": "r1"},
            "p-2": {"kind": "clarify", "chat": "main", "clarify_id": "c1"},
            "p-3": {"kind": "clarify", "chat": "side", "clarify_id": "c2"},
        }
        self.assertEqual(core.open_prompts(prompts, chat="main"), ["p-1", "p-2"])
        self.assertEqual(core.open_prompts(prompts, kind="clarify", clarify_id="c2"), ["p-3"])
        self.assertEqual(core.open_prompts(prompts, kind="approval", clarify_id="c1"), [])
        self.assertEqual(core.open_prompts(prompts, chat="other"), [])
        self.assertEqual(core.open_prompts({}, chat="main"), [])

class IdentityTests(unittest.TestCase):
    """The name and picture the agent chooses for itself on the handheld."""

    def setUp(self):
        try:
            from PIL import Image
        except ImportError:
            self.skipTest("Pillow is not installed")
        self.Image = Image
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.store = core.Store(self.dir / "goblin-link")

    def tearDown(self):
        self.tmp.cleanup()

    def image(self, name="face.png", size=(640, 480), colour=(200, 120, 40), fmt=None):
        path = self.dir / name
        self.Image.new("RGB", size, colour).save(path, format=fmt)
        return str(path)

    def test_nothing_is_set_until_the_agent_sets_it(self):
        self.assertEqual(self.store.identity(), {})
        self.assertIsNone(self.store.avatar("r_0000000000000000"))
        with self.assertRaises(core.Refused):
            self.store.set_identity()

    def test_a_name_and_a_picture_are_kept_and_the_picture_is_made_small_and_square(self):
        saved = self.store.set_identity(name="  Pip \t the  Goblin ", image_path=self.image())
        self.assertEqual(saved["name"], "Pip the Goblin")
        png = self.store.avatar(saved["revision"])
        self.assertEqual(saved["revision"], core.avatar_revision(png))
        import io

        with self.Image.open(io.BytesIO(png)) as kept:
            self.assertEqual((kept.format, kept.size), ("PNG", (core.AVATAR_PX, core.AVATAR_PX)))
        kept_file = self.store.dir / f"avatar-{saved['revision']}.png"
        self.assertEqual(stat.S_IMODE(os.stat(kept_file).st_mode), 0o600)
        # A second store over the same folder reads it back: it outlives the gateway.
        again = core.Store(self.dir / "goblin-link")
        self.assertEqual(again.identity(), saved)

    def test_each_part_changes_alone_and_an_old_revision_is_no_longer_given(self):
        first = self.store.set_identity(name="Pip", image_path=self.image())
        renamed = self.store.set_identity(name="Pop")
        self.assertEqual((renamed["name"], renamed["revision"]), ("Pop", first["revision"]))
        second = self.store.set_identity(image_path=self.image("other.jpg", colour=(10, 20, 30), fmt="JPEG"))
        self.assertEqual(second["name"], "Pop")
        self.assertNotEqual(second["revision"], first["revision"])
        self.assertIsNone(self.store.avatar(first["revision"]))
        self.assertIsNotNone(self.store.avatar(second["revision"]))
        # Only the picture in use is kept.
        self.assertEqual([p.name for p in self.store.dir.glob("avatar-*.png")], [f"avatar-{second['revision']}.png"])

    def test_a_change_that_cannot_be_saved_leaves_the_old_identity_whole(self):
        before = self.store.set_identity(name="Pip", image_path=self.image())
        other = self.image("other.png", colour=(1, 2, 3))
        real_write, calls = core.Store._write, []

        def failing(path, data):
            calls.append(path.name)
            if path.name == "identity.json":
                raise OSError("disk full")
            real_write(path, data)

        core.Store._write = staticmethod(failing)
        try:
            with self.assertRaises(core.Refused):
                self.store.set_identity(name="Pop", image_path=other)
        finally:
            core.Store._write = staticmethod(real_write)
        # The new picture had been written; the record still names the old one, and it is there.
        self.assertEqual(len(calls), 2)
        self.assertEqual(self.store.identity(), before)
        self.assertIsNotNone(self.store.avatar(before["revision"]))

    def test_no_more_of_a_file_is_read_than_may_be_used(self):
        # A regular file that says it is empty and gives data almost without end when read.
        endless = "/proc/self/pagemap"
        if not (os.path.isfile(endless) and os.stat(endless).st_size == 0):
            self.skipTest("no such file here")
        read = []
        real_open = open

        def counting(path, *args, **kwargs):
            real = real_open(path, *args, **kwargs)
            if str(path) != endless:
                return real
            return type("Counted", (), {
                "read": lambda self, n=-1: read.append(n) or real.read(n),
                "__enter__": lambda self: self,
                "__exit__": lambda self, *a: real.close(),
            })()

        core.open = counting
        try:
            with self.assertRaises(core.Refused):
                core.avatar_png(endless)
        finally:
            del core.open
        self.assertEqual(read, [core.MAX_IMAGE_FILE_BYTES + 1])

    def test_what_is_refused_changes_nothing(self):
        before = self.store.set_identity(name="Pip", image_path=self.image())
        text = self.dir / "notes.png"
        text.write_text("not an image")
        huge = self.dir / "huge.png"
        with open(huge, "wb") as handle:
            handle.truncate(core.MAX_IMAGE_FILE_BYTES + 1)
        refused = [
            {"name": ""},
            {"name": "x" * (core.MAX_NAME_CHARS + 1)},
            {"name": "bell\x07"},
            {"name": 7},
            {"image_path": "https://example.com/face.png"},
            {"image_path": str(self.dir / "missing.png")},
            {"image_path": str(self.dir)},
            {"image_path": str(text)},
            {"image_path": str(huge)},
            {"image_path": self.image("tiny.png", size=(32, 32))},
            {"image_path": self.image("wide.png", size=(core.MAX_IMAGE_SIDE + 1, 64))},
            {"image_path": self.image("anim.gif", fmt="GIF")},
            # A good name with a bad picture: neither is taken.
            {"name": "Pop", "image_path": str(text)},
        ]
        for args in refused:
            with self.subTest(args=args), self.assertRaises(core.Refused):
                self.store.set_identity(**args)
            self.assertEqual(self.store.identity(), before)

    def test_a_damaged_record_or_picture_falls_back_to_none(self):
        saved = self.store.set_identity(name="Pip", image_path=self.image())
        (self.store.dir / f"avatar-{saved['revision']}.png").write_bytes(b"\x89PNG\r\n\x1a\nsomething else")
        self.assertIsNone(self.store.avatar("../../etc/passwd"))
        self.assertEqual(self.store.identity(), {"name": "Pip"})
        self.assertIsNone(self.store.avatar(saved["revision"]))
        (self.store.dir / "identity.json").write_text("{broken")
        self.assertEqual(self.store.identity(), {})


if __name__ == "__main__":
    unittest.main()
