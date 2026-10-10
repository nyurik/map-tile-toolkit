#!/usr/bin/env bash
# Count the slicing benchmarks' CPU instructions into a Markdown report (the CI PR comment), compared
# with a base commit if given. Run from the repository root, usually as `just ci-bench`.
#
# Usage: benches/ci-bench.sh [OUTPUT] [BASE_REF]   (OUTPUT defaults to target/bench/summary.md)
#
# Needs `valgrind` and `python3`; installs the `gungraun-runner` each side needs. Skips the `geo`
# baselines, which take most of an hour under Valgrind.
set -euo pipefail
output="${1:-target/bench/summary.md}"
base_ref="${2:-}"
out_dir="$(realpath -m "$(dirname "$output")")"
mkdir -p "$out_dir"
rm -f "$out_dir"/head.jsonl "$out_dir"/base.jsonl "$out_dir"/base.log
# Both sides are counted from sibling worktrees outside this one, so their paths have the same
# length: the program's path shifts the stack and heap layout, which moves malloc's instruction
# counts by a percent or two between identical builds. (Outside this checkout, cargo does not see
# nested packages; each builds into its own target directory, as a shared one would make cargo take
# one checkout's binary for the other's.)
work="$(mktemp -d)"
head_src="$work/head"
base_src="$work/base"
trap 'for w in "$head_src" "$base_src"; do git worktree remove --force "$w" 2>/dev/null || true; done' EXIT
git worktree add --quiet --detach "$head_src" HEAD
# Count uncommitted changes too (CI has none).
if ! git diff --quiet HEAD; then git diff HEAD --binary | git -C "$head_src" apply; fi
# The benchmark functions of the checkout in `$1`, in order, without the `geo` baselines.
functions() {
    (cd "$1" && cargo bench --bench slicing -- --list) \
        | sed -n 's/^slicing::slicing::\([^:]*\)::.*: benchmark$/\1/p' | awk '!seen[$0]++' | grep -v '_geo$'
}
# Build the checkout in `$1` against a `gungraun-runner` of the gungraun version it depends on (the two
# must match), installing that runner if needed.
use_runner() {
    local ver root
    ver=$(cd "$1" && cargo metadata --format-version 1 | python3 -c \
        'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "gungraun"))')
    root="${CARGO_HOME:-$HOME/.cargo}/gungraun-runner/$ver"
    if [ ! -x "$root/bin/gungraun-runner" ]; then
        cargo install --quiet gungraun-runner --version "=$ver" --root "$root"
    fi
    export GUNGRAUN_RUNNER="$root/bin/gungraun-runner"
}
# Count the functions `$2..` of the checkout in `$1`, as gungraun's JSON lines on stdout.
count() {
    local src="$1"; shift
    for f in "$@"; do
        (cd "$src" && cargo bench --bench slicing -- "slicing::slicing::$f::*" --output-format json) || return 1
    done
}

base_note=""
if [ -n "$base_ref" ]; then
    base="$(git rev-parse --short "$base_ref")"
    git worktree add --quiet --detach "$base_src" "$base_ref"
    use_runner "$base_src"
    # Only the toolkit's slicers: the baselines are the same code on both sides.
    slicers=$(use_runner "$head_src" && functions "$head_src" | grep -v '_stripe$')
    # Count the base with this commit's benchmark and workloads, so both measure the same work.
    rm -rf "$base_src/benches" "$base_src/tests/support"
    cp -r "$head_src/benches" "$base_src/benches"
    cp -r "$head_src/tests/support" "$base_src/tests/support"
    if count "$base_src" $slicers > "$out_dir/base.jsonl" 2>> "$out_dir/base.log"; then
        base_note="Compared with the base commit $base, counted with this commit's benchmark."
    else
        # It may need API or dev-dependencies the base lacks: fall back to the base's own benchmark.
        rm -rf "$base_src/benches" "$base_src/tests/support"
        git -C "$base_src" checkout --quiet -- . 2>> "$out_dir/base.log" || true
        own=$(functions "$base_src" 2>> "$out_dir/base.log" | grep -v '_stripe$' || true)
        if [ -n "$own" ] && count "$base_src" $own > "$out_dir/base.jsonl" 2>> "$out_dir/base.log"; then
            base_note="Compared with the base commit $base, counted with its own benchmark, as this commit's does not build there (see the job log): a changed workload also shows as a change."
        else
            rm -f "$out_dir/base.jsonl"
            base_note="The base commit $base could not be benchmarked; see the job log."
        fi
        echo "::warning::Could not count the base commit with this commit's benchmark:" >&2
        tail -n 30 "$out_dir/base.log" >&2
    fi
fi

use_runner "$head_src"
count "$head_src" $(functions "$head_src") > "$out_dir/head.jsonl"
{
    echo "## Slicing instruction counts"
    echo
    echo "Commit $(git rev-parse --short HEAD), $(lscpu | sed -n 's/^Model name: *//p')."
    echo "Counted with valgrind's callgrind (via gungraun), so the counts are exact and repeat on any CPU with the same features."
    if [ -n "$base_note" ]; then echo "$base_note"; fi
    echo
    if [ -f "$out_dir/base.jsonl" ]; then
        python3 "$head_src/benches/report.py" "$out_dir/head.jsonl" "$out_dir/base.jsonl"
    else
        python3 "$head_src/benches/report.py" "$out_dir/head.jsonl"
    fi
} > "$output"
cat "$output" >> "${GITHUB_STEP_SUMMARY:-/dev/stdout}"
