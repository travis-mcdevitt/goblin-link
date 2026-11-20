"""The adapter's handling of questions, with Hermes stood in for: run with
``python hermes-plugin/tests/test_adapter.py``. No Hermes, no socket and no process is
started; the few names the adapter takes from Hermes are given here as plain stand-ins, so
this checks what the adapter does with them, not that Hermes still has them."""

import asyncio
import base64
import enum
import sys
import threading
import types
import unittest
from dataclasses import dataclass
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))


@dataclass
class SendResult:
    success: bool
    message_id: Any = None
    error: Any = None


@dataclass
class ExecApprovalPrompt:
    chat_id: str
    session_key: str
    text: str
    actions: list
    command: str
    request_id: Any = None


class ProcessingOutcome(enum.Enum):
    SUCCESS = "success"
    FAILURE = "failure"
    CANCELLED = "cancelled"


class BasePlatformAdapter:
    async def on_processing_complete(self, event, outcome):
        pass


class Approvals:
    """Hermes's side of an approval: who watches for its end, and what was resolved."""

    def __init__(self):
        self.pending = True
        self.observers = []
        self.resolved = []
        self.restricted = []
        # (clarify id, the owner's answer) for each clarifying question answered
        self.clarified = []

    def observe_gateway_settle(self, session_key, request_id, observer):
        if self.pending:
            self.observers.append(observer)
        return self.pending

    def restrict_gateway_approval_card_to_deny(self, session_key, request_id):
        self.restricted.append(request_id)

    def resolve_gateway_approval_card(self, session_key, choice, request_id):
        self.resolved.append((request_id, choice))
        self.settle("resolved")
        return 1

    def settle(self, reason):
        """The wait ends, on a thread that is not the gateway's loop, as in Hermes."""
        self.pending = False
        observers, self.observers = self.observers, []
        thread = threading.Thread(target=lambda: [watch(reason) for watch in observers])
        thread.start()
        thread.join()


def stand_in(approvals):
    base = types.ModuleType("gateway.platforms.base")
    base.BasePlatformAdapter = BasePlatformAdapter
    base.ExecApprovalPrompt = ExecApprovalPrompt
    base.MessageEvent = base.MessageType = object
    base.ProcessingOutcome = ProcessingOutcome
    base.SendResult = SendResult
    config = types.ModuleType("gateway.config")
    config.Platform = config.PlatformConfig = object
    approval = types.ModuleType("tools.approval")
    approval.observe_gateway_settle = approvals.observe_gateway_settle
    approval.resolve_gateway_approval_card = approvals.resolve_gateway_approval_card
    approval.restrict_gateway_approval_card_to_deny = approvals.restrict_gateway_approval_card_to_deny
    clarify = types.ModuleType("tools.clarify_gateway")
    clarify.resolve_gateway_clarify = lambda clarify_id, response: approvals.clarified.append((clarify_id, response))
    sys.modules.update({
        "gateway": types.ModuleType("gateway"), "gateway.config": config,
        "gateway.platforms": types.ModuleType("gateway.platforms"), "gateway.platforms.base": base,
        "tools": types.ModuleType("tools"), "tools.approval": approval, "tools.clarify_gateway": clarify,
    })


class Socket:
    def __init__(self):
        self.sent = []

    async def send(self, text):
        import json

        self.sent.append(json.loads(text))


ACTIONS = [("Allow Once", "once", "primary"), ("Deny", "deny", "danger")]


class QuestionTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.approvals = Approvals()
        stand_in(self.approvals)
        from goblin_link import adapter

        self.adapter = object.__new__(adapter.GoblinLinkAdapter)
        self.adapter.loop = asyncio.get_running_loop()
        self.socket = Socket()
        self.handheld = adapter.Handheld("dev-1", self.socket, {})
        self.adapter._handhelds = {"dev-1": self.handheld}
        self.event = object()
        self.handheld.turns["main"] = {"id": "t1", "chat": "main", "event": self.event, "quiet": False}

    def sent(self, kind):
        return [f for f in self.socket.sent if f["type"] == kind]

    async def settled(self):
        """Lets what another thread handed to the loop run."""
        for _ in range(5):
            await asyncio.sleep(0)

    async def ask(self, request_id="r1"):
        prompt = ExecApprovalPrompt("dev-1:main", "s1", "Run it?\nls", ACTIONS, "ls", request_id)
        sent = await self.adapter._send_exec_approval_prompt(prompt)
        self.assertTrue(sent.success)
        return sent.message_id

    async def test_an_approval_that_times_out_is_taken_back_from_the_handheld(self):
        prompt_id = await self.ask()
        self.assertEqual([f["id"] for f in self.sent("prompt")], [prompt_id])
        self.approvals.settle("timeout")
        await self.settled()
        self.assertEqual(self.sent("prompt.closed"), [{"type": "prompt.closed", "id": prompt_id}])
        # Its answer comes too late: nothing is resolved, and it is not told twice.
        await self.adapter._on_frame(self.handheld, {"type": "prompt.answer", "id": prompt_id, "answer": "once"})
        await self.settled()
        self.assertEqual(self.approvals.resolved, [])
        self.assertEqual(len(self.sent("prompt.closed")), 1)

    async def test_a_command_the_card_cannot_show_as_written_can_only_be_refused(self):
        # A card shows spacing as plain spaces and leaves the joiners out, so a command with
        # any of them is not on the card as it will run: the only answer offered is no.
        for n, command in enumerate(("ls\t-la", "ls\u00a0-la", "ls\u200d -la", "ls\u202e -la")):
            prompt = ExecApprovalPrompt("dev-1:main", "s1", f"Run it?\n{command}", ACTIONS, command, f"r{n}")
            self.assertTrue((await self.adapter._send_exec_approval_prompt(prompt)).success)
            self.assertEqual([c["id"] for c in self.sent("prompt")[-1]["choices"]], ["deny"], repr(command))
        self.assertEqual(self.approvals.restricted, ["r0", "r1", "r2", "r3"])
        await self.ask("r9")
        self.assertEqual([c["id"] for c in self.sent("prompt")[-1]["choices"]], ["once", "deny"])

    async def test_an_approval_answered_on_the_handheld_is_not_also_closed(self):
        prompt_id = await self.ask()
        await self.adapter._on_frame(self.handheld, {"type": "prompt.answer", "id": prompt_id, "answer": "once"})
        await self.settled()
        self.assertEqual(self.approvals.resolved, [("r1", "once")])
        self.assertEqual(self.sent("prompt.closed"), [])

    async def test_an_approval_that_ended_before_it_was_watched_is_closed_at_once(self):
        self.approvals.pending = False
        prompt_id = await self.ask()
        self.assertEqual(self.sent("prompt.closed"), [{"type": "prompt.closed", "id": prompt_id}])
        self.assertEqual(self.handheld.prompts, {})

    async def test_a_clarifying_question_hermes_retires_is_taken_back(self):
        asked = await self.adapter.send_clarify("dev-1:main", "Which?", ["a", "b"], "c1", "s1")
        other = await self.adapter.send_clarify("dev-1:main", "And?", ["a", "b"], "c2", "s1")
        await self.adapter.retire_clarify_card("c1", "It timed out.")
        await self.adapter.retire_clarify_card("c9", "Never asked.")
        self.assertEqual(self.sent("prompt.closed"), [{"type": "prompt.closed", "id": asked.message_id}])
        self.assertEqual(list(self.handheld.prompts), [other.message_id])

    async def test_what_was_asked_in_a_turn_is_closed_before_the_turn_is_said_to_be_over(self):
        prompt_id = await self.ask()
        asked = await self.adapter.send_clarify("dev-1:main", "Which?", ["a"], "c1", "s1")
        self.handheld.turns["side"] = {"id": "t2", "chat": "side", "event": object(), "quiet": False}
        elsewhere = await self.adapter.send_clarify("dev-1:side", "Which?", ["a"], "c2", "s1")
        await self.adapter.on_processing_complete(self.event, ProcessingOutcome.SUCCESS)
        said = [(f["type"], f.get("id") or f.get("turn")) for f in self.socket.sent[3:]]
        self.assertEqual(
            said, [("prompt.closed", prompt_id), ("prompt.closed", asked.message_id), ("turn.done", "t1")]
        )
        self.assertEqual(list(self.handheld.prompts), [elsewhere.message_id])

    async def turn(self, turn_id, **said):
        """The handheld sends a turn to the chat whose turn is under way; what it gave rise
        to is done with when this returns."""
        await self.adapter._on_frame(self.handheld, {"type": "turn", "id": turn_id, "chat": "main", **said})
        await asyncio.gather(*self.handheld.answering.values())

    def hear(self, *transcripts):
        """What Hermes hears in the voice notes to come, in turn."""
        heard = list(transcripts)

        async def transcribe(_wav):
            return heard.pop(0)

        self.adapter._transcribe = transcribe

    async def test_words_answer_the_question_named_and_as_words(self):
        older = await self.adapter.send_clarify("dev-1:main", "Which?", ["a", "b"], "c1", "s1")
        newer = await self.adapter.send_clarify("dev-1:main", "Called what?", [], "c2", "s1")
        # Words that look like the id of a choice are still words.
        await self.adapter._on_frame(
            self.handheld, {"type": "prompt.answer", "id": older.message_id, "answer": "c1", "words": True}
        )
        self.assertEqual(self.approvals.clarified, [("c1", "c1")])
        # A press gives the choice itself; a question with no choices takes what it is given.
        await self.adapter._on_frame(self.handheld, {"type": "prompt.answer", "id": newer.message_id, "answer": "c0"})
        self.assertEqual(self.approvals.clarified[-1], ("c2", "c0"))
        # Words never approve.
        approval = await self.ask()
        await self.adapter._on_frame(
            self.handheld, {"type": "prompt.answer", "id": approval, "answer": "once", "words": True}
        )
        await self.settled()
        self.assertEqual(self.approvals.resolved, [("r1", "deny")])

    async def test_a_voice_note_answers_the_question_it_names_with_what_was_heard(self):
        older = await self.adapter.send_clarify("dev-1:main", "Which colour?", [], "c1", "s1")
        await self.adapter.send_clarify("dev-1:main", "And then?", [], "c2", "s1")
        wav = base64.b64encode(b"RIFF" + bytes(60)).decode()
        # Not understood: the turn fails, and the question still waits for an answer.
        self.hear("", "c1")
        await self.turn("t2", voice_wav_b64=wav, answers=older.message_id)
        self.assertEqual(self.sent("turn.failed")[-1]["reason"], "the voice note could not be understood")
        self.assertIn(older.message_id, self.handheld.prompts)
        await self.turn("t3", voice_wav_b64=wav, answers=older.message_id)
        self.assertEqual(self.sent("status")[-1], {"type": "status", "turn": "t3", "text": "c1",
                                                   "working": True, "heard": True})
        # The question named, not the newest; and what was heard is words.
        self.assertEqual(self.approvals.clarified, [("c1", "c1")])
        self.assertEqual(self.sent("prompt.closed"), [{"type": "prompt.closed", "id": older.message_id}])
        self.assertEqual(self.sent("turn.done"), [{"type": "turn.done", "turn": "t3"}])
        # The turn that asked goes on.
        self.assertEqual(self.handheld.turns["main"]["id"], "t1")

    async def test_only_a_voice_note_naming_a_waiting_question_is_taken_in_a_busy_chat(self):
        approval = await self.ask()
        asked = await self.adapter.send_clarify("dev-1:main", "Which?", [], "c1", "s1")
        wav = base64.b64encode(b"RIFF" + bytes(60)).decode()
        self.hear("yes", "yes")
        for n, said in enumerate([
            {"text": "No, don't install it"},  # words: an answer to a card, not to it
            {"text": "Brick", "answers": asked.message_id},  # words go as prompt.answer
            {"voice_wav_b64": wav},  # names no question
            {"voice_wav_b64": wav, "answers": approval},  # an approval is never answered so
            {"voice_wav_b64": wav, "answers": "p-other"},
        ]):
            await self.turn(f"t{n + 2}", **said)
        rejected = [(f["id"], f["reason"]) for f in self.sent("turn.rejected")]
        self.assertEqual(rejected, [(f"t{n}", "busy") for n in range(2, 7)])
        self.assertEqual((self.approvals.clarified, self.approvals.resolved), ([], []))
        # One voice note at a time is heard in a chat.
        hearing = asyncio.Event()

        async def slow(_wav):
            await hearing.wait()
            return "blue"

        self.adapter._transcribe = slow
        await self.adapter._on_frame(self.handheld, {"type": "turn", "id": "t8", "chat": "main",
                                                     "voice_wav_b64": wav, "answers": asked.message_id})
        await self.adapter._on_frame(self.handheld, {"type": "turn", "id": "t9", "chat": "main",
                                                     "voice_wav_b64": wav, "answers": asked.message_id})
        self.assertEqual(self.sent("turn.rejected")[-1], {"type": "turn.rejected", "id": "t9", "reason": "busy"})
        hearing.set()
        await asyncio.gather(*self.handheld.answering.values())
        self.assertEqual(self.approvals.clarified, [("c1", "blue")])


class VoiceNoteTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        stand_in(Approvals())
        from goblin_link import adapter

        self.module = adapter
        self.adapter = object.__new__(adapter.GoblinLinkAdapter)
        self.adapter.loop = asyncio.get_running_loop()
        self.adapter.store = types.SimpleNamespace(devices=lambda: {})
        self.adapter.build_source = lambda **how: how
        self.handled = []

        async def handle_message(event):
            self.handled.append(event)

        self.adapter.handle_message = handle_message
        self.socket = Socket()
        self.handheld = adapter.Handheld("dev-1", self.socket, {})
        self.adapter._handhelds = {"dev-1": self.handheld}
        self.state = {"id": "t1", "chat": "main", "chat_id": "dev-1:main", "quiet": False}
        self.handheld.turns["main"] = self.state
        self.stood_in = adapter.MessageEvent, adapter.MessageType
        adapter.MessageEvent = types.SimpleNamespace
        adapter.MessageType = types.SimpleNamespace(TEXT="text")

    async def asyncTearDown(self):
        self.module.MessageEvent, self.module.MessageType = self.stood_in

    async def hear(self, transcript):
        async def transcribe(wav):
            return transcript

        self.adapter._transcribe = transcribe
        await self.adapter._start_turn(self.handheld, self.state, "m1", None, b"RIFF")
        heard = [f for f in self.socket.sent if f["type"] == "status" and f.get("heard")]
        self.assertEqual(len(heard), 1, self.socket.sent)
        return heard[0]["text"]

    async def test_all_of_a_long_voice_note_is_said_to_be_heard(self):
        transcript = "\n".join(f"Sentence {n} of a voice note that goes on for a while." for n in range(14))
        self.assertGreater(len(transcript), 700)
        self.assertEqual(await self.hear(transcript), transcript)
        self.assertEqual(self.handled[0].text, transcript)

    async def test_what_is_heard_is_kept_to_what_the_handheld_takes(self):
        heard = await self.hear("a" * 9000)
        self.assertEqual(heard, "a" * 8192)

    async def test_a_status_line_for_a_step_stays_one_short_line(self):
        await self.adapter._status("dev-1:main", True, "first step\n" + "b" * 300)
        self.assertEqual(self.socket.sent[-1]["text"], "b" * 200)


class BusyCommandTests(unittest.IsolatedAsyncioTestCase):
    """What the owner sends while Hermes replies in a chat."""

    async def asyncSetUp(self):
        stand_in(Approvals())
        from goblin_link import adapter

        self.module = adapter
        self.adapter = object.__new__(adapter.GoblinLinkAdapter)
        self.adapter.loop = asyncio.get_running_loop()
        self.handled = []
        self.hermes = self.answers

        async def handle_message(event):
            self.handled.append(event)
            await self.hermes(event)

        self.adapter.handle_message = handle_message
        self.socket = Socket()
        self.handheld = adapter.Handheld("dev-1", self.socket, {})
        self.adapter._handhelds = {"dev-1": self.handheld}
        self.source = object()
        self.running = {"id": "t1", "chat": "main", "chat_id": "dev-1:main", "quiet": False, "progress": set(),
                        "event": types.SimpleNamespace(source=self.source)}
        self.handheld.turns["main"] = self.running
        self.stood_in = adapter.MessageEvent, adapter.MessageType
        adapter.MessageEvent = types.SimpleNamespace
        adapter.MessageType = types.SimpleNamespace(TEXT="text", COMMAND="command")

    async def asyncTearDown(self):
        self.module.MessageEvent, self.module.MessageType = self.stood_in

    async def says(self, event, text, final=True):
        """Hermes says ``text`` under ``event``, as its answer or as a step before it."""
        await self.adapter.send("dev-1:main", text, reply_to=event.message_id,
                                metadata={"notify": True} if final else None)

    async def answers(self, event):
        # Hermes answers a command in a busy chat at once, as a reply to it, or refuses it in
        # words of its own: either way its handling returns.
        await self.says(event, f"done: {event.text}")

    async def turn(self, turn_id, **said):
        await self.adapter._on_frame(self.handheld, {"type": "turn", "id": turn_id, "chat": "main", **said})
        await asyncio.gather(*self.handheld.dispatching)

    def said(self, turn_id):
        return [f["type"] for f in self.socket.sent if turn_id in (f.get("id"), f.get("turn"))]

    def frames(self, kind):
        return [f for f in self.socket.sent if f["type"] == kind]

    async def test_a_slash_command_goes_to_hermes_as_its_own_turn(self):
        # Whatever the command: Hermes decides, as from its terminal.
        for n, text in enumerate(["/status", "/model gpt", "/s nudge", "/not-one"]):
            await self.turn(f"t{n + 2}", text=text)
            self.assertEqual(self.said(f"t{n + 2}"), ["turn.accepted", "message", "turn.done"], text)
        self.assertEqual([e.text for e in self.handled], ["/status", "/model gpt", "/s nudge", "/not-one"])
        self.assertTrue(all(e.message_type == "command" and e.source is self.source for e in self.handled))
        # Hermes's answer is the command's reply, not a step of the reply under way.
        reply = self.frames("message")[0]
        self.assertEqual((reply["turn"], reply["text"], reply["done"]), ("t2", "done: /status", True))
        self.assertEqual(self.running["progress"], set())
        self.assertIs(self.handheld.turns["main"], self.running)
        # Said late under a command whose turn is over: an answer is a notice, a step nothing.
        before = len(self.socket.sent)
        await self.says(self.handled[0], "a step", final=False)
        await self.says(self.handled[0], "late")
        self.assertEqual([(f["type"], f["text"]) for f in self.socket.sent[before:]], [("notice", "late")])

    async def test_approvals_and_what_is_not_a_command_wait(self):
        wav = base64.b64encode(b"RIFF" + bytes(60)).decode()
        said = [
            {"text": "/approve"},  # answered on its card alone
            {"text": "/approve always"},
            {"text": "/deny"},
            {"text": "/Status"},
            {"text": " /status"},
            {"text": "status"},
            {"text": "/status\u00a0x"},
            {"voice_wav_b64": wav},
        ]
        for n, frame in enumerate(said):
            await self.turn(f"t{n + 2}", **frame)
        # A voice note still being heard is not a turn Hermes has.
        del self.running["event"]
        await self.turn("t20", text="/status")
        rejected = [f["id"] for f in self.frames("turn.rejected")]
        self.assertEqual(rejected, [f"t{n + 2}" for n in range(len(said))] + ["t20"])
        self.assertEqual(self.handled, [])

    async def test_a_command_turn_ends_when_hermes_has_dealt_with_it(self):
        async def queues(event):
            # "Queued" is the command's answer; the prompt is a turn Hermes runs later, or
            # drops (a /stop clears it), without a word to this adapter either way.
            if event.text.startswith("/queue"):
                await self.says(event, "Queued")

        self.hermes = queues
        await self.turn("t2", text="/queue then tidy up")
        await self.turn("t3", text="/retry")  # answered with nothing at all
        self.assertEqual(self.said("t2"), ["turn.accepted", "message", "turn.done"])
        self.assertEqual(self.said("t3"), ["turn.accepted", "turn.done"])
        # A /stop after it finds the /queue's turn over already, and is a turn of its own.
        await self.turn("t4", text="/stop")
        self.assertEqual(self.said("t4"), ["turn.accepted", "turn.done"])
        self.assertEqual([f["turn"] for f in self.frames("turn.done")], ["t2", "t3", "t4"])
        # The queued prompt runs after all: its steps are not shown, its answer is a notice.
        before = len(self.socket.sent)
        queued = self.handled[0]
        await self.says(queued, "Tidying", final=False)
        await self.says(queued, "Tidied.")
        await self.adapter.on_processing_complete(types.SimpleNamespace(message_id=queued.message_id),
                                                  ProcessingOutcome.SUCCESS)
        self.assertEqual([(f["type"], f.get("text")) for f in self.socket.sent[before:]], [("notice", "Tidied.")])

    async def test_a_handheld_that_goes_leaves_its_commands_to_hermes(self):
        release = asyncio.Event()

        async def slow(event):
            await release.wait()
            await self.answers(event)

        self.hermes = slow
        await self.adapter._on_frame(self.handheld, {"type": "turn", "id": "t2", "chat": "main", "text": "/new"})
        await asyncio.sleep(0)
        await self.adapter._abandon(self.handheld)
        release.set()
        done = await asyncio.gather(*self.handheld.dispatching, return_exceptions=True)
        self.assertEqual(done, [None])
        self.assertEqual(self.said("t2")[-1], "turn.done")


class Registry:
    """Hermes's command registry and its plugin and skill commands, as far as the adapter
    reads them, in Hermes's shapes: what a gateway platform may use of the registry (config
    gates already applied), plugin commands as ``(name, description, args_hint)``, and
    skill commands as ``{"/name": {"name", "description", ...}}``."""

    def __init__(self):
        def command(name, description, category, args_hint="", busy_policy="reject"):
            return types.SimpleNamespace(
                name=name, category=category, args_hint=args_hint, busy_policy=busy_policy,
                # In the owner's language, as Hermes's describe() gives it.
                describe=lambda: f"{description} (de)",
            )

        self.commands = [
            command("start", "Acknowledge platform start pings", "Session", busy_policy="dispatch"),
            command("new", "Start a new session", "Session", "[name]", "interrupt_then_dispatch"),
            command("topic", "Enable Telegram DM topic sessions", "Session", "[off|help]"),
            command("approve", "Approve a pending dangerous command", "Session", "[session|always]", "dispatch"),
            command("deny", "Deny a pending dangerous command", "Session", "[all] [reason]", "dispatch"),
            command("bg", "Run a prompt in a separate background session", "Session", "<prompt>", "dispatch"),
            command("sethome", "Set this chat as the home channel", "Session"),
            command("model", "Switch model", "Configuration", "[model|reset] [--global]"),
            command("platform", "Pause, resume, or list a failing gateway platform", "Info", "<pause|resume|list>"),
            command("help", "Show available commands", "Info", "[skills|<filter>]", "dispatch"),
        ]
        self.plugins = [
            ("zeta", "Needs words", "<what>"),
            ("goblinlink", "Pair a handheld", "pair | devices"),
            ("deny", "A plugin's own deny", ""),
        ]
        self.skills = {
            "/pdf": {"name": "pdf", "description": "Read PDF files"},
            "/no-here": {"name": "no-here", "description": "Turned off on this platform"},
            "/help": {"name": "help", "description": "A skill under a command's name"},
            "/Not Valid": {"name": "Not Valid", "description": "A name no handheld takes"},
            "/start": {"name": "start", "description": "A skill under a name left out"},
        }
        self.bundles = {}
        self.disabled_on = []

    def install(self, test):
        """Puts these where Hermes keeps them, for the length of ``test``."""
        commands = types.ModuleType("hermes_cli.commands")
        commands.category_label = lambda category: f"{category} (de)"
        commands._iter_plugin_command_entries = lambda: list(self.plugins)
        platforms = types.ModuleType("hermes_cli.commands_platforms")
        platforms._gateway_available_commands = lambda: list(self.commands)
        platforms._gateway_bundles = lambda: sorted(self.bundles.items())
        skill_commands = types.ModuleType("agent.skill_commands")
        skill_commands.get_skill_commands = lambda: dict(self.skills)
        skill_utils = types.ModuleType("agent.skill_utils")

        def disabled(platform=None):
            self.disabled_on.append(platform)
            return {"no-here"}

        skill_utils.get_disabled_skill_names = disabled
        modules = {
            "hermes_cli": types.ModuleType("hermes_cli"), "hermes_cli.commands": commands,
            "hermes_cli.commands_platforms": platforms, "agent": types.ModuleType("agent"),
            "agent.skill_commands": skill_commands, "agent.skill_utils": skill_utils,
        }
        kept = {name: sys.modules.get(name) for name in modules}
        sys.modules.update(modules)

        def restore():
            for name, module in kept.items():
                if module is None:
                    sys.modules.pop(name, None)
                else:
                    sys.modules[name] = module

        test.addCleanup(restore)


class CommandListTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        stand_in(Approvals())
        from goblin_link import adapter

        self.module = adapter
        self.registry = Registry()
        self.registry.install(self)
        self.adapter = object.__new__(adapter.GoblinLinkAdapter)
        self.adapter.loop = asyncio.get_running_loop()
        self.socket = Socket()
        self.handheld = adapter.Handheld("dev-1", self.socket, {})
        self.adapter._handhelds = {"dev-1": self.handheld}

    def listed(self):
        return {item["name"]: item for item in self.module._command_list()}

    async def offered(self):
        """Offers the handheld the list as it is now; the lists it has been sent so far."""
        self.adapter._offer_commands(self.handheld)
        await asyncio.gather(*self.handheld.offering)
        return [f["items"] for f in self.socket.sent if f["type"] == "commands"]

    def test_the_list_is_hermess_commands_then_plugin_commands_then_skills(self):
        listed = self.listed()
        self.assertEqual(list(listed), ["new", "bg", "model", "help", "zeta", "pdf"])
        self.assertEqual(listed["model"], {
            "name": "model", "description": "Switch model (de)", "group": "commands",
            "category": "Configuration (de)", "args": "[model|reset] [--global]",
            "needs_text": False,
        })
        # A command that names what it needs first never goes without it.
        self.assertEqual([listed["bg"]["needs_text"], listed["help"]["needs_text"]], [True, False])
        self.assertEqual((listed["zeta"]["category"], listed["zeta"]["needs_text"]), ("Plugins (de)", True))
        self.assertEqual(listed["pdf"], {
            "name": "pdf", "description": "Read PDF files", "group": "skills", "category": "",
            "args": "", "needs_text": False,
        })
        # A skill turned off for this platform is not offered; nor one under a command's
        # name, which Hermes would take for the command.
        self.assertEqual(self.registry.disabled_on, ["goblin_link"])
        names = [item["name"] for item in self.module._command_list()]
        self.assertEqual(len(names), len(set(names)))
        self.assertEqual(listed["help"]["description"], "Show available commands (de)")
        self.assertEqual(listed["help"]["group"], "commands")

    def test_bundles_are_listed_with_the_skills_and_a_bundle_wins_over_its_skill(self):
        self.registry.bundles = {
            "/research": {"name": "Research", "description": "Load 3 skills as a bundle"},
            "/pdf": {"name": "pdf", "description": "Read, fill in and sign PDFs"},
            "/help": {"name": "help", "description": "A bundle under a command's name"},
            "/start": {"name": "start", "description": "A bundle under a name left out"},
        }
        listed = self.listed()
        self.assertEqual(list(listed), ["new", "bg", "model", "help", "zeta", "pdf", "research"])
        self.assertEqual(listed["research"], {
            "name": "research", "description": "Load 3 skills as a bundle", "group": "skills",
            "category": "", "args": "", "needs_text": False,
        })
        # Hermes runs the bundle when /pdf is sent, so the bundle is what the sheet describes.
        self.assertEqual(listed["pdf"]["description"], "Read, fill in and sign PDFs")
        self.assertEqual(listed["help"]["group"], "commands")
        # A Hermes whose bundles cannot be read still sends its skills.
        sys.modules["hermes_cli.commands_platforms"]._gateway_bundles = None
        self.assertEqual(self.listed()["pdf"]["description"], "Read PDF files")

    def test_what_makes_no_sense_on_a_handheld_is_left_out(self):
        left_out = {"start", "topic", "sethome", "platform", "approve", "deny", "goblinlink"}
        self.assertEqual(self.module.LEFT_OUT, left_out)
        offered = {c.name for c in self.registry.commands}
        offered |= {name for name, _, _ in self.registry.plugins} | {k[1:] for k in self.registry.skills}
        self.assertTrue(left_out <= offered)
        # Left out whichever kind it is: a registry command, a plugin's, a skill's.
        self.assertFalse(left_out & set(self.listed()))

    def test_each_command_fits_what_the_handheld_takes(self):
        self.registry.commands[1].name = "x" * 33
        self.registry.commands[5].describe = lambda: "Long\tand\nlong " + "d" * 300
        self.registry.commands[7].category = "c" * 60
        listed = self.listed()
        self.assertNotIn("x" * 33, listed)
        self.assertEqual(len(listed["bg"]["description"]), 200)
        self.assertTrue(listed["bg"]["description"].startswith("Long and long ddd"))
        self.assertTrue(listed["bg"]["description"].endswith("…"))
        self.assertEqual(len(listed["model"]["category"]), 40)
        self.registry.skills = {f"/s{n}": {"name": f"s{n}", "description": ""} for n in range(400)}
        self.assertEqual(len(self.module._command_list()), 300)

    def test_a_hermes_without_the_registry_sends_no_list_and_one_without_skills_the_rest(self):
        del sys.modules["hermes_cli.commands_platforms"]
        self.assertIsNone(self.module._command_list())
        self.registry.install(self)
        sys.modules["agent.skill_commands"].get_skill_commands = None
        self.assertEqual(list(self.listed())[-1], "zeta")

    async def test_the_list_is_sent_again_only_when_it_changes(self):
        sent = await self.offered()
        self.assertEqual(len(sent), 1)
        self.assertEqual(await self.offered(), sent, "the same list is not sent twice")
        # A turn ends: the list is looked at again, and sent only if it changed.
        event = object()
        self.handheld.turns["main"] = {"id": "t1", "chat": "main", "event": event, "quiet": False}
        await self.adapter.on_processing_complete(event, ProcessingOutcome.SUCCESS)
        await asyncio.gather(*self.handheld.offering)
        self.assertEqual(len(await self.offered()), 1)
        self.registry.skills["/pdf"]["description"] = "Read and fill in PDF files"
        sent = await self.offered()
        self.assertEqual(len(sent), 2)
        self.assertEqual(sent[1][-1]["description"], "Read and fill in PDF files")


if __name__ == "__main__":
    unittest.main()
