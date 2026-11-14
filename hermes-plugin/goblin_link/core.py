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
import time
from pathlib import Path
from typing import Any

PROTOCOL = 1
DEFAULT_PORT = 47832
CODE_TTL_S = 600
# After this many wrong codes every waiting code is dropped: guessing is over, pair again.
MAX_BAD_CODES = 5
MAX_PENDING_CODES = 4
MAX_DEVICES = 16
MAX_TEXT_CHARS = 8192
MAX_VOICE_BYTES = 10 << 20

_ID = re.compile(r"[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}\Z")
_HOST = re.compile(r"[A-Za-z0-9._-]{1,253}\Z")


def valid_id(value: Any) -> bool:
    """An id as the protocol allows it: short, and safe in a file name or a log line."""
    return isinstance(value, str) and _ID.match(value) is not None


def _digest(secret: str) -> str:
    return hashlib.sha256(secret.encode()).hexdigest()


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
        self._bad_codes = 0

    # ---- the certificate -------------------------------------------------------------------

    def certificate(self, hosts: list[str]) -> tuple[Path, Path, str]:
        """The certificate and key files, made once, and the certificate's SHA-256.

        Handhelds trust the fingerprint, not the names inside: the certificate is never
        remade just because the addresses changed, or every handheld would have to pair again."""
        cert_file, key_file = self.dir / "cert.pem", self.dir / "key.pem"
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
        # The key file exists only ever with its final mode.
        fd = os.open(key_file, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "wb") as handle:
            handle.write(pem)
        cert_file.write_bytes(cert.public_bytes(serialization.Encoding.PEM))

    # ---- pairing ---------------------------------------------------------------------------

    def new_code(self) -> str:
        """A one-time code: 160 random bits as 32 letters and digits, good for ten minutes."""
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

        A code presented a second time unpairs the device it paired: one of the two who held
        that code was not the owner."""
        self._expire()
        code, device = frame.get("code"), frame.get("device")
        if frame.get("protocol") != PROTOCOL:
            raise Refused("this handheld speaks another version of the protocol; update it or the plugin")
        if not isinstance(code, str) or not isinstance(device, dict) or not valid_id(device.get("id")):
            raise Refused("that is not a pairing request")
        digest = _digest(code)
        spent = self._spent.get(digest)
        if spent is not None:
            self.revoke(spent[1])
            raise Refused(
                "that code was already used; the handheld it paired has been unpaired",
                revoked=spent[1],
            )
        if not any(hmac.compare_digest(digest, known) for known in self._codes):
            self._bad_codes += 1
            if self._bad_codes >= MAX_BAD_CODES:
                self._codes.clear()
                self._bad_codes = 0
            raise Refused("that code is wrong or has expired; ask Hermes for a new link")
        del self._codes[digest]
        self._bad_codes = 0
        devices = self._load()
        device_id = device["id"]
        if device_id not in devices and len(devices) >= MAX_DEVICES:
            raise Refused("too many handhelds are paired; unpair one first")
        token = secrets.token_hex(32)
        name = device.get("name")
        devices[device_id] = {
            "name": name[:64] if isinstance(name, str) else "",
            "token_sha256": _digest(token),
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
        known = self._load().get(device_id) if valid_id(device_id) else None
        # Compare even for an unknown device, so the answer takes as long either way.
        expected = known["token_sha256"] if known else _digest("no such device")
        given = _digest(token) if isinstance(token, str) else ""
        if not hmac.compare_digest(given, expected) or not known:
            raise Refused("this handheld is not paired with this Hermes", code="unpaired")
        return device_id

    def devices(self) -> dict[str, dict[str, Any]]:
        """Paired handhelds by id, without their token digests."""
        return {k: {"name": v.get("name", ""), "paired_at": v.get("paired_at", 0)} for k, v in self._load().items()}

    def revoke(self, device_id: str) -> bool:
        devices = self._load()
        if devices.pop(device_id, None) is None:
            return False
        self._save(devices)
        return True

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


def clean(text: Any, limit: int) -> str:
    """Text as it may be shown on the handheld: bounded, without control characters."""
    if not isinstance(text, str):
        return ""
    return "".join(c for c in text if c == "\n" or c.isprintable())[:limit]
