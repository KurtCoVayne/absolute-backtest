"""Compare a kernel dump with the reference executor's replay of it
(data-bundle doc, section 8): fills identical in quantity (exactly under
whole lots, to floating tolerance under fractional ones) and in price to
floating tolerance, the book identical at the end, the NAV within floating
tolerance at every bar, the same dropped count.

    python3 reference/diff.py DUMP_DIR [--tolerance 1e-9]

Exit status 0 when they agree; 1 with every disagreement listed.
"""

from __future__ import annotations

import os
import sys

import pandas as pd

from engine import Halt, Reference, read_rows


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
    if not argv:
        print(__doc__)
        return 2
    tol = 1e-9
    if "--tolerance" in argv:
        tol = float(argv[argv.index("--tolerance") + 1])
    problems = compare(argv[0], tol)
    if problems:
        for p in problems:
            print(p)
        print(f"{len(problems)} disagreement(s)")
        return 1
    print("kernel and reference agree")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
