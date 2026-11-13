#!/bin/sh
# Run inside Docker; fake the build tools to verify failure before any test is executed.
set -eu
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/bin"
export RUNNER_PROBE="$scratch"
export PATH="$scratch/bin:$PATH"
cat > "$scratch/bin/cargo" <<'EOF'
#!/bin/sh
echo '{"reason":"compiler-artifact"}'
echo 'simulated compiler error' >&2
exit 42
EOF
cat > "$scratch/bin/python3" <<'EOF'
#!/bin/sh
cat >/dev/null
echo '/crate /crate/test'
EOF
cat > "$scratch/bin/docker" <<'EOF'
#!/bin/sh
touch "$RUNNER_PROBE/tests-ran"
EOF
chmod +x "$scratch/bin/"*
status=0
sh tools/test-in-container.sh > "$scratch/stdout" 2> "$scratch/stderr" || status=$?
[ "$status" -eq 42 ] || { echo "build failure was masked: exit $status" >&2; exit 1; }
[ ! -e "$scratch/tests-ran" ] || { echo 'tests ran after a failed build' >&2; exit 1; }
grep -q 'simulated compiler error' "$scratch/stderr"
echo 'test runner preserves build failures and diagnostics'
