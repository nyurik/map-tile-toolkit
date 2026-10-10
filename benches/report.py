#!/usr/bin/env python3
"""Render the slicing benchmarks' instruction counts as Markdown tables, for the CI PR comment.

Reads gungraun's JSON output (`cargo bench --bench slicing -- --output-format json`, one benchmark
per line) for this commit, and optionally for a base commit. Per geometry type it prints the counts,
each compared with the lowest count of its operation in that row (the toolkit's slicer and its
baselines), and, given a base, each slicer's change from the base commit.

Usage: report.py HEAD_JSONL [BASE_JSONL]
"""

import json
import sys

# The toolkit's benchmark functions per table; each is an operation, its baselines are its columns.
TABLES = [("Polylines", ["all", "one"]), ("Polygons", ["polygon_all", "polygon_one"])]
# A baseline function is named after its operation with one of these suffixes (`benches/slicing.rs`).
BASELINES = ["stripe", "geo"]

NOTES = """\
Percentages in parentheses: how much more than the lowest count of the same operation in its row.

Baselines (`benches/baseline/mod.rs`) clip the same inputs into the same tiles in `f64`, computing \
intersection points and dropping each tile's output, where the toolkit keeps the original vertices and \
stores every tile's output. `stripe`: a geojson-vt-style stripe clipper, the realistic target. `geo`: \
`geo`'s `BooleanOps`, an upper bound; too slow to count in CI, `just bench geo` counts it.
"""
BASE_NOTES = """\

Change from the base commit: `=` same count, `~0%` within 0.05%, `new` not in the base. Only the \
toolkit's slicers are counted on the base: the baselines are the same code.
"""


def instructions(profile):
    """A benchmark's instruction count, from the JSON of gungraun 0.20+ (schema 7) or 0.19 (schema 6),
    so that a base commit on an older gungraun still compares."""
    if "data" in profile:
        return int(profile["data"]["total"]["metrics"]["Ir"]["values"]["new"])
    (count,) = profile["summaries"]["total"]["summary"]["Callgrind"]["Ir"]["metrics"]["Left"].values()
    return int(count)


def load(path):
    """`{(function, id): instructions}`, and each function's ids in the order they were counted."""
    counts, order = {}, {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            if not line.startswith("{"):
                continue
            bench = json.loads(line)
            counts[bench["function_name"], bench["id"]] = instructions(bench["profiles"][0])
            order.setdefault(bench["function_name"], []).append(bench["id"])
    return counts, order


def compact(n):
    """A count to three significant digits, e.g. `761k`, `41.2M`."""
    for div, unit in ((1e9, "G"), (1e6, "M"), (1e3, "k")):
        if n >= div:
            return f"{n / div:.3g}{unit}"
    return str(n)


def above_best(n, best):
    """How much more than the lowest count `best` the count `n` is: `best`, `+12%`, or `85×`."""
    if n == best:
        return "best"
    pct = (n / best - 1) * 100
    if pct >= 1000:
        return f"{n / best:.0f}×"
    return f"+{pct:.2g}%" if pct < 10 else f"+{pct:.0f}%"


def change(new, old):
    """The change from the base count `old`: `=`, `~0%` (within 0.05%), or a signed percentage."""
    if old is None:
        return "new"
    if new == old:
        return "="
    pct = (new - old) / old * 100
    return "~0%" if abs(pct) < 0.05 else f"{pct:+.1f}%"


def table(header, rows):
    """A Markdown table, its first column left-aligned and the others right-aligned."""
    widths = [max(len(row[i]) for row in [header, *rows]) for i in range(len(header))]

    def line(cells):
        padded = [c.ljust(w) if i == 0 else c.rjust(w) for i, (c, w) in enumerate(zip(cells, widths))]
        return "| " + " | ".join(padded) + " |"

    rule = "|" + "|".join("-" * (w + 2) if i == 0 else "-" * (w + 1) + ":" for i, w in enumerate(widths))
    return "\n".join([line(header), rule + "|", *map(line, rows)])


def main():
    head, order = load(sys.argv[1])
    base = load(sys.argv[2])[0] if len(sys.argv) > 2 else None

    listed = {op for _, ops in TABLES for op in ops}
    known = listed | {f"{op}_{b}" for op in listed for b in BASELINES}
    other = [f for f in order if f not in known]
    for title, ops in TABLES + ([("Other", other)] if other else []):
        # Per operation: its function, then those of its baselines that were counted.
        columns = [[op] + [f"{op}_{b}" for b in BASELINES if f"{op}_{b}" in order] for op in ops if op in order]
        if not columns:
            continue
        ids = []
        for f in (f for funcs in columns for f in funcs):
            ids += [i for i in order[f] if i not in ids]

        rows = []
        for i in ids:
            row = [i]
            for funcs in columns:
                best = min((head[f, i] for f in funcs if (f, i) in head), default=None)
                row += [f"{compact(head[f, i])} ({above_best(head[f, i], best)})" if (f, i) in head else "n/a"
                        for f in funcs]
            rows.append(row)
        header = ["Input"] + [f.replace("_", " ") for funcs in columns for f in funcs]
        print(f"**{title}**: CPU instructions (lower is better)\n\n{table(header, rows)}\n")

        if base is not None:
            slicers = [funcs[0] for funcs in columns]
            rows = [[i] + [change(head[f, i], base.get((f, i))) if (f, i) in head else "n/a" for f in slicers]
                    for i in ids]
            rows = [row for row in rows if any(c != "n/a" for c in row[1:])]
            header = ["Input"] + [f.replace("_", " ") for f in slicers]
            print(f"**{title}**: CPU instructions, change from the base commit\n\n{table(header, rows)}\n")

    print(NOTES + (BASE_NOTES if base is not None else ""), end="")


if __name__ == "__main__":
    main()
