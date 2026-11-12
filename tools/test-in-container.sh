#!/bin/sh
# test-in-container.sh [cargo test args...] -- build tests on the host, run them in Docker.
#
# Why: goblind's executor kills process groups. A bad pid in that path once SIGKILLed the
# whole user session on the dev host (systemd --user, terminal, agent). Compiling is
# harmless; executing test binaries that send signals is not. So test binaries run only
# inside a throwaway container: no network, non-root, source mounted read-only.
#
# Usage: tools/test-in-container.sh                 # whole workspace
#        tools/test-in-container.sh -p goblind        # one crate
# Extra args after `--` go to every test binary (e.g. -- --test-threads=1).
set -eu
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
IMAGE="${GOBLIN_TEST_IMAGE:-ubuntu:24.04}"

cargo_args=""
bin_args=""
seen_sep=0
for a in "$@"; do
    if [ "$seen_sep" = 0 ] && [ "$a" = "--" ]; then seen_sep=1; continue; fi
    if [ "$seen_sep" = 0 ]; then cargo_args="$cargo_args $a"; else bin_args="$bin_args $a"; fi
done
[ -n "$cargo_args" ] || cargo_args=" --workspace"

list="$(mktemp)"
trap 'rm -f "$list"' EXIT
# shellcheck disable=SC2086
cargo test $cargo_args --no-run --message-format=json 2>/dev/null | python3 -c '
import json, os, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m.get("profile", {}).get("test"):
        print(os.path.dirname(m["manifest_path"]), m["executable"])
' > "$list"
[ -s "$list" ] || { echo "no test binaries built (did cargo fail? rerun without --message-format)" >&2; exit 1; }

# Docker reads the list and runs each binary from its crate directory, as cargo would.
docker run --rm --network none --user "$(id -u):$(id -g)" --tmpfs /tmp:exec -e HOME=/tmp \
    -e RUST_BACKTRACE=1 -v "$ROOT:$ROOT:ro" -v "$list:/tests.txt:ro" "$IMAGE" sh -c '
fail=0
while read -r dir bin; do
    echo "== $(basename "$bin")"
    cd "$dir"
    timeout 300 "$bin" '"$bin_args"' || { echo "FAILED: $bin"; fail=1; }
done < /tests.txt
exit $fail'
