"""The manifest against the code: run with ``python hermes-plugin/tests/test_manifest.py``.
Hermes reads ``plugin.yaml`` to know what the plugin offers, so a tool registered or a
variable read without being listed there is one Hermes may not show or offer. This reads the
plugin's source rather than importing it, so it needs neither Hermes nor any package."""

import ast
import re
import sys
import unittest
from pathlib import Path

PLUGIN = Path(__file__).resolve().parents[1]


def manifest_lists():
    """The names under ``provides_tools`` and ``optional_env``. The manifest is plain enough
    to read line by line, which keeps this test free of a YAML package."""
    tools, env, section = [], [], None
    for line in (PLUGIN / "plugin.yaml").read_text(encoding="utf-8").splitlines():
        if line and not line[0].isspace():
            section = line.split(":", 1)[0]
        elif section == "provides_tools" and (m := re.fullmatch(r"\s+-\s+(\S+)\s*", line)):
            tools.append(m.group(1))
        elif section == "optional_env" and (m := re.fullmatch(r"\s+-\s+name:\s+(\S+)\s*", line)):
            env.append(m.group(1))
    return tools, env


def code_names():
    """Tools passed to ``register_tool(name=...)`` and variables read through
    ``os.environ.get``, ``os.environ[...]`` or ``os.getenv`` anywhere in the plugin."""
    tools, env = set(), set()
    for path in PLUGIN.rglob("*.py"):
        if "tests" in path.relative_to(PLUGIN).parts:
            continue
        for node in ast.walk(ast.parse(path.read_text(encoding="utf-8"))):
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
                attr, target = node.func.attr, ast.unparse(node.func.value)
                if attr == "register_tool":
                    for kw in node.keywords:
                        if kw.arg == "name" and isinstance(kw.value, ast.Constant):
                            tools.add(kw.value.value)
                elif (target, attr) in (("os.environ", "get"), ("os", "getenv")) and node.args:
                    if isinstance(node.args[0], ast.Constant):
                        env.add(node.args[0].value)
            elif isinstance(node, ast.Subscript) and ast.unparse(node.value) == "os.environ":
                if isinstance(node.slice, ast.Constant):
                    env.add(node.slice.value)
    return tools, env


class ManifestTests(unittest.TestCase):
    def setUp(self):
        self.listed_tools, self.listed_env = manifest_lists()
        self.code_tools, self.code_env = code_names()

    def test_the_code_was_read(self):
        # Guards the two tests below against passing because nothing was found.
        self.assertIn("handheld_run", self.code_tools)
        self.assertIn("GOBLIN_LINK_HOSTS", self.code_env)

    def test_every_registered_tool_is_listed_and_no_other(self):
        self.assertEqual(sorted(self.listed_tools), sorted(set(self.listed_tools)))
        self.assertEqual(set(self.listed_tools), self.code_tools)

    def test_every_variable_read_is_listed_and_no_other(self):
        self.assertEqual(sorted(self.listed_env), sorted(set(self.listed_env)))
        self.assertEqual(set(self.listed_env), self.code_env)

    def test_the_readme_names_every_tool_and_variable(self):
        readme = (PLUGIN / "README.md").read_text(encoding="utf-8")
        for name in sorted(self.code_tools | self.code_env):
            self.assertRegex(readme, rf"\b{name}\b", f"{name} is not in README.md")


if __name__ == "__main__":
    sys.exit(unittest.main())
