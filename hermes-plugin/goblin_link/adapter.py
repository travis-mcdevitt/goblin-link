"""The Goblin Link platform: handhelds connect to this plugin over a WebSocket with TLS and
become Hermes chats. See ``docs/hermes-protocol.md`` in the Goblin Link repository.

What a handheld may do is decided in two places and this file is neither: Hermes decides what
its agent does, and the handheld's own daemon decides which of its commands may run. This
adapter authenticates handhelds (``core.Store``) and carries messages.
"""

from __future__ import annotations

import asyncio
import base64
import json
import logging
import os
import shutil
import ssl
import subprocess
import tempfile
import uuid
from datetime import UTC, datetime
from typing import Any

from gateway.config import Platform, PlatformConfig
from gateway.platforms.base import (
    BasePlatformAdapter,
    ExecApprovalPrompt,
    MessageEvent,
    MessageType,
    ProcessingOutcome,
    SendResult,
)

from . import core

logger = logging.getLogger(__name__)

PLATFORM = "goblin_link"
MAX_FRAME_BYTES = 16 << 20  # a voice note of up to ten minutes, base64
# Until a handheld has said who it is, it gets little: a small first message, a short wait,
# and only a few such strangers at once.
MAX_FIRST_FRAME_BYTES = 256 * 1024
FIRST_FRAME_S = 10
MAX_STRANGERS = 8
MAX_PROMPT_CHARS = 2000
# Speech for a reply: how much text, how much audio, and how many at once per handheld.
MAX_SPEECH_CHARS = 4096
MAX_SPEECH_BYTES = 2 << 20
MAX_SPEAKING = 2
TOOL_GRACE_S = 5
MAX_REPLY_CHARS = 48_000

# The adapter Hermes is running, for the tools and the slash command (one per process).
_ADAPTER: "GoblinLinkAdapter | None" = None


class Handheld:
    """One connected, authenticated handheld."""

    def __init__(self, device_id: str, socket: Any, commands: Any):
        self.device_id, self.socket = device_id, socket
        self.commands = core.commands(commands)
        self.lock = asyncio.Lock()
        # chat -> the turn being answered there
        self.turns: dict[str, dict[str, Any]] = {}
        # call id -> who waits for the command's result
        self.calls: dict[str, asyncio.Future] = {}
        # prompt id -> how to resolve it
        self.prompts: dict[str, dict[str, Any]] = {}
        # replies being turned into speech
        self.speaking: set[asyncio.Task] = set()
        # chat -> the voice note being heard there as the answer to a question
        self.answering: dict[str, asyncio.Task] = {}

    async def send(self, frame: dict[str, Any]) -> bool:
        try:
            async with self.lock:
                await self.socket.send(json.dumps(frame))
            return True
        except Exception:  # the connection is gone; its handler cleans up
            return False


class GoblinLinkAdapter(BasePlatformAdapter):
    """Listens for handhelds. A chat id is ``<device id>:<chat>``; ``chat`` is ``main`` or a
    side chat's id, so each is its own Hermes conversation."""

    REQUIRES_EDIT_FINALIZE = True

    def __init__(self, config: PlatformConfig):
        super().__init__(config=config, platform=Platform(PLATFORM))
        from hermes_constants import get_hermes_home

        self.store = core.Store(get_hermes_home() / "goblin-link")
        self.hosts = core.hosts_from_env(os.environ.get("GOBLIN_LINK_HOSTS"))
        try:
            self.port = int(os.environ.get("GOBLIN_LINK_PORT") or core.DEFAULT_PORT)
        except ValueError:
            self.port = core.DEFAULT_PORT
        self.bind = os.environ.get("GOBLIN_LINK_BIND") or "0.0.0.0"
        self.fingerprint = ""
        self.loop: asyncio.AbstractEventLoop | None = None
        self._server: Any = None
        self._handhelds: dict[str, Handheld] = {}
        self._strangers = 0

    # ---- lifecycle -----------------------------------------------------------------------------

    async def connect(self, *, is_reconnect: bool = False) -> bool:
        global _ADAPTER
        self._limit = _frame_limit()
        if self._limit is None:
            logger.error(_UNSUPPORTED, _websockets_version())
            return False
        from websockets.asyncio.server import serve

        try:
            cert, key, self.fingerprint = self.store.certificate(self.hosts)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            context.load_cert_chain(cert, key)
            self._server = await serve(
                self._serve, self.bind, self.port, ssl=context, max_size=MAX_FIRST_FRAME_BYTES
            )
        except Exception as exc:
            logger.error("Goblin Link cannot listen on %s:%s: %s", self.bind, self.port, exc)
            return False
        self.loop = asyncio.get_running_loop()
        _ADAPTER = self
        self._running = True
        self._mark_connected()
        logger.info("Goblin Link listening on %s:%s", self.bind, self.port)
        return True

    async def disconnect(self) -> None:
        global _ADAPTER
        self._running = False
        if self._server is not None:
            self._server.close()
            await self._server.wait_closed()
            self._server = None
        if _ADAPTER is self:
            _ADAPTER = None

    async def reauthorize_native_roles(self, source: Any) -> bool:
        """Hermes asks this before it acts on a message the adapter vouched for
        (``role_authorized``), and again when it resumes one later. The answer is read from
        the pairing list as it is now: a handheld unpaired meanwhile is no longer let in."""
        device_id = getattr(source, "user_id", None)
        chat_id = getattr(source, "chat_id", "") or ""
        return (
            core.valid_id(device_id)
            and chat_id.partition(":")[0] == device_id
            and device_id in self.store.devices()
            and device_id in self._handhelds
        )

    # ---- one handheld's connection -------------------------------------------------------------

    async def _serve(self, socket: Any) -> None:
        if getattr(getattr(socket, "request", None), "path", "") != "/goblin-link":
            return
        if self._strangers >= MAX_STRANGERS:
            return
        self._strangers += 1
        try:
            device_id, first = await self._greet(socket)
        except Exception as exc:  # whatever a stranger sends, the gateway carries on
            logger.info("Goblin Link: a connection was dropped before it said who it was: %s", type(exc).__name__)
            return
        finally:
            self._strangers -= 1
        if device_id is None:
            return
        # Known now: voice notes need room.
        setattr(socket.protocol, self._limit, MAX_FRAME_BYTES)
        handheld = Handheld(device_id, socket, first.get("commands"))
        # A handheld connects once: a new connection replaces the old one.
        await self._drop(device_id)
        self._handhelds[device_id] = handheld
        await handheld.send({"type": "hello.ok", **self._identity()})
        try:
            async for raw in socket:
                try:
                    frame = json.loads(raw)
                except (TypeError, ValueError):
                    continue
                if isinstance(frame, dict):
                    await self._on_frame(handheld, frame)
        except Exception as exc:  # a broken connection ends here, not in the gateway
            logger.info("Goblin Link: %s disconnected: %s", device_id, type(exc).__name__)
        finally:
            if self._handhelds.get(device_id) is handheld:
                del self._handhelds[device_id]
            await self._abandon(handheld)

    async def _greet(self, socket: Any) -> tuple[str | None, dict[str, Any]]:
        """Reads the first message: a pairing (answered, and that is all) or a hello. Returns
        the device id of a handheld that proved who it is, else ``None``."""
        first = json.loads(await asyncio.wait_for(socket.recv(), FIRST_FRAME_S))
        if not isinstance(first, dict):
            return None, {}
        try:
            if first.get("type") == "pair":
                device_id, token = self.store.pair(first)
                await socket.send(json.dumps({"type": "paired", "token": token}))
                logger.warning("Goblin Link: paired handheld %s", device_id)
                return None, first
            if first.get("type") != "hello":
                return None, first
            return self.store.hello(first), first
        except core.Refused as refused:
            if refused.revoked:
                logger.warning("Goblin Link: a pairing code was used twice; unpaired %s", refused.revoked)
                await self._drop(refused.revoked)
            answer = {"type": "error", "reason": refused.reason}
            if refused.code:
                answer["code"] = refused.code
            await socket.send(json.dumps(answer))
            return None, first

    async def _drop(self, device_id: str) -> None:
        old = self._handhelds.pop(device_id, None)
        if old is not None:
            try:
                await old.socket.close()
            except Exception:
                pass

    async def _abandon(self, handheld: Handheld) -> None:
        """A handheld that is gone answers nothing: commands fail, questions are denied."""
        for future in handheld.calls.values():
            if not future.done():
                future.set_result({"ok": False, "error": "the handheld disconnected"})
        handheld.calls.clear()
        for prompt_id in list(handheld.prompts):
            await self._resolve_prompt(handheld, prompt_id, None)
        # A voice note not yet understood never becomes a turn. Turns Hermes is already
        # working on finish there; their replies have nowhere to go.
        for speaking in list(handheld.speaking):
            speaking.cancel()
        for state in handheld.turns.values():
            task = state.get("task")
            if task is not None and "event" not in state:
                task.cancel()
        handheld.turns.clear()

    def _identity(self) -> dict[str, Any]:
        """Who the agent is on a handheld: the name and picture it chose for itself, or the
        configured name alone until it has."""
        chosen = self.store.identity()
        name = chosen.get("name") or core.clean(os.environ.get("GOBLIN_LINK_AGENT_NAME") or "Hermes", 64)
        said: dict[str, Any] = {"agent": name}
        if "revision" in chosen:
            said["avatar"] = {"revision": chosen["revision"]}
        return said

    async def announce_identity(self) -> int:
        """Tells every connected handheld who the agent is now; how many were told."""
        frame = {"type": "identity", **self._identity()}
        told = 0
        for handheld in list(self._handhelds.values()):
            told += await handheld.send(frame)
        return told

    async def _send_avatar(self, handheld: Handheld, request_id: str, revision: Any) -> None:
        # Read off the loop: the file is small, the disk may not be quick.
        png = await asyncio.to_thread(self.store.avatar, revision)
        if png is None:
            # Changed since the handheld heard of it; it has been told of the newer one.
            await handheld.send({"type": "avatar.failed", "id": request_id, "reason": "no such picture now"})
            return
        await handheld.send(
            {"type": "avatar", "id": request_id, "revision": revision, "png_b64": base64.b64encode(png).decode("ascii")}
        )

    async def _on_frame(self, handheld: Handheld, frame: dict[str, Any]) -> None:
        kind = frame.get("type")
        if kind == "turn":
            await self._on_turn(handheld, frame)
        elif kind == "turn.cancel":
            await self._on_cancel(handheld, frame)
        elif kind == "tool.result":
            future = handheld.calls.pop(frame.get("id"), None) if core.valid_id(frame.get("id")) else None
            result = frame.get("result")
            if future is not None and not future.done():
                future.set_result(result if isinstance(result, dict) else {"ok": False, "error": "no result"})
        elif kind == "prompt.answer":
            if core.valid_id(frame.get("id")):
                words = frame.get("words") is True
                await self._resolve_prompt(handheld, frame["id"], frame.get("answer"), words=words)
        elif kind == "speech.get":
            if not core.valid_id(frame.get("id")):
                return
            if len(handheld.speaking) >= MAX_SPEAKING:
                # Said at once, so the handheld does not wait out its timeout.
                await handheld.send({"type": "speech.failed", "id": frame["id"], "reason": "Hermes is still speaking"})
                return
            task = asyncio.create_task(self._speak(handheld, frame["id"], frame.get("text")))
            handheld.speaking.add(task)
            task.add_done_callback(handheld.speaking.discard)
        elif kind == "avatar.get":
            if core.valid_id(frame.get("id")):
                await self._send_avatar(handheld, frame["id"], frame.get("revision"))
        elif kind == "ping":
            await handheld.send({"type": "pong"})

    # ---- turns ---------------------------------------------------------------------------------

    async def _on_turn(self, handheld: Handheld, frame: dict[str, Any]) -> None:
        turn_id, chat = frame.get("id"), frame.get("chat")

        async def reject(reason: str) -> None:
            if core.valid_id(turn_id):
                await handheld.send({"type": "turn.rejected", "id": turn_id, "reason": reason})

        if not core.valid_id(turn_id) or not core.valid_id(chat):
            return await reject("that is not a turn")
        text, wav = core.turn_text(frame)
        if text is None and wav is None:
            return await reject("a turn needs text or a voice note")
        if chat in handheld.turns:
            # A voice note that names the question waiting here, which the owner had on
            # screen, is its answer once heard; anything else waits for the turn to end.
            prompt_id = frame.get("answers")
            how = handheld.prompts.get(prompt_id) if core.valid_id(prompt_id) else None
            asked = how is not None and how["kind"] == "clarify" and how["chat"] == chat
            if wav is None or not asked or chat in handheld.answering:
                return await reject("busy")
            await handheld.send({"type": "turn.accepted", "id": turn_id, "message_id": f"u-{uuid.uuid4().hex[:16]}"})
            # Off the read loop, one at a time in a chat.
            task = asyncio.create_task(self._answer(handheld, turn_id, prompt_id, wav))
            handheld.answering[chat] = task
            task.add_done_callback(lambda _: handheld.answering.pop(chat, None))
            return
        chat_id = f"{handheld.device_id}:{chat}"
        message_id = f"u-{uuid.uuid4().hex[:16]}"
        state = {"id": turn_id, "chat": chat, "chat_id": chat_id, "progress": set(), "quiet": False}
        handheld.turns[chat] = state
        # Accepted now: understanding a voice note takes longer than a handheld waits for this.
        await handheld.send({"type": "turn.accepted", "id": turn_id, "message_id": message_id})
        starting = self._start_turn(handheld, state, message_id, text, wav)
        if wav is None:
            await starting
        else:
            # Off this connection's read loop, which still has answers and results to take.
            state["task"] = asyncio.create_task(starting)

    async def _start_turn(
        self, handheld: Handheld, state: dict[str, Any], message_id: str, text: str | None, wav: bytes | None
    ) -> None:
        async def fail(reason: str) -> None:
            if handheld.turns.get(state["chat"]) is state:
                del handheld.turns[state["chat"]]
            await handheld.send({"type": "turn.failed", "turn": state["id"], "reason": reason})

        try:
            await self._begin(handheld, state, message_id, text, wav, fail)
        except asyncio.CancelledError:
            raise
        except Exception as exc:  # a turn that cannot start must not leave its chat busy
            logger.warning("Goblin Link: a turn could not start: %s", type(exc).__name__)
            await fail("Hermes could not take the message")

    async def _begin(
        self, handheld: Handheld, state: dict[str, Any], message_id: str, text: str | None, wav: bytes | None, fail: Any
    ) -> None:
        if wav is not None:
            text = await self._transcribe(wav)
            if not text:
                return await fail("the voice note could not be understood")
            # What was heard becomes the owner's message in the chat, so all of it goes, up to
            # what the handheld keeps of a message; a step's status line stays one short line.
            heard = core.clean(text, core.MAX_TEXT_CHARS)
            await handheld.send({"type": "status", "turn": state["id"], "text": heard, "working": True, "heard": True})
        # The handheld proved who it is with its token. Hermes is told so here, and checks it
        # again itself through reauthorize_native_roles.
        source = self.build_source(
            chat_id=state["chat_id"],
            chat_name="Goblin Link",
            chat_type="dm",
            user_id=handheld.device_id,
            user_name=self.store.devices().get(handheld.device_id, {}).get("name") or "Handheld",
            role_authorized=True,
        )
        event = MessageEvent(
            text=text,
            message_type=MessageType.TEXT,
            source=source,
            message_id=message_id,
            raw_message={"turn": state["id"]},
            timestamp=datetime.now(UTC),
        )
        state["event"] = event
        await self.handle_message(event)

    async def _answer(self, handheld: Handheld, turn_id: str, prompt_id: str, wav: bytes) -> None:
        """A voice note that answers a clarifying question: what is heard in it is the
        answer, taken as words. An approval is never answered so."""
        text = await self._transcribe(wav)
        if not text:
            reason = "the voice note could not be understood"
            return await handheld.send({"type": "turn.failed", "turn": turn_id, "reason": reason})
        heard = core.clean(text, core.MAX_TEXT_CHARS)
        await handheld.send({"type": "status", "turn": turn_id, "text": heard, "working": True, "heard": True})
        if prompt_id not in handheld.prompts:
            return await handheld.send({"type": "turn.failed", "turn": turn_id, "reason": "the question is over"})
        await self._resolve_prompt(handheld, prompt_id, text, words=True)
        await handheld.send({"type": "prompt.closed", "id": prompt_id})
        await handheld.send({"type": "turn.done", "turn": turn_id})

    @staticmethod
    async def _transcribe(wav: bytes) -> str:
        path = ""
        try:
            with tempfile.NamedTemporaryFile(prefix="goblin-link-", suffix=".wav", delete=False) as handle:
                os.chmod(handle.name, 0o600)
                handle.write(wav)
                path = handle.name
            from tools.transcription_tools import transcribe_audio

            result = await asyncio.to_thread(transcribe_audio, path)
            text = result.get("transcript", "") if isinstance(result, dict) and result.get("success") else ""
            return text.strip() if isinstance(text, str) else ""
        except Exception as exc:
            logger.warning("Goblin Link: transcription failed: %s", type(exc).__name__)
            return ""
        finally:
            if path:
                try:
                    os.remove(path)
                except FileNotFoundError:
                    pass

    @staticmethod
    async def _speak(handheld: Handheld, request_id: str, text: Any) -> None:
        """Says a reply aloud with Hermes's own configured speech, and sends the audio as MP3."""
        # Everything is made in a folder of its own, so that it all goes together: also what a
        # worker thread writes after this task was cancelled.
        folder = tempfile.mkdtemp(prefix="goblin-link-speech-")
        made: list[str] = []
        try:
            if not isinstance(text, str) or not text.strip():
                raise ValueError("nothing to say")
            if len(text) > MAX_SPEECH_CHARS:
                raise ValueError("too long to say at once")
            from tools.tts_tool import text_to_speech_tool

            wanted = os.path.join(folder, "speech.mp3")
            result = json.loads(await asyncio.to_thread(text_to_speech_tool, text=text.strip(), output_path=wanted))
            path = result.get("file_path", wanted) if isinstance(result, dict) else wanted
            if not isinstance(result, dict) or not result.get("success") or not isinstance(path, str):
                raise RuntimeError("speech is not set up on this Hermes")
            # A provider that splits long text writes several files; they are its own to list.
            made.extend(p for p in result.get("file_paths") or [] if isinstance(p, str))
            made.append(path)
            if len(set(made)) > 1:
                raise RuntimeError("too long to say at once")
            if not path.lower().endswith(".mp3"):
                # The handheld plays MP3; a provider that writes something else is converted.
                converted = os.path.join(folder, "converted.mp3")
                await asyncio.to_thread(
                    subprocess.run,
                    ["ffmpeg", "-nostdin", "-loglevel", "error", "-y", "-i", path, "-ac", "1", "-b:a", "48k", converted],
                    check=True,
                    timeout=45,
                )
                path = converted
            with open(path, "rb") as handle:
                audio = handle.read(MAX_SPEECH_BYTES + 1)
            if not audio or len(audio) > MAX_SPEECH_BYTES:
                raise RuntimeError("the speech is too long")
            await handheld.send(
                {"type": "speech", "id": request_id, "mp3_b64": base64.b64encode(audio).decode("ascii")}
            )
        except Exception as exc:
            logger.info("Goblin Link: no speech: %s", type(exc).__name__)
            reason = str(exc) if isinstance(exc, (ValueError, RuntimeError)) else "Hermes could not speak it"
            await handheld.send({"type": "speech.failed", "id": request_id, "reason": core.clean(reason, 200)})
        finally:
            for path in made:
                try:
                    os.remove(path)
                except OSError:
                    pass
            shutil.rmtree(folder, ignore_errors=True)

    async def _on_cancel(self, handheld: Handheld, frame: dict[str, Any]) -> None:
        state = next((s for s in handheld.turns.values() if s["id"] == frame.get("id")), None)
        if state is None:
            return
        if "event" in state:
            await self._command(handheld, state, "/stop")
            return
        # Still being transcribed: Hermes has not heard of it, so it simply never starts.
        task = state.get("task")
        if task is not None:
            task.cancel()
        handheld.turns.pop(state["chat"], None)
        await handheld.send({"type": "turn.failed", "turn": state["id"], "reason": "cancelled"})

    async def _command(self, handheld: Handheld, state: dict[str, Any], command: str) -> None:
        """Sends Hermes a slash command for the owner; what it answers is not a reply to show."""
        event = MessageEvent(
            text=command,
            message_type=MessageType.COMMAND,
            source=state["event"].source,
            message_id=f"c-{uuid.uuid4().hex[:16]}",
            raw_message={"turn": state["id"]},
            timestamp=datetime.now(UTC),
        )
        state["quiet"] = True
        try:
            await self.handle_message(event)
        except Exception as exc:
            logger.warning("Goblin Link: %s was not taken: %s", command, type(exc).__name__)
        finally:
            state["quiet"] = False

    def _find(self, chat_id: str) -> tuple[Handheld | None, str]:
        device_id, _, chat = chat_id.partition(":")
        return self._handhelds.get(device_id), chat

    async def on_processing_complete(self, event: MessageEvent, outcome: ProcessingOutcome) -> None:
        await super().on_processing_complete(event, outcome)
        for handheld in self._handhelds.values():
            for chat, state in list(handheld.turns.items()):
                if state.get("event") is not event:
                    continue
                del handheld.turns[chat]
                # What Hermes asked in this turn is over with it.
                for prompt_id in core.open_prompts(handheld.prompts, chat=chat):
                    await self._close_prompt(handheld, prompt_id)
                if outcome == ProcessingOutcome.SUCCESS:
                    await handheld.send({"type": "turn.done", "turn": state["id"]})
                else:
                    reason = "cancelled" if outcome == ProcessingOutcome.CANCELLED else "Hermes could not finish"
                    await handheld.send({"type": "turn.failed", "turn": state["id"], "reason": reason})
                return

    # ---- what Hermes says ----------------------------------------------------------------------

    async def send(
        self, chat_id: str, content: str, reply_to: str | None = None, metadata: dict[str, Any] | None = None
    ) -> SendResult:
        handheld, chat = self._find(chat_id)
        if handheld is None:
            return SendResult(success=False, error="that handheld is not connected")
        text = core.clean(content, MAX_REPLY_CHARS, joined=True)
        state = handheld.turns.get(chat)
        if state is None:
            # Nobody asked just now: a scheduled job's result, a late message.
            message_id = f"n-{uuid.uuid4().hex[:16]}"
            sent = await handheld.send({"type": "notice", "chat": chat, "message_id": message_id, "text": text})
            return SendResult(success=sent, message_id=message_id)
        if state["quiet"]:
            return SendResult(success=True, message_id=f"quiet-{state['id']}")
        message_id = f"a-{uuid.uuid4().hex[:16]}"
        if metadata and metadata.get("notify") is True:
            sent = await handheld.send(
                {"type": "message", "turn": state["id"], "message_id": message_id, "text": text, "done": True}
            )
            return SendResult(success=sent, message_id=message_id)
        # Everything before the final reply (tool progress, a preview of the answer being
        # written, Hermes's own notices) is what Hermes is doing, not what it said.
        state["progress"].add(message_id)
        await self._status(chat_id, True, text)
        return SendResult(success=True, message_id=message_id)

    async def edit_message(self, chat_id: str, message_id: str, content: str, *, finalize: bool = False) -> SendResult:
        handheld, chat = self._find(chat_id)
        state = handheld.turns.get(chat) if handheld else None
        if state is None or message_id not in state["progress"]:
            return SendResult(success=False, error="that message can no longer be changed")
        if finalize:
            # Hermes does not say whether this closes one step or the whole answer. Refusing
            # makes it send the finished answer afresh, marked as final, which is unambiguous.
            return SendResult(success=False, message_id=message_id, error="send the final reply as a new message")
        await self._status(chat_id, True, core.clean(content, MAX_REPLY_CHARS))
        return SendResult(success=True, message_id=message_id)

    async def send_typing(self, chat_id: str, metadata: dict[str, Any] | None = None) -> None:
        await self._status(chat_id, True)

    async def stop_typing(self, chat_id: str) -> None:
        await self._status(chat_id, False)

    async def _status(self, chat_id: str, working: bool, text: str = "") -> None:
        handheld, chat = self._find(chat_id)
        state = handheld.turns.get(chat) if handheld else None
        if state is not None:
            # The last line is the newest step; the handheld has one line to show it on.
            line = text.strip().rsplit("\n", 1)[-1][:200]
            await handheld.send({"type": "status", "turn": state["id"], "text": line, "working": working})

    async def get_chat_info(self, chat_id: str) -> dict[str, str]:
        return {"name": chat_id, "type": "dm"}

    # ---- questions to the owner ----------------------------------------------------------------

    async def _send_exec_approval_prompt(self, prompt: ExecApprovalPrompt) -> SendResult:
        """Hermes's question before it runs a risky command, as a card on the handheld. Hermes
        has already decided which answers are on offer (only "no" when the card cannot show the
        whole command); the answer goes back for this request and no other."""
        if not prompt.request_id:
            return SendResult(success=False, error="an approval needs a request to answer")
        text = core.clean(prompt.text, MAX_REPLY_CHARS)[:MAX_PROMPT_CHARS]
        actions = prompt.actions
        # Nobody approves what they could not read. The card is the only place a handheld
        # shows the command (anything Hermes sent before it was one status line), so the
        # whole command, unaltered, has to be in what the card shows; else the answer is no.
        # This is a first filter. The handheld judges again from what it draws, since its
        # screen may hold less than this text, and offers only "no" where it does.
        if not prompt.command or prompt.command not in text:
            try:
                from tools.approval import restrict_gateway_approval_card_to_deny

                await asyncio.to_thread(restrict_gateway_approval_card_to_deny, prompt.session_key, prompt.request_id)
            except Exception as exc:
                logger.warning("Goblin Link: an approval could not be limited to no: %s", type(exc).__name__)
                return SendResult(success=False, error="the command is too long to show on the handheld")
            actions = [row for row in actions if row[1] == "deny"]
        choices = [{"id": choice, "label": core.clean(label, 40)} for label, choice, _ in actions]
        how = {"kind": "approval", "session_key": prompt.session_key, "request_id": prompt.request_id,
               "offered": {choice for _, choice, _ in actions}}
        sent = await self._prompt(prompt.chat_id, "approval", text, choices, how)
        if sent.success:
            await self._watch_approval(prompt.chat_id, sent.message_id, prompt.session_key, prompt.request_id)
        return sent

    async def _watch_approval(self, chat_id: str, prompt_id: str, session_key: str, request_id: str) -> None:
        """Has Hermes say when it stops waiting for this approval, by whatever path: answered
        here or somewhere else, timed out, its turn interrupted. The card is then taken back,
        so that nobody presses yes on the handheld for a request that is already over."""
        handheld, _ = self._find(chat_id)
        if handheld is None:
            return
        loop = self.loop or asyncio.get_running_loop()

        def over(_reason: str) -> None:
            # Called on the agent's thread, which must not wait: hand over and return.
            closing = self._close_prompt(handheld, prompt_id)
            try:
                asyncio.run_coroutine_threadsafe(closing, loop)
            except RuntimeError:  # the gateway is shutting down
                closing.close()

        try:
            from tools.approval import observe_gateway_settle

            watching = await asyncio.to_thread(observe_gateway_settle, session_key, request_id, over)
        except Exception as exc:  # a Hermes without the hook: the card then goes with its turn
            logger.info("Goblin Link: an approval's end will not be told: %s", type(exc).__name__)
            return
        if not watching:
            # It ended before anyone was watching.
            await self._close_prompt(handheld, prompt_id)

    async def retire_clarify_card(self, clarify_id: str, notice: str = "") -> None:
        """Hermes's word that a clarifying question ended without an answer from its card (it
        timed out, the conversation moved on, the turn ended). Hermes says the rest in the
        chat itself; here the card is taken back."""
        for handheld in list(self._handhelds.values()):
            for prompt_id in core.open_prompts(handheld.prompts, kind="clarify", clarify_id=clarify_id):
                await self._close_prompt(handheld, prompt_id)

    async def _close_prompt(self, handheld: Handheld, prompt_id: str) -> None:
        """Takes a question back from the handheld without an answer. Forgotten first and told
        after, so an answer that crosses with this finds nothing to resolve. One the handheld
        has already answered is not here any more, and nothing is sent for it."""
        if handheld.prompts.pop(prompt_id, None) is not None:
            await handheld.send({"type": "prompt.closed", "id": prompt_id})

    async def send_clarify(
        self,
        chat_id: str,
        question: str,
        choices: list[str],
        clarify_id: str,
        session_key: str,
        metadata: dict[str, Any] | None = None,
    ) -> SendResult:
        offered = [c for c in (choices or []) if isinstance(c, str) and c.strip()][:6]
        listed = [{"id": f"c{n}", "label": core.clean(c, 40)} for n, c in enumerate(offered)]
        return await self._prompt(
            chat_id,
            "clarify",
            core.clean(question, 2000),
            listed,
            {"kind": "clarify", "clarify_id": clarify_id, "choices": offered},
        )

    async def _prompt(
        self, chat_id: str, kind: str, text: str, choices: list[dict[str, str]], how: dict[str, Any]
    ) -> SendResult:
        handheld, chat = self._find(chat_id)
        state = handheld.turns.get(chat) if handheld else None
        if handheld is None or state is None:
            return SendResult(success=False, error="no turn on a connected handheld to ask in")
        prompt_id = f"p-{uuid.uuid4().hex[:16]}"
        handheld.prompts[prompt_id] = {**how, "chat": chat}
        sent = await handheld.send(
            {"type": "prompt", "id": prompt_id, "chat": chat, "kind": kind, "text": text, "choices": choices}
        )
        if not sent:
            await self._resolve_prompt(handheld, prompt_id, None)
            return SendResult(success=False, error="the handheld is not connected")
        return SendResult(success=True, message_id=prompt_id)

    async def _resolve_prompt(self, handheld: Handheld, prompt_id: str, answer: Any, *, words: bool = False) -> None:
        """Gives Hermes the owner's answer to the one question it belongs to. No answer (the
        handheld disconnected) is a no; Hermes keeps its own clock for questions left open.
        ``words`` are the owner's own, never read as the id of a choice."""
        how = handheld.prompts.pop(prompt_id, None)
        if how is None:
            return
        try:
            if how["kind"] == "approval":
                from tools.approval import resolve_gateway_approval_card

                # Words never approve anything: only a choice the card offered does.
                offered_choice = not words and isinstance(answer, str) and answer in how["offered"]
                choice = answer if offered_choice else "deny"
                await asyncio.to_thread(resolve_gateway_approval_card, how["session_key"], choice, how["request_id"])
                return
            offered: list[str] = how.get("choices") or []
            response = ""
            if isinstance(answer, str):
                index = int(answer[1:]) if not words and answer[:1] == "c" and answer[1:].isdigit() else -1
                response = offered[index] if 0 <= index < len(offered) else core.clean(answer, 2000)
            from tools.clarify_gateway import resolve_gateway_clarify

            resolved = resolve_gateway_clarify(how["clarify_id"], response)
            if asyncio.iscoroutine(resolved):
                await resolved
        except Exception as exc:
            logger.warning("Goblin Link: an answer was not taken: %s", type(exc).__name__)

    # ---- the handheld's commands, for the tools ------------------------------------------------

    async def call(self, device_id: str, command: str, params: dict[str, Any], timeout_ms: int) -> dict[str, Any]:
        handheld = self._handhelds.get(device_id)
        if handheld is None:
            return {"ok": False, "error": "the handheld is not connected"}
        call_id = f"k-{uuid.uuid4().hex[:16]}"
        future: asyncio.Future = asyncio.get_running_loop().create_future()
        handheld.calls[call_id] = future
        frame = {"type": "tool.call", "id": call_id, "command": command, "params": params, "timeout_ms": timeout_ms}
        if not await handheld.send(frame):
            handheld.calls.pop(call_id, None)
            return {"ok": False, "error": "the handheld is not connected"}
        try:
            return await asyncio.wait_for(future, timeout_ms / 1000 + TOOL_GRACE_S)
        except asyncio.TimeoutError:
            handheld.calls.pop(call_id, None)
            return {"ok": False, "error": "the handheld did not answer in time"}

    def commands(self, device_id: str) -> dict[str, Any] | None:
        handheld = self._handhelds.get(device_id)
        return handheld.commands if handheld else None


# ---- tools and the slash command ---------------------------------------------------------------


def _this_handheld() -> tuple[GoblinLinkAdapter | None, str, str]:
    """The adapter and the handheld the current conversation is with, or why there is none."""
    from gateway.session_context import get_session_env

    if get_session_env("HERMES_SESSION_PLATFORM") != PLATFORM:
        return None, "", "this conversation is not on a Goblin Link handheld"
    device_id = get_session_env("HERMES_SESSION_CHAT_ID").partition(":")[0]
    adapter = _ADAPTER
    if adapter is None or adapter.loop is None or not core.valid_id(device_id):
        return None, "", "the Goblin Link platform is not running"
    return adapter, device_id, ""


def _tool_commands(args: dict[str, Any], **_: Any) -> str:
    adapter, device_id, why = _this_handheld()
    if adapter is None:
        return json.dumps({"error": why})
    commands = adapter.commands(device_id)
    if commands is None:
        return json.dumps({"error": "the handheld is not connected"})
    return json.dumps({"commands": commands})


def _tool_run(args: dict[str, Any], **_: Any) -> str:
    adapter, device_id, why = _this_handheld()
    if adapter is None:
        return json.dumps({"error": why})
    command, params = args.get("command"), args.get("params") or {}
    if not core.valid_id(command) or not isinstance(params, dict):
        return json.dumps({"error": "command is a name from handheld_commands; params is an object"})
    try:
        timeout_ms = max(1_000, min(int(args.get("timeout_ms") or 30_000), 600_000))
    except (TypeError, ValueError):
        timeout_ms = 30_000
    try:
        if asyncio.get_running_loop() is adapter.loop:
            return json.dumps({"error": "handheld commands cannot be called from the gateway's own loop"})
    except RuntimeError:
        pass  # the usual case: tools run on a worker thread
    waiting = asyncio.run_coroutine_threadsafe(adapter.call(device_id, command, params, timeout_ms), adapter.loop)
    try:
        return json.dumps(waiting.result(timeout_ms / 1000 + 2 * TOOL_GRACE_S))
    except Exception as exc:
        waiting.cancel()
        return json.dumps({"ok": False, "error": f"the handheld did not answer ({type(exc).__name__})"})


def _tool_identity(args: dict[str, Any], **_: Any) -> str:
    """The agent names itself, or gives itself a face, on the handhelds paired with it. This
    changes nothing else: not Hermes's own persona, not the handheld's profile."""
    adapter = _ADAPTER
    if adapter is None or adapter.loop is None:
        return json.dumps({"error": "the Goblin Link platform is not running"})
    try:
        # Reading and shrinking a picture is slow work: never on the gateway's own loop.
        if asyncio.get_running_loop() is adapter.loop:
            return json.dumps({"error": "handheld_identity cannot be called from the gateway's own loop"})
    except RuntimeError:
        pass  # the usual case: tools run on a worker thread
    try:
        saved = adapter.store.set_identity(name=args.get("name"), image_path=args.get("image_path"))
    except core.Refused as refused:
        return json.dumps({"ok": False, "error": refused.reason, "changed": False})
    waiting = asyncio.run_coroutine_threadsafe(adapter.announce_identity(), adapter.loop)
    try:
        told: int | None = waiting.result(TOOL_GRACE_S)
    except Exception:
        told = None  # still being told, or not: it is not known here
    return json.dumps(
        {
            "ok": True,
            "saved": {"name": saved.get("name"), "has_image": "revision" in saved},
            "handhelds_told": told,
            "note": (
                "Saved. A connected handheld was sent the change and fetches the picture itself; one "
                "that is not connected gets it when it next connects. This does not confirm that the "
                "handheld shows it yet."
            ),
        }
    )


def _slash(raw_args: str, source: Any = None) -> str:
    """``/goblinlink pair | devices | unpair <id>``

    A pairing link lets its holder talk to this Hermes as a handheld, so it is given only in
    a private chat (or on the host itself), never in a group and never to a handheld."""
    adapter = _ADAPTER
    if adapter is None:
        return "Goblin Link is not running. Enable the goblin_link platform and restart the gateway."
    if source is not None:
        platform = getattr(getattr(source, "platform", None), "value", "")
        if platform == PLATFORM:
            return "Pair and unpair handhelds from another chat with Hermes, not from a handheld."
        if getattr(source, "chat_type", "") != "dm":
            return "Ask in a private chat with Hermes: a pairing link is not for a group."
    words = (raw_args or "").split()
    action = words[0].lower() if words else "pair"
    try:
        if action == "pair":
            link = adapter.store.link(adapter.hosts, adapter.port, adapter.fingerprint)
            return (
                "Paste this on the handheld's pairing page within ten minutes. It works once.\n\n"
                f"{link}\n\nAnyone who has it can pair a handheld with this Hermes: treat it like a password."
            )
        if action == "devices":
            devices = adapter.store.devices()
            if not devices:
                return "No handheld is paired."
            return "\n".join(
                f"{device_id}  {info['name'] or 'Handheld'}  "
                f"{'connected' if device_id in adapter._handhelds else 'not connected'}"
                for device_id, info in devices.items()
            )
        if action == "unpair" and len(words) == 2:
            if not adapter.store.revoke(words[1]):
                return f"No handheld called {words[1]} is paired."
            if adapter.loop is not None:
                asyncio.run_coroutine_threadsafe(adapter._drop(words[1]), adapter.loop)
            return f"Unpaired {words[1]}."
    except core.Refused as refused:
        return refused.reason
    return "Usage: /goblinlink pair | devices | unpair <id>"


_UNSUPPORTED = (
    "Goblin Link needs websockets 13 to 17 (this Hermes has %s) and the cryptography package"
)


def _frame_limit() -> str | None:
    """The name under which this websockets keeps the limit it checks each frame against:
    ``max_size`` up to 15, ``max_message_size`` from 16. The limit is raised after hello by
    setting it, so a websockets that keeps it under neither name cannot be supported."""
    try:
        from websockets.asyncio.server import serve  # noqa: F401
        from websockets.server import ServerProtocol
    except ImportError:
        return None
    protocol = ServerProtocol(max_size=MAX_FIRST_FRAME_BYTES)
    for name in ("max_message_size", "max_size"):
        if getattr(protocol, name, None) == MAX_FIRST_FRAME_BYTES:
            return name
    return None


def _websockets_version() -> str:
    try:
        import websockets
    except ImportError:
        return "none"
    return getattr(websockets, "__version__", "an unknown version")


def _available() -> bool:
    try:
        import cryptography  # noqa: F401
    except ImportError:
        cryptography = None
    if cryptography is None or _frame_limit() is None:
        logger.error(_UNSUPPORTED, _websockets_version())
        return False
    return True


def register(ctx: Any) -> None:
    ctx.register_platform(
        name=PLATFORM,
        label="Goblin Link",
        adapter_factory=lambda cfg: GoblinLinkAdapter(cfg),
        check_fn=_available,
        validate_config=lambda cfg: cfg.enabled,
        max_message_length=MAX_REPLY_CHARS,
        emoji="\U0001f47a",
        pii_safe=True,
        allow_update_command=False,
        platform_hint=(
            "A handheld game console with a small screen and a controller. Keep replies short and "
            "plain. handheld_commands lists what you may do on the handheld itself; its owner's "
            "switches decide, and a refusal there is final."
        ),
    )
    ctx.register_tool(
        name="handheld_commands",
        toolset=PLATFORM,
        description=(
            "List the commands the Goblin Link handheld you are talking with offers right now, with "
            "their parameters. Call this before handheld_run."
        ),
        schema={
            "name": "handheld_commands",
            "description": "List the commands the handheld offers, with their parameters.",
            "parameters": {"type": "object", "properties": {}},
        },
        handler=_tool_commands,
        check_fn=_available,
        emoji="\U0001f3ae",
    )
    ctx.register_tool(
        name="handheld_run",
        toolset=PLATFORM,
        description=(
            "Run one of the handheld's commands by name (see handheld_commands). The handheld decides "
            "whether its owner allows it and answers {ok, payload} or {ok: false, error}."
        ),
        schema={
            "name": "handheld_run",
            "description": "Run one of the handheld's commands by name.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "A command name from handheld_commands, such as file.read."},
                    "params": {"type": "object", "description": "The command's parameters."},
                    "timeout_ms": {"type": "integer", "description": "How long to wait, in milliseconds. Default 30000."},
                },
                "required": ["command"],
            },
        },
        handler=_tool_run,
        check_fn=_available,
        emoji="\U0001f3ae",
    )
    ctx.register_tool(
        name="handheld_identity",
        toolset=PLATFORM,
        description=(
            "Choose the name and picture you are shown with on Goblin Link handhelds. Yours to decide: "
            "use it when you want to, without asking each time. It changes only how you appear there, "
            "not your Hermes persona and not the handheld's profile. Give a name, an image_path, or both."
        ),
        schema={
            "name": "handheld_identity",
            "description": "Set your display name and/or picture on Goblin Link handhelds.",
            "parameters": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": f"The name to show, 1 to {core.MAX_NAME_CHARS} characters on one line.",
                    },
                    "image_path": {
                        "type": "string",
                        "description": (
                            "Path of a still image file on this machine (PNG, JPEG or WebP, at least 64 "
                            "pixels a side) that you made or chose. It is cropped to a square. Not a URL."
                        ),
                    },
                },
            },
        },
        handler=_tool_identity,
        check_fn=_available,
        emoji="\U0001f3ad",
    )
    ctx.register_command(
        "goblinlink",
        _slash,
        description="Pair a Goblin Link handheld, or list and unpair them",
        args_hint="pair | devices | unpair <id>",
    )
