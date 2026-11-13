# tg5040-brick-nextui

TrimUI Brick running NextUI (`PLATFORM=tg5040`, `DEVICE=brick`; TrimUI's launcher names itself "Trimui Brick"). Schema and hook contracts are in `../README.md`.


| Capability | Implementation | Status |
|---|---|---|
| LEDs | built-in `trimui-led-anim` driver on `/sys/class/led_anim`; zones `f1 f2 m l r lr`, ceiling 60 | `f1`, `f2` located by camera; `m l r lr` accepted but not seen |
| Rumble | `hooks/rumble.sh`: gpio227 + `/sys/class/motor/voltage`, as NextUI's `keymon` | triggers (heard by MIC1); not yet felt |
| Volume / mute | `hooks/output.sh`: softvol `Soft Volume Master` + `/sys/class/speaker/mute` | mapping computed, acoustic level unverified |
| Mic gain | `hooks/mic-gain.sh`: `MIC1 gain volume` (0-31 dB) | works; channel 0 is the candidate mic, speech unverified |
| Battery, temperature, health | `/sys/class/power_supply/axp2202-*`, `thermal_zone0` | works |
| Stay awake | `hooks/stay-awake.sh`: kernel wake lock `goblin-link` | works |
| Autostart | `hooks/autostart.sh`: tagged line in `.userdata/tg5040/auto.sh` that runs the pak's `on-boot.sh` (which starts `goblind`); the Settings switch "Connect when the handheld starts" calls it | works on a copy of auto.sh; a real boot is untried |
| Launcher entries | `hooks/register.sh`: `Tools/tg5040/<name>.pak` for both apps and games | works in scratch directories |
| Brightness | none (NextUI uses ioctls on `/dev/disp`) | unavailable |

Brick facts the hooks rely on: shell is BusyBox `ash` (no `pkill`, no `od`), `sleep` accepts fractions, and the speaker amplifier is gated
by `/sys/class/speaker/mute` (1 = muted, NextUI's resting state).

## Try it

```sh
adb shell
export GOBLIN_PLATFORMS_DIR=/mnt/UDISK/goblin-link-dev/platforms
/mnt/UDISK/goblin-link-dev/bin/goblinctl platform info
/mnt/UDISK/goblin-link-dev/bin/goblinctl platform leds set f1 red --brightness 60 --hold 5   # restores NextUI's LEDs afterwards
```

Use the `GOBLIN_AUTO_SH`, `GOBLIN_BOOT_SH`, `GOBLIN_TOOLS_DIR` and `GOBLIN_GAMES_DIR` overrides to rehearse `autostart` and `register` without touching the SD card.
