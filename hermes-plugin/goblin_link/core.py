"""What the plugin keeps and checks, with no Hermes in it: the certificate handhelds pin, the
one-time pairing codes, the paired devices and their tokens, and the limits on what a handheld
may send. Kept apart from the adapter so it can be tested on its own."""

from __future__ import annotations

import base64
import hashlib
import hmac
import ipaddress
import json
import os
import re
import secrets
import threading
import time
from pathlib import Path
from typing import Any

PROTOCOL = 1
DEFAULT_PORT = 47832
CODE_TTL_S = 600
MAX_PENDING_CODES = 4
# What a handheld may send as its command list: it is shown to the agent.
MAX_COMMANDS = 64
MAX_COMMANDS_CHARS = 64 * 1024
MAX_DEVICES = 16
MAX_TEXT_CHARS = 8192
MAX_VOICE_BYTES = 10 << 20
# Hermes's slash commands as the handheld's command sheet takes them: hermes-link's
# `read_commands` leaves out whatever goes past these.
MAX_LISTED = 300
MAX_DESCRIPTION_CHARS = 200
MAX_CATEGORY_CHARS = 40
MAX_ARGS_CHARS = 200
# The agent's own name and picture on the handheld (see `Store.set_identity`).
MAX_NAME_CHARS = 40
AVATAR_PX = 352  # what the handheld keeps: twice its largest avatar
MAX_IMAGE_FILE_BYTES = 10 << 20
MAX_IMAGE_SIDE = 4096
MAX_AVATAR_BYTES = 1 << 20

_ID = re.compile(r"[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}\Z")
_REVISION = re.compile(r"r_[0-9a-f]{16}\Z")
_HOST = re.compile(r"[A-Za-z0-9._-]{1,253}\Z")
_COMMAND_NAME = re.compile(r"[a-z0-9_-]{1,32}\Z")


def valid_id(value: Any) -> bool:
    """An id as the protocol allows it: short, and safe in a file name or a log line."""
    return isinstance(value, str) and _ID.match(value) is not None


def _digest(secret: str) -> str:
    # A string from the network may hold what UTF-8 cannot: hash it all the same.
    return hashlib.sha256(secret.encode("utf-8", "surrogatepass")).hexdigest()


class Refused(Exception):
    """A handheld's request was refused. ``code`` is for the handheld to act on; ``revoked``
    names a device that was unpaired as a result."""

    def __init__(self, reason: str, code: str | None = None, revoked: str | None = None):
        super().__init__(reason)
        self.reason, self.code, self.revoked = reason, code, revoked


class Store:
    """The plugin's state under ``<hermes home>/goblin-link``, readable by the owner alone.

    Tokens and codes are kept only as SHA-256 digests: reading the files does not let anyone
    pass for a handheld."""

    def __init__(self, directory: Path, now=time.time):
        self.dir, self._now = Path(directory), now
        self.dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chmod(self.dir, 0o700)
        self._devices_file = self.dir / "devices.json"
        # digest -> expiry, for codes waiting to be spent
        self._codes: dict[str, float] = {}
        # digest -> (expiry, device id), for codes already spent: a second use is a theft
        self._spent: dict[str, tuple[float, str]] = {}
        # Hermes calls in from its event loop and from worker threads (the slash command).
        self._lock = threading.RLock()

    # ---- the certificate -------------------------------------------------------------------

    def certificate(self, hosts: list[str]) -> tuple[Path, Path, str]:
        """The certificate and key files, made once, and the certificate's SHA-256.

        Handhelds trust the fingerprint, not the names inside: the certificate is never
        remade just because the addresses changed, or every handheld would have to pair again."""
        cert_file, key_file = self.dir / "cert.pem", self.dir / "key.pem"
        with self._lock:
            if not (cert_file.exists() and key_file.exists()):
                self._make_certificate(cert_file, key_file, hosts)
        from cryptography import x509
        from cryptography.hazmat.primitives import serialization

        cert = x509.load_pem_x509_certificate(cert_file.read_bytes())
        der = cert.public_bytes(serialization.Encoding.DER)
        return cert_file, key_file, hashlib.sha256(der).hexdigest()

    @staticmethod
    def _make_certificate(cert_file: Path, key_file: Path, hosts: list[str]) -> None:
        import datetime

        from cryptography import x509
        from cryptography.hazmat.primitives import hashes, serialization
        from cryptography.hazmat.primitives.asymmetric import ec
        from cryptography.x509.oid import NameOID

        key = ec.generate_private_key(ec.SECP256R1())
        name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Goblin Link plugin")])
        names: list[x509.GeneralName] = []
        for host in hosts:
            try:
                names.append(x509.IPAddress(ipaddress.ip_address(host)))
            except ValueError:
                names.append(x509.DNSName(host))
        now = datetime.datetime.now(datetime.timezone.utc)
        builder = (
            x509.CertificateBuilder()
            .subject_name(name)
            .issuer_name(name)
            .public_key(key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(now - datetime.timedelta(days=1))
            .not_valid_after(now + datetime.timedelta(days=3650))
        )
        if names:
            builder = builder.add_extension(x509.SubjectAlternativeName(names), critical=False)
        cert = builder.sign(key, hashes.SHA256())
        pem = key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        )
        # Each file appears whole or not at all, the key only ever with its final mode, and
        # the certificate last: a pair is complete once the certificate is there.
        for path, data, mode in ((key_file, pem, 0o600), (cert_file, cert.public_bytes(serialization.Encoding.PEM), 0o644)):
            tmp = path.with_suffix(".tmp")
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, mode)
            with os.fdopen(fd, "wb") as handle:
                handle.write(data)
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(tmp, path)

    # ---- pairing ---------------------------------------------------------------------------

    def new_code(self) -> str:
        """A one-time code: 160 random bits as 32 letters and digits, good for ten minutes."""
        with self._lock:
            self._expire()
            while len(self._codes) >= MAX_PENDING_CODES:
                self._codes.pop(min(self._codes, key=self._codes.get))
            code = base64.b32encode(secrets.token_bytes(20)).decode()
            self._codes[_digest(code)] = self._now() + CODE_TTL_S
            return code

    def link(self, hosts: list[str], port: int, fingerprint: str) -> str:
        """The link the owner pastes on the handheld's pairing page."""
        hosts = [h for h in hosts if _HOST.match(h)]
        if not hosts:
            raise Refused("set GOBLIN_LINK_HOSTS to the addresses a handheld can reach this machine at")
        return f"goblinlink://{','.join(hosts[:8])}:{port}/?fp={fingerprint}&code={self.new_code()}"

    def pair(self, frame: dict[str, Any]) -> tuple[str, str]:
        """Spends a code and gives a new device its token: ``(device id, token)``.

        A code presented a second time unpairs the device it paired, if that pairing still
        stands: one of the two who held the code was not the owner. Wrong codes cost nothing
        but the attempt: at 160 bits there is nothing to guess, and dropping the waiting
        codes on bad guesses would only let a stranger stop the owner from pairing."""
        code, device = frame.get("code"), frame.get("device")
        if frame.get("protocol") != PROTOCOL:
            raise Refused("this handheld speaks another version of the protocol; update it or the plugin")
        if not isinstance(code, str) or not isinstance(device, dict) or not valid_id(device.get("id")):
            raise Refused("that is not a pairing request")
        digest = _digest(code)
        with self._lock:
            self._expire()
            spent = self._spent.get(digest)
            if spent is not None:
                devices = self._load()
                if devices.get(spent[1], {}).get("code_sha256") == digest:
                    del devices[spent[1]]
                    self._save(devices)
                    raise Refused(
                        "that code was already used; the handheld it paired has been unpaired",
                        revoked=spent[1],
                    )
                raise Refused("that code was already used; ask Hermes for a new link")
            if not any(hmac.compare_digest(digest, known) for known in self._codes):
                raise Refused("that code is wrong or has expired; ask Hermes for a new link")
            devices = self._load()
            device_id = device["id"]
            if device_id not in devices and len(devices) >= MAX_DEVICES:
                raise Refused("too many handhelds are paired; unpair one first")
            del self._codes[digest]
            token = secrets.token_hex(32)
            name = device.get("name")
            devices[device_id] = {
                "name": clean(name, 64),
                "token_sha256": _digest(token),
                "code_sha256": digest,
                "paired_at": int(self._now()),
            }
            self._save(devices)
            self._spent[digest] = (self._now() + CODE_TTL_S, device_id)
            return device_id, token

    def hello(self, frame: dict[str, Any]) -> str:
        """Checks a handheld's hello and returns its device id."""
        if frame.get("protocol") != PROTOCOL:
            raise Refused("this handheld speaks another version of the protocol; update it or the plugin")
        device_id, token = frame.get("device"), frame.get("token")
        with self._lock:
            known = self._load().get(device_id) if valid_id(device_id) else None
        # Compare even for an unknown device, so the answer takes as long either way.
        expected = known["token_sha256"] if known else _digest("no such device")
        given = _digest(token) if isinstance(token, str) else ""
        if not hmac.compare_digest(given, expected) or not known:
            raise Refused("this handheld is not paired with this Hermes", code="unpaired")
        return device_id

    def devices(self) -> dict[str, dict[str, Any]]:
        """Paired handhelds by id, without their token digests."""
        with self._lock:
            devices = self._load()
        return {k: {"name": v.get("name", ""), "paired_at": v.get("paired_at", 0)} for k, v in devices.items()}

    def revoke(self, device_id: str) -> bool:
        with self._lock:
            devices = self._load()
            if devices.pop(device_id, None) is None:
                return False
            self._save(devices)
            return True

    # ---- the agent's name and picture on the handheld --------------------------------------

    def identity(self) -> dict[str, str]:
        """What the agent has chosen to be called and to look like on the handheld: ``name``
        and the picture's ``revision``, each only if set and usable. A damaged record or a
        picture that is not the one recorded is the same as none: the handheld falls back."""
        with self._lock:
            try:
                data = json.loads((self.dir / "identity.json").read_text())
            except (OSError, ValueError):
                return {}
            if not isinstance(data, dict):
                return {}
            found: dict[str, str] = {}
            try:
                found["name"] = display_name(data.get("name"))
            except Refused:
                pass
            revision = data.get("revision")
            if isinstance(revision, str) and self._picture(revision) is not None:
                found["revision"] = revision
            return found

    def avatar(self, revision: Any) -> bytes | None:
        """The picture, if ``revision`` is the one in use now."""
        with self._lock:
            if not isinstance(revision, str) or self.identity().get("revision") != revision:
                return None
            return self._picture(revision)

    def _picture_file(self, revision: str) -> Path:
        return self.dir / f"avatar-{revision}.png"

    def _picture(self, revision: str) -> bytes | None:
        # The revision goes into a file name: only what this plugin itself would have made.
        if _REVISION.match(revision) is None:
            return None
        try:
            with open(self._picture_file(revision), "rb") as handle:
                png = handle.read(MAX_AVATAR_BYTES + 1)
        except OSError:
            return None
        return png if 0 < len(png) <= MAX_AVATAR_BYTES and avatar_revision(png) == revision else None

    def set_identity(self, name: Any = None, image_path: Any = None) -> dict[str, str]:
        """Changes the name, the picture, or both; what is not given stays. Everything is
        checked before anything is replaced, so a refusal leaves the identity as it was."""
        if name is None and image_path is None:
            raise Refused("give a name, an image_path, or both")
        new_name = display_name(name) if name is not None else None
        png = avatar_png(image_path) if image_path is not None else None
        with self._lock:
            current = self.identity()
            record = dict(current)
            if new_name is not None:
                record["name"] = new_name
            # The new picture is written beside the old one, and the record names it last:
            # stopped anywhere on the way, the identity still reads as the old one, whole.
            try:
                if png is not None:
                    record["revision"] = avatar_revision(png)
                    self._write(self._picture_file(record["revision"]), png)
                self._write(self.dir / "identity.json", json.dumps(record, indent=2).encode())
            except OSError:
                raise Refused("the plugin could not save it; nothing was changed")
            # Also what a write that was cut short left behind.
            for old in self.dir.glob("avatar-*.png*"):
                if old != self._picture_file(record.get("revision", "")):
                    try:
                        old.unlink()
                    except OSError:
                        pass
            return record

    @staticmethod
    def _write(path: Path, data: bytes) -> None:
        tmp = path.with_suffix(path.suffix + ".tmp")
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)

    def _expire(self) -> None:
        now = self._now()
        self._codes = {k: v for k, v in self._codes.items() if v > now}
        self._spent = {k: v for k, v in self._spent.items() if v[0] > now}

    def _load(self) -> dict[str, dict[str, Any]]:
        try:
            data = json.loads(self._devices_file.read_text())
        except FileNotFoundError:
            return {}
        except (OSError, ValueError):
            # Unreadable is not "nobody is paired" for writing purposes: refuse to go on.
            raise Refused("the plugin cannot read its list of paired handhelds")
        if not isinstance(data, dict):
            raise Refused("the plugin's list of paired handhelds is damaged")
        return {k: v for k, v in data.items() if valid_id(k) and isinstance(v, dict) and isinstance(v.get("token_sha256"), str)}

    def _save(self, devices: dict[str, dict[str, Any]]) -> None:
        tmp = self._devices_file.with_suffix(".tmp")
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as handle:
            json.dump(devices, handle, indent=2)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, self._devices_file)


def display_name(value: Any) -> str:
    """A name as the handheld may show it: one short line of printable characters."""
    if not isinstance(value, str):
        raise Refused("the name is a short piece of text")
    name = " ".join(value.split())
    if not name or len(name) > MAX_NAME_CHARS or not all(c.isprintable() for c in name):
        raise Refused(f"the name is 1 to {MAX_NAME_CHARS} printable characters on one line")
    return name


def avatar_revision(png: bytes) -> str:
    """What names a picture: it changes whenever the picture does."""
    return "r_" + hashlib.sha256(png).hexdigest()[:16]


def avatar_png(image_path: Any) -> bytes:
    """The still image at a local path, as the square PNG the handheld shows. Only a file
    on this machine is read (never an address), and only if it is an image of a sane size."""
    if not isinstance(image_path, str) or not image_path or "://" in image_path or "\x00" in image_path:
        raise Refused("image_path is the path of an image file on this machine, not an address")
    path = Path(image_path).expanduser()
    try:
        if not path.is_file():
            raise Refused("there is no image file at image_path")
        # No more is read than may be used, whatever the file says its size is (some files
        # say nothing true about it, and one can grow while it is read).
        with open(path, "rb") as handle:
            raw = handle.read(MAX_IMAGE_FILE_BYTES + 1)
    except OSError:
        raise Refused("the image file cannot be read")
    if len(raw) > MAX_IMAGE_FILE_BYTES:
        raise Refused(f"the image file is larger than {MAX_IMAGE_FILE_BYTES >> 20} MB")
    try:
        from PIL import Image, ImageOps
    except ImportError:
        raise Refused("this Hermes has no Pillow to read images with")
    import io
    import warnings

    try:
        with warnings.catch_warnings():
            # A picture that declares an enormous size is refused, not decoded with a warning.
            warnings.simplefilter("error", Image.DecompressionBombWarning)
            with Image.open(io.BytesIO(raw)) as image:
                if image.format not in ("PNG", "JPEG", "WEBP"):
                    raise Refused("the image is a PNG, a JPEG or a WebP")
                width, height = image.size
                if not (64 <= width <= MAX_IMAGE_SIDE and 64 <= height <= MAX_IMAGE_SIDE):
                    raise Refused(f"the image is 64 to {MAX_IMAGE_SIDE} pixels on each side")
                image.seek(0)  # a moving picture gives its first frame
                image = ImageOps.exif_transpose(image)
                # A centred square, towards the top where a face is; as the handheld crops.
                # Cut before it is converted, so that no more is held than is kept.
                side = min(image.size)
                left = (image.width - side) // 2
                top = min((image.height - side) // 8, image.height - side)
                image = (
                    image.crop((left, top, left + side, top + side))
                    .convert("RGBA")
                    .resize((AVATAR_PX, AVATAR_PX), Image.LANCZOS)
                )
                out = io.BytesIO()
                image.save(out, format="PNG", optimize=True)
    except Refused:
        raise
    except Exception:
        raise Refused("the file is not an image that can be read")
    png = out.getvalue()
    if len(png) > MAX_AVATAR_BYTES:
        raise Refused("the image is too detailed to send to the handheld")
    return png


def hosts_from_env(value: str | None) -> list[str]:
    """``GOBLIN_LINK_HOSTS`` as a list, keeping only what can be a host name or an address."""
    return [h for h in (part.strip() for part in (value or "").split(",")) if h and _HOST.match(h)]


def turn_text(frame: dict[str, Any]) -> tuple[str | None, bytes | None]:
    """A turn's text or its voice note, checked: ``(text, wav)``; both ``None`` when neither is valid."""
    text, voice = frame.get("text"), frame.get("voice_wav_b64")
    if isinstance(text, str) and text.strip() and len(text) <= MAX_TEXT_CHARS:
        return text, None
    if isinstance(voice, str) and len(voice) <= MAX_VOICE_BYTES * 4 // 3 + 4:
        try:
            wav = base64.b64decode(voice, validate=True)
        except ValueError:
            return None, None
        if 44 <= len(wav) <= MAX_VOICE_BYTES and wav[:4] == b"RIFF":
            return None, wav
    return None, None


# Spaces other than U+0020 read as a space, so they become one: deleting them joins the
# words and digits they kept apart. The line and paragraph separators are line breaks.
_SPACES = dict.fromkeys(map(ord, "\u00a0\u1680\u202f\u205f\u3000"), " ")
_SPACES.update(dict.fromkeys(range(0x2000, 0x200B), " "))
_SPACES.update({0x2028: "\n", 0x2029: "\n"})
# A tab keeps its indentation as spaces: the handheld draws a tab as one space.
_SPACES[ord("\t")] = "    "
# What holds joined emoji (and some scripts) together. Each is invisible on its own, so a
# card, which must read as it was written, does not keep them.
_JOINERS = frozenset("\u200c\u200d")


def clean(text: Any, limit: int, *, joined: bool = False) -> str:
    """Text as it may be shown on the handheld: bounded, spaces and tabs as plain spaces,
    without control characters or the invisible ones that reorder or hide text.
    ``joined`` keeps the joiners in emoji: for a reply, never for a card or a status line.
    ``hermes-link``'s ``text`` and ``reply_text`` follow the same rule."""
    if not isinstance(text, str):
        return ""
    spaced = text.translate(_SPACES)
    return "".join(c for c in spaced if c == "\n" or c.isprintable() or (joined and c in _JOINERS))[:limit]


def open_prompts(prompts: dict[str, dict[str, Any]], **which: Any) -> list[str]:
    """The ids of the questions still waiting on a handheld that are all of ``which``
    (``chat="main"``, ``clarify_id="…"``), in the order they were asked."""
    return [prompt_id for prompt_id, how in prompts.items() if all(how.get(k) == v for k, v in which.items())]


def commands(value: Any) -> dict[str, Any]:
    """A handheld's command list as it may be shown to the agent: a bounded object keyed by
    command names, or nothing."""
    if not isinstance(value, dict) or len(value) > MAX_COMMANDS or not all(valid_id(k) for k in value):
        return {}
    try:
        if len(json.dumps(value)) > MAX_COMMANDS_CHARS:
            return {}
    except (TypeError, ValueError):
        return {}
    return value


def _one_line(text: Any, limit: int) -> str:
    """``clean`` text on one line, cut with an ellipsis to fit ``limit``."""
    flat = " ".join(clean(text, 4096).split())
    return flat if len(flat) <= limit else flat[: limit - 1] + "\u2026"


def listed_command(
    name: Any, description: Any, group: str, category: Any = "", args_hint: Any = ""
) -> dict[str, Any] | None:
    """One slash command as the handheld's command sheet takes it, or ``None`` for a name the
    handheld would refuse (it could not be sent back as written). The rest is made to fit."""
    if not isinstance(name, str) or _COMMAND_NAME.match(name) is None:
        return None
    hint = args_hint if isinstance(args_hint, str) else ""
    return {
        "name": name,
        "description": _one_line(description, MAX_DESCRIPTION_CHARS),
        "group": group,
        "category": _one_line(category, MAX_CATEGORY_CHARS),
        "args": _one_line(hint, MAX_ARGS_CHARS),
        # As Hermes's own menus judge it (``_requires_argument``): a hint that begins with
        # an angle bracket names something the command cannot do without.
        "needs_text": hint.strip().startswith("<"),
    }
