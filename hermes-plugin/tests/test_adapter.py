"""The adapter's handling of questions, with Hermes stood in for: run with
``python hermes-plugin/tests/test_adapter.py``. No Hermes, no socket and no process is
started; the few names the adapter takes from Hermes are given here as plain stand-ins, so
this checks what the adapter does with them, not that Hermes still has them."""

import asyncio
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

    def observe_gateway_settle(self, session_key, request_id, observer):
        if self.pending:
            self.observers.append(observer)
        return self.pending

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
    clarify = types.ModuleType("tools.clarify_gateway")
    clarify.resolve_gateway_clarify = lambda clarify_id, response: True
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


if __name__ == "__main__":
    unittest.main()
