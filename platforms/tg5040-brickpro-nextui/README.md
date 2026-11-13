# tg5040-brickpro-nextui

TrimUI Brick Pro running NextUI (`PLATFORM=tg5040`, `DEVICE=brickpro`). **Not qualified:** this file was written
from NextUI's source (`workspace/tg5040/platform/platform.h` and `platform.c`) and from the
TrimUI Brick's platform file. Nobody has run Goblin Link on this device.

Where it differs from the Brick is noted in `platform.toml`. The hooks are copies of the
Brick's and are equally untried here.

If you have one, see "Testing on a device nobody has tried" in `CONTRIBUTING.md` and the
qualification issue for this device. The app writes `GoblinLink/device-report.txt` on the SD
card the first time it starts on an unqualified device; that file is what we need most.
