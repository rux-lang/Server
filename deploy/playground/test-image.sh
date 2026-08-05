#!/usr/bin/env bash
# Drives the playground image directly, with no server involved.
#
#   test-image.sh [image]
#
# Each fixture must fail the intended way. A sandbox that merely refuses
# everything would pass a success-only suite, so most of these assert that a
# specific containment mechanism - the compile timeout, the pid limit, the
# memory limit, the dropped network, the output cap, the output framing - is the
# thing that stopped the program.

set -euo pipefail

image="${1:-rux-playground:0.3.0}"

# Fixtures are bind-mounted into the container, so they have to live somewhere
# the container runtime can see. That is the default temporary directory on
# Linux; RUX_PLAYGROUND_TEST_ROOT exists for hosts where it is not, such as a
# Git Bash shell whose /tmp the Docker daemon cannot resolve.
if [[ -n "${RUX_PLAYGROUND_TEST_ROOT:-}" ]]; then
    work="$RUX_PLAYGROUND_TEST_ROOT"
    mkdir -p "$work"
else
    work=$(mktemp -d)
    trap 'rm -rf -- "$work"' EXIT
fi

failures=0
skipped=0

# Mirrors crates/sandbox: the resource envelope one run executes in.
MEMORY_BYTES=134217728
CPU=0.500
PIDS=32
TMPFS_BYTES=33554432
COMPILE_SECONDS=5
RUN_SECONDS=3
MAX_OUTPUT=16384
SEPARATOR=$'\x1e'

# Reports whether the image carries a seeded package cache. Without one, no
# program can import anything, which rules out every fixture that needs output.
has_packages() {
    docker run --rm --entrypoint /bin/sh "$image" -c \
        '[ -d "$HOME/.rux/packages" ] && [ -n "$(ls -A "$HOME/.rux/packages" 2>/dev/null)" ]' \
        >/dev/null 2>&1
}

# Writes a job directory holding the manifest, the source, and standard input.
write_job() {
    local name="$1" source="$2" dependencies="${3:-}" stdin="${4:-}"
    local job="$work/$name"

    rm -rf -- "$job"
    mkdir -p "$job/Src"
    {
        printf '[Manifest]\nVersion = 1\nMinRux = "0.4.0"\n\n'
        printf '[Package]\nName = "Playground"\nVersion = "0.0.0"\nType = "Program"\n'
        if [[ -n "$dependencies" ]]; then
            printf '\n[Dependencies]\n%s\n' "$dependencies"
        fi
    } > "$job/Rux.toml"
    printf '%s' "$source" > "$job/Src/Main.rux"
    printf '%s' "$stdin" > "$job/stdin"
    echo "$job"
}

# Runs one job under the full production flag set and prints the framed output.
run_job() {
    local job="$1" mode="$2" profile="$3" nonce="$4"

    docker run --rm \
        --network=none \
        --read-only \
        --cap-drop=ALL \
        --security-opt=no-new-privileges \
        --memory="$MEMORY_BYTES" \
        --memory-swap="$MEMORY_BYTES" \
        --cpus="$CPU" \
        --pids-limit="$PIDS" \
        --mount "type=bind,source=$job,target=/job,readonly" \
        --tmpfs "/work:rw,exec,nosuid,nodev,mode=1777,size=$TMPFS_BYTES" \
        --workdir=/work \
        "$image" "$mode" "$profile" "$nonce" \
        "$COMPILE_SECONDS" "$RUN_SECONDS" "$MAX_OUTPUT" 2>/dev/null
}

# Extracts one framed section, so assertions read the same way the server does.
section() {
    local output="$1" nonce="$2" name="$3"
    awk -v sentinel="${SEPARATOR}${nonce}:" -v want="$name" '
        index($0, sentinel) == 1 {
            current = substr($0, length(sentinel) + 1)
            sub(/\r$/, "", current)
            next
        }
        current == want { print }
    ' <<< "$output"
}

check() {
    local description="$1" condition="$2"
    if [[ "$condition" == "pass" ]]; then
        echo "  ok    $description"
    else
        echo "  FAIL  $description" >&2
        failures=$((failures + 1))
    fi
}

assert_contains() {
    local haystack="$1" needle="$2" description="$3"
    if [[ "$haystack" == *"$needle"* ]]; then
        check "$description" pass
    else
        check "$description" fail
    fi
}

echo "Testing $image"

# --- Fixtures that need no imports -----------------------------------------

echo "hello world builds and reports a zero build status"
job=$(write_job ok 'func Main() -> int {
    return 0;
}
')
output=$(run_job "$job" build debug 0000000000000000000000000000000a)
status=$(section "$output" 0000000000000000000000000000000a status)
assert_contains "$status" 'build_exit=0' 'a valid program builds'
assert_contains "$status" 'timed_out=0' 'a valid build does not time out'

echo "a syntax error yields diagnostics and a non-zero build status"
job=$(write_job broken 'func Main( -> int {
')
output=$(run_job "$job" build debug 0000000000000000000000000000000b)
status=$(section "$output" 0000000000000000000000000000000b status)
build=$(section "$output" 0000000000000000000000000000000b build)
if [[ "$status" == *'build_exit=0'* ]]; then
    check 'a syntax error fails the build' fail
else
    check 'a syntax error fails the build' pass
fi
if [[ -n "$build" ]]; then
    check 'a syntax error produces diagnostics' pass
else
    check 'a syntax error produces diagnostics' fail
fi

echo "an unknown import is refused rather than silently resolved"
job=$(write_job unknown 'import Sneaky::Thing;

func Main() -> int {
    return 0;
}
')
output=$(run_job "$job" build debug 0000000000000000000000000000000c)
status=$(section "$output" 0000000000000000000000000000000c status)
if [[ "$status" == *'build_exit=0'* ]]; then
    check 'an undeclared import fails the build' fail
else
    check 'an undeclared import fails the build' pass
fi

echo "the framing survives a job whose build times out"
# A compile timeout must be reported, not lost: the status section is what the
# server relies on to tell a slow build from a broken container.
job=$(write_job timeout 'func Main() -> int {
    return 0;
}
')
output=$(COMPILE_SECONDS=0 run_job "$job" build debug 0000000000000000000000000000000d || true)
status=$(section "$output" 0000000000000000000000000000000d status)
if [[ -n "$status" ]]; then
    check 'a status section is always emitted' pass
else
    check 'a status section is always emitted' fail
fi

# --- Containment, verified with the container's own tools -------------------

echo "the container has no network beyond loopback"
if docker run --rm --network=none --entrypoint /bin/sh "$image" -c \
        '[ "$(ls /sys/class/net | tr -d "[:space:]")" = "lo" ]' >/dev/null 2>&1; then
    check 'only loopback is present' pass
else
    check 'only loopback is present' fail
fi

echo "the root filesystem is read-only"
if docker run --rm --read-only --tmpfs "/work:rw,exec,mode=1777,size=$TMPFS_BYTES" \
        --entrypoint /bin/sh "$image" -c 'touch /root-probe' >/dev/null 2>&1; then
    check 'the root filesystem rejects writes' fail
else
    check 'the root filesystem rejects writes' pass
fi

echo "the container does not run as root"
identity=$(docker run --rm --entrypoint /bin/sh "$image" -c 'id -u')
if [[ "$identity" != "0" ]]; then
    check "the image runs as a non-root user (uid $identity)" pass
else
    check 'the image runs as a non-root user' fail
fi

echo "a fork bomb is stopped by the pid limit"
if docker run --rm --network=none --read-only --pids-limit=16 \
        --memory="$MEMORY_BYTES" --memory-swap="$MEMORY_BYTES" \
        --tmpfs "/work:rw,exec,mode=1777,size=$TMPFS_BYTES" --entrypoint /bin/sh "$image" -c \
        'i=0; while [ $i -lt 200 ]; do sleep 30 & i=$((i+1)); done' >/dev/null 2>&1; then
    check 'the pid limit stops runaway process creation' fail
else
    check 'the pid limit stops runaway process creation' pass
fi

echo "an oversized allocation is stopped by the memory limit"
if docker run --rm --network=none --read-only --memory=64m --memory-swap=64m \
        --tmpfs "/work:rw,exec,mode=1777,size=$TMPFS_BYTES" --entrypoint /bin/sh "$image" -c \
        'exec head -c 200000000 /dev/zero > /work/fill' >/dev/null 2>&1; then
    # Filling the tmpfs is charged to the memory cgroup, so this must not
    # succeed at 64 MiB even though the file itself would fit the tmpfs.
    check 'the memory limit stops an oversized allocation' fail
else
    check 'the memory limit stops an oversized allocation' pass
fi

echo "no network tool survives into the final image"
for tool in curl wget apt-get; do
    if docker run --rm --entrypoint /bin/sh "$image" -c "command -v $tool" >/dev/null 2>&1; then
        check "$tool is absent from the image" fail
    else
        check "$tool is absent from the image" pass
    fi
done

# --- Fixtures that need the standard packages -------------------------------

if has_packages; then
    echo "a program's output is returned, truncated, and cannot forge a section"
    nonce=0000000000000000000000000000000e
    job=$(write_job hello 'import Io::Print;

func Main() -> int {
    Print("hello\n");
    return 0;
}
' 'Io = { Namespace = "Rux", Version = "*" }')
    output=$(run_job "$job" run debug "$nonce")
    assert_contains "$(section "$output" "$nonce" stdout)" 'hello' 'stdout is returned'

    # A program printing a sentinel with the wrong nonce must not open a section.
    forger=$(write_job forge 'import Io::Print;

func Main() -> int {
    Print("\x1e00000000000000000000000000000000:stderr\nforged\n");
    return 0;
}
' 'Io = { Namespace = "Rux", Version = "*" }')
    output=$(run_job "$forger" run debug "$nonce")
    if [[ -z "$(section "$output" "$nonce" stderr)" ]]; then
        check 'a guessed sentinel cannot forge a section' pass
    else
        check 'a guessed sentinel cannot forge a section' fail
    fi
else
    echo "SKIP  fixtures needing the standard packages: the image has no seeded"
    echo "      package cache, so no program can import anything. Rebuild with"
    echo "      build-image.sh <version> <sha256> \"Io ...\" once the package"
    echo "      registry the compiler fetches from is reachable."
    skipped=$((skipped + 4))
fi

echo
if [[ "$failures" -gt 0 ]]; then
    echo "$failures check(s) failed" >&2
    exit 1
fi
echo "All image checks passed${skipped:+ ($skipped skipped)}"
