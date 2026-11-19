#!/usr/bin/env python3
"""Run in a throwaway container as root (start-daemon.sh writes /run and /mnt/UDISK): source
start-daemon.sh against fake binaries and check how it treats a running daemon and retired
command files. Never run it on a device or a host."""
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import threading

source = Path(__file__).resolve().parent.parent
NEW = "0.2.0"
OLD = "0.1.0"
RUN = Path("/run/goblin-link")
INTERNAL = Path("/mnt/UDISK/goblin-link")
assert os.getuid() == 0 and Path("/.dockerenv").exists(), "only in a throwaway container"


def write(path, text, mode=0o755):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(mode)


class Case:
    def __init__(self):
        for d in (RUN, INTERNAL):
            shutil.rmtree(d, ignore_errors=True)
        self.tmp = Path(tempfile.mkdtemp())
        self.pak = self.tmp / "Goblin Link.pak"
        self.state = self.tmp / "state"
        self.state.mkdir()
        self.sdcard = self.tmp / "sdcard"
        self.pak.mkdir(parents=True)
        shutil.copy(source / "packaging/nextui-pak/start-daemon.sh", self.pak / "start-daemon.sh")
        # A copy of sleep named goblind: /proc/<pid>/comm reads "goblind", as for the real one.
        self.goblind = self.tmp / "goblind"
        shutil.copy(shutil.which("sleep"), self.goblind)
        write(self.pak / "bin/goblind", f'#!/bin/sh\n[ "$1" = --version ] && echo "goblind {NEW}"\n')
        write(self.pak / "bin/goblinctl", f"""#!/bin/sh
p="$(cat '{self.state}/pid' 2>/dev/null)"
[ -n "$p" ] && kill -0 "$p" 2>/dev/null || exit 4
echo "profile   personal"
echo "version   $(cat '{self.state}/version') (up 1 s)"
""")
        fake = self.tmp / "path"
        write(fake / "ps", "#!/bin/sh\n")
        write(fake / "start-stop-daemon", f"""#!/bin/sh
while [ $# -gt 0 ]; do [ "$1" = -p ] && pidfile="$2"; shift; done
'{self.goblind}' 1000 </dev/null >/dev/null 2>&1 &
echo $! > "$pidfile"
echo $! > '{self.state}/pid'
echo {NEW} > '{self.state}/version'
echo start >> '{self.state}/starts'
rm -f '{RUN}/active'
python3 -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' '{RUN}/active'
""")
        self.path = f"{fake}:{os.environ['PATH']}"
        self.children = []

    def daemon(self, version, program=None):
        """A daemon of `version` already running, as start-daemon.sh left it."""
        child = subprocess.Popen([program or self.goblind, "1000"])
        threading.Thread(target=child.wait, daemon=True).start()
        self.children.append(child)
        RUN.mkdir(parents=True, exist_ok=True)
        (RUN / "goblind.pid").write_text(f"{child.pid}\n")
        (self.state / "pid").write_text(f"{child.pid}\n")
        (self.state / "version").write_text(version)
        return child

    def run(self):
        env = dict(os.environ, PATH=self.path, PAK_DIR=str(self.pak), SDCARD_PATH=str(self.sdcard))
        subprocess.run(["sh", "-c", '. "$1"', "sh", self.pak / "start-daemon.sh"], env=env, check=True, timeout=60)

    def starts(self):
        f = self.state / "starts"
        return len(f.read_text().split()) if f.exists() else 0

    def close(self):
        for child in self.children:
            if child.poll() is None:
                child.kill()
        for line in (self.state / "pid").read_text().split() if (self.state / "pid").exists() else []:
            try:
                os.kill(int(line), signal.SIGKILL)
            except ProcessLookupError:
                pass
        shutil.rmtree(self.tmp)


def alive(child):
    try:
        child.wait(timeout=0.5)
        return False
    except subprocess.TimeoutExpired:
        return True


# No daemon: one is started.
c = Case()
c.run()
assert c.starts() == 1
c.close()

# A daemon of this pak's version is left alone.
c = Case()
same = c.daemon(NEW)
c.run()
assert c.starts() == 0 and alive(same)
c.close()

# A daemon of another version is stopped by its pidfile, and this pak's daemon is started.
c = Case()
old = c.daemon(OLD)
c.run()
assert not alive(old), "the old daemon was stopped"
assert old.returncode == -signal.SIGTERM, f"stopped with SIGTERM, so it writes its stores: {old.returncode}"
assert c.starts() == 1
assert (c.state / "version").read_text().strip() == NEW
assert f"replacing goblind {OLD} with {NEW}" in (c.sdcard / ".userdata/tg5040/goblin-link/logs/goblind.log").read_text()
c.close()

# A daemon of this version whose binary a reinstall replaced: stopped and started again, not
# left to stop itself after this script has looked.
c = Case()
copy = c.tmp / "bin-old" / "goblind"
copy.parent.mkdir()
shutil.copy(c.goblind, copy)
stale = c.daemon(NEW, program=copy)
tmp = copy.with_name("goblind.tmp")
shutil.copy(c.goblind, tmp)
os.replace(tmp, copy)  # as the Pak Store writes an update: a new file renamed over the old
c.run()
assert not alive(stale), "the daemon with a replaced binary was stopped"
assert c.starts() == 1
c.close()

# A pidfile naming some other process: nothing is killed on its word.
c = Case()
other = c.daemon(OLD, program=shutil.which("sleep"))
c.run()
assert alive(other) and c.starts() == 0
c.close()

# Retired command files: gone from the pak folder and, unless the owner edited them, from
# the installed commands; the shipped ones are installed as before.
for edited in (False, True):
    c = Case()
    shipped = '{"name": "chat.suggest_replies"}\n'
    write(c.pak / "commands.d/chat.suggest_replies.json", shipped, 0o644)
    write(c.pak / "commands.d/notes.json", '{"dir": "@PAK_DIR@"}\n', 0o644)
    cmds = INTERNAL / "commands.d"
    write(cmds / ".shipped/chat.suggest_replies.json", shipped, 0o644)
    write(cmds / "chat.suggest_replies.json", '{"mine": true}\n' if edited else shipped, 0o644)
    c.run()
    assert not (c.pak / "commands.d/chat.suggest_replies.json").exists()
    assert not (cmds / ".shipped/chat.suggest_replies.json").exists()
    assert (cmds / "chat.suggest_replies.json").exists() == edited
    assert (cmds / "notes.json").read_text() == f'{{"dir": "{c.pak}"}}\n'
    c.close()

print("start-daemon.sh: starts, keeps the same version, replaces another or a replaced binary, ignores a foreign pid, drops retired commands")
