"""The Goblin Link platform: handhelds connect to this plugin over a WebSocket with TLS and
become Hermes chats. See ``docs/hermes-protocol.md`` in the Goblin Link repository.

What a handheld may do is decided in two places and this file is neither: Hermes decides what
its agent does, and the handheld's own daemon decides which of its commands may run. This
adapter authenticates handhelds (``core.Store``) and carries messages.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import ssl
import tempfile
import uuid
from datetime import UTC, datetime
from typing import Any

from gateway.config import Platform, PlatformConfig
from gateway.platforms.base import (
    BasePlatformAdapter,
    MessageEvent,
    MessageType,
    ProcessingOutcome,
    SendResult,
)

from . import core

logger = logging.getLogger(__name__)

PLATFORM = "goblin_link"
MAX_FRAME_BYTES = 16 << 20  # a voice note of up to ten minutes, base64
FIRST_FRAME_S = 10
PROMPT_S = 300
TOOL_GRACE_S = 5
MAX_REPLY_CHARS = 48_000

# The adapter Hermes is running, for the tools and the slash command (one per process).
_ADAPTER: "GoblinLinkAdapter | None" = None


class Handheld:
    """One connected, authenticated handheld."""

    def __init__(self, device_id: str, socket: Any, commands: Any):
        self.device_id, self.socket = device_id, socket
        self.commands = commands if isinstance(commands, dict) else {}
        self.lock = asyncio.Lock()
        # chat -> the turn being answered there
        self.turns: dict[str, dict[str, Any]] = {}
        # call id -> who waits for the command's result
        self.calls: dict[str, asyncio.Future] = {}
        # prompt id -> how to resolve it
        self.prompts: dict[str, dict[str, Any]] = {}

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

    # ---- lifecycle -----------------------------------------------------------------------------

    async def connect(self, *, is_reconnect: bool = False) -> bool:
        global _ADAPTER
        from websockets.asyncio.server import serve

        try:
            cert, key, self.fingerprint = self.store.certificate(self.hosts)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            context.load_cert_chain(cert, key)
            self._server = await serve(
                self._serve, self.bind, self.port, ssl=context, max_size=MAX_FRAME_BYTES
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

    # ---- one handheld's connection -------------------------------------------------------------

    async def _serve(self, socket: Any) -> None:
        if getattr(getattr(socket, "request", None), "path", "") != "/goblin-link":
            return
        try:
            first = json.loads(await asyncio.wait_for(socket.recv(), FIRST_FRAME_S))
        except Exception:
            return
        if not isinstance(first, dict):
            return
        try:
            if first.get("type") == "pair":
                device_id, token = self.store.pair(first)
                await socket.send(json.dumps({"type": "paired", "token": token}))
                logger.warning("Goblin Link: paired handheld %s", device_id)
                return
            if first.get("type") != "hello":
                return
            device_id = self.store.hello(first)
        except core.Refused as refused:
            if refused.revoked:
                logger.warning("Goblin Link: a pairing code was used twice; unpaired %s", refused.revoked)
                await self._drop(refused.revoked)
            answer = {"type": "error", "reason": refused.reason}
            if refused.code:
                answer["code"] = refused.code
            await socket.send(json.dumps(answer))
            return
        handheld = Handheld(device_id, socket, first.get("commands"))
        # A handheld connects once: a new connection replaces the old one.
        await self._drop(device_id)
        self._handhelds[device_id] = handheld
        await handheld.send({"type": "hello.ok", "agent": self._agent_name()})
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
        handheld.turns.clear()

    @staticmethod
    def _agent_name() -> str:
        return core.clean(os.environ.get("GOBLIN_LINK_AGENT_NAME") or "Hermes", 64)

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
                await self._resolve_prompt(handheld, frame["id"], frame.get("answer"))
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
        if chat in handheld.turns:
            return await reject("busy")
        text, wav = core.turn_text(frame)
        if text is None and wav is None:
            return await reject("a turn needs text or a voice note")
        chat_id = f"{handheld.device_id}:{chat}"
        state = {"id": turn_id, "chat": chat, "chat_id": chat_id, "open": {}, "next": 0, "quiet": False}
        handheld.turns[chat] = state
        if wav is not None:
            text = await self._transcribe(wav)
            if not text:
                handheld.turns.pop(chat, None)
                return await reject("the voice note could not be understood")
        # The handheld proved who it is with its token; Hermes is told so, and needs no
        # allowlist of its own for this platform.
        source = self.build_source(
            chat_id=chat_id,
            chat_name="Goblin Link",
            chat_type="dm",
            user_id=handheld.device_id,
            user_name=self.store.devices().get(handheld.device_id, {}).get("name") or "Handheld",
            role_authorized=True,
        )
        message_id = f"u-{uuid.uuid4().hex[:16]}"
        event = MessageEvent(
            text=text,
            message_type=MessageType.TEXT,
            source=source,
            message_id=message_id,
            raw_message={"turn": turn_id},
            timestamp=datetime.now(UTC),
        )
        state["event"] = event
        try:
            await self.handle_message(event)
        except Exception as exc:
            logger.warning("Goblin Link: Hermes refused a turn: %s", type(exc).__name__)
            handheld.turns.pop(chat, None)
            return await reject("Hermes could not take the message")
        accepted = {"type": "turn.accepted", "id": turn_id, "message_id": message_id}
        if wav is not None:
            accepted["transcript"] = core.clean(text, core.MAX_TEXT_CHARS)
        await handheld.send(accepted)

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

    async def _on_cancel(self, handheld: Handheld, frame: dict[str, Any]) -> None:
        state = next((s for s in handheld.turns.values() if s["id"] == frame.get("id")), None)
        if state is not None:
            await self._command(handheld, state, "/stop")

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
                # A reply still open is as finished as it will get.
                for message_id, text in state["open"].items():
                    await handheld.send(
                        {"type": "message", "turn": state["id"], "message_id": message_id, "text": text, "done": True}
                    )
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
        text = core.clean(content, MAX_REPLY_CHARS)
        state = handheld.turns.get(chat)
        if state is None:
            # Nobody asked just now: a scheduled job's result, a late message.
            message_id = f"n-{uuid.uuid4().hex[:16]}"
            sent = await handheld.send({"type": "notice", "chat": chat, "message_id": message_id, "text": text})
            return SendResult(success=sent, message_id=message_id)
        if state["quiet"]:
            return SendResult(success=True, message_id=f"quiet-{state['id']}")
        state["next"] += 1
        message_id = f"a-{state['id']}-{state['next']}"
        final = bool(metadata and metadata.get("notify") is True)
        if not final:
            state["open"][message_id] = text
        sent = await handheld.send(
            {"type": "message", "turn": state["id"], "message_id": message_id, "text": text, "done": final}
        )
        return SendResult(success=sent, message_id=message_id)

    async def edit_message(self, chat_id: str, message_id: str, content: str, *, finalize: bool = False) -> SendResult:
        handheld, chat = self._find(chat_id)
        state = handheld.turns.get(chat) if handheld else None
        if state is None or message_id not in state["open"]:
            return SendResult(success=False, error="that message can no longer be changed")
        text = core.clean(content, MAX_REPLY_CHARS)
        if finalize:
            del state["open"][message_id]
        else:
            state["open"][message_id] = text
        sent = await handheld.send(
            {"type": "message", "turn": state["id"], "message_id": message_id, "text": text, "done": finalize}
        )
        return SendResult(success=sent, message_id=message_id)

    async def send_typing(self, chat_id: str, metadata: dict[str, Any] | None = None) -> None:
        await self._status(chat_id, True)

    async def stop_typing(self, chat_id: str) -> None:
        await self._status(chat_id, False)

    async def _status(self, chat_id: str, working: bool) -> None:
        handheld, chat = self._find(chat_id)
        state = handheld.turns.get(chat) if handheld else None
        if state is not None:
            await handheld.send({"type": "status", "turn": state["id"], "text": "", "working": working})

    async def get_chat_info(self, chat_id: str) -> dict[str, str]:
        return {"name": chat_id, "type": "dm"}

    # ---- questions to the owner ----------------------------------------------------------------

    async def send_exec_approval(
        self,
        chat_id: str,
        command: str,
        session_key: str,
        description: str = "dangerous command",
        metadata: dict[str, Any] | None = None,
    ) -> SendResult:
        text = f"{core.clean(description, 400)}\n{core.clean(command, 1200)}"
        choices = [{"id": "once", "label": "Run once"}, {"id": "deny", "label": "No"}]
        return await self._prompt(chat_id, "approval", text, choices, {"kind": "approval"})

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
        how["state"] = state
        how["expiry"] = asyncio.create_task(self._expire_prompt(handheld, prompt_id))
        handheld.prompts[prompt_id] = how
        sent = await handheld.send(
            {"type": "prompt", "id": prompt_id, "chat": chat, "kind": kind, "text": text, "choices": choices}
        )
        if not sent:
            await self._resolve_prompt(handheld, prompt_id, None)
            return SendResult(success=False, error="the handheld is not connected")
        return SendResult(success=True, message_id=prompt_id)

    async def _expire_prompt(self, handheld: Handheld, prompt_id: str) -> None:
        await asyncio.sleep(PROMPT_S)
        await self._resolve_prompt(handheld, prompt_id, None)

    async def _resolve_prompt(self, handheld: Handheld, prompt_id: str, answer: Any) -> None:
        """Gives Hermes the owner's answer. No answer (expired, disconnected) is a no."""
        how = handheld.prompts.pop(prompt_id, None)
        if how is None:
            return
        expiry = how.get("expiry")
        if expiry is not None and expiry is not asyncio.current_task():
            expiry.cancel()
        if how["kind"] == "approval":
            await self._command(handheld, how["state"], "/approve" if answer == "once" else "/deny")
            return
        offered: list[str] = how.get("choices") or []
        response = ""
        if isinstance(answer, str):
            index = int(answer[1:]) if answer[:1] == "c" and answer[1:].isdigit() else -1
            response = offered[index] if 0 <= index < len(offered) else core.clean(answer, 2000)
        try:
            from tools.clarify_gateway import resolve_gateway_clarify

            resolved = resolve_gateway_clarify(how["clarify_id"], response)
            if asyncio.iscoroutine(resolved):
                await resolved
        except Exception as exc:
            logger.warning("Goblin Link: a clarification was not taken: %s", type(exc).__name__)

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


def _slash(raw_args: str) -> str:
    """``/goblinlink pair | devices | unpair <id>``"""
    adapter = _ADAPTER
    if adapter is None:
        return "Goblin Link is not running. Enable the goblin_link platform and restart the gateway."
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


def _available() -> bool:
    try:
        import cryptography  # noqa: F401
        import websockets  # noqa: F401
    except ImportError:
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
    ctx.register_command(
        "goblinlink",
        _slash,
        description="Pair a Goblin Link handheld, or list and unpair them",
        args_hint="pair | devices | unpair <id>",
    )
