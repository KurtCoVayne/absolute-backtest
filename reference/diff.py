"""Compare a run with the reference executor's replay of it (data-bundle doc,
section 8): fills identical in quantity (exactly under whole lots, to
floating tolerance under fractional ones) and in price to floating
tolerance, the book identical at the end, the NAV within floating tolerance
at every bar, the same dropped count.

The run is read back from the store it ran over (`store.py`; M-9: the
store is the dump), its data and outputs through `abt data query`:

    python3 reference/diff.py STORE RUN [--abt PATH] [--tolerance 1e-9]

Exit status 0 when they agree; 1 with every disagreement listed.
"""

from __future__ import annotations

import os
import sys
import tempfile

import pandas as pd

from engine import Halt, Reference, read_rows
from store import export


def close(a: float, b: float, tol: float) -> bool:
    return abs(a - b) <= tol * max(1.0, abs(a), abs(b))


def compare(directory: str, tol: float = 1e-9) -> list[str]:
    problems: list[str] = []
    try:
        ref = Reference(directory).run()
    except Halt as h:
        return [f"the reference halted: {h}"]
    fills, nav, final = ref.frames()
    k_fills = read_rows(os.path.join(directory, "fills.parquet"))
    # A whole-lot quantity is an integer and must be identical; a fractional
    # one is a real number sized from the book and is held to tolerance.
    whole = ref.cfg.lot_whole
    k_nav = read_rows(os.path.join(directory, "nav.parquet"))
    k_final = read_rows(os.path.join(directory, "final.parquet"))
    k_dropped = read_rows(os.path.join(directory, "dropped.parquet"))
    if len(fills) != len(k_fills):
        problems.append(f"fills: kernel {len(k_fills)}, reference {len(fills)}")
    for i in range(min(len(fills), len(k_fills))):
        a = k_fills.iloc[i]
        b = fills.iloc[i]
        same_qty = float(a["quantity"]) == float(b["quantity"]) if whole else close(float(a["quantity"]), float(b["quantity"]), tol)
        if a["t"] != b["t"] or a["equity"] != b["equity"] or not same_qty:
            problems.append(f"fill {i + 1}: kernel {a['t']} {a['equity']} {a['quantity']} vs reference {b['t']} {b['equity']} {b['quantity']}")
            continue
        for col in ("price", "commission", "fee", "slippage", "impact"):
            if not close(float(a[col]), float(b[col]), tol):
                problems.append(f"fill {i + 1} ({a['t']} {a['equity']}): {col} kernel {a[col]} vs reference {b[col]}")
        for col in ("at_last_price", "partial", "forced"):
            if str(a[col]).lower() != str(b[col]).lower():
                problems.append(f"fill {i + 1} ({a['t']} {a['equity']}): {col} kernel {a[col]} vs reference {b[col]}")
    if len(nav) != len(k_nav):
        problems.append(f"bars: kernel {len(k_nav)}, reference {len(nav)}")
    for i in range(min(len(nav), len(k_nav))):
        a = k_nav.iloc[i]
        b = nav.iloc[i]
        if a["t"] != b["t"]:
            problems.append(f"bar {i + 1}: kernel {a['t']} vs reference {b['t']}")
            continue
        for col in ("equity", "cash", "gross", "net"):
            if not close(float(a[col]), float(b[col]), tol):
                problems.append(f"bar {a['t']}: {col} kernel {a[col]} vs reference {b[col]}")
    k_final_map = {r["key"]: float(r["value"]) for _, r in k_final.iterrows()}
    for key in sorted(set(k_final_map) | set(final)):
        a = k_final_map.get(key)
        b = final.get(key)
        if a is None or b is None or not close(a, b, tol):
            problems.append(f"final {key}: kernel {a} vs reference {b}")
    if len(k_dropped) != len(ref.dropped):
        problems.append(f"dropped: kernel {len(k_dropped)}, reference {len(ref.dropped)}")
    return problems


def main(argv: list[str]) -> int:
    def option(name: str):
        nonlocal argv
        if name not in argv:
            return None
        i = argv.index(name)
        value = argv[i + 1]
        argv = argv[:i] + argv[i + 2 :]
        return value

    tol = float(option("--tolerance") or 1e-9)
    abt = option("--abt")
    if len(argv) != 2:
        print(__doc__)
        return 2
    store, run = argv
    with tempfile.TemporaryDirectory() as tmp:
        export(store, run, tmp, abt)
        problems = compare(tmp, tol)
    if problems:
        for p in problems:
            print(p)
        print(f"{len(problems)} disagreement(s)")
        return 1
    print("kernel and reference agree")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
