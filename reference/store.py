"""A run read back from the store it ran over (M-9: the store is the dump).

The binding's data and the kernel's outputs are read through the data
layer's query interface (`abt data query --store STORE ... --parquet`) and
written in the layout `engine.py` replays and `diff.py` compares:

    config.json            the resolved execution block (the binding's
                           `configuration`), the price and volume relations,
                           the instruments by name, the decision bars
    data/<relation>.parquet  the price and volume relations, `split`,
                           `dividend` and `delisted`, at the versions the
                           binding names, each instrument by name
    decisions.parquet, fills.parquet, dropped.parquet, actions.parquet,
    nav.parquet, final.parquet
                           the kernel's, from kernel.<run>.decisions,
                           .events, .book, .holdings and .report

An instrument's name is its `vendor_id` key, as the kernel names it.

    python3 reference/store.py STORE RUN OUT_DIR [--abt PATH]
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile

import pandas as pd


def abt_path(explicit: str | None = None) -> str:
    """The `abt` binary: the one named, `$ABT`, else the one on the PATH."""
    return explicit or os.environ.get("ABT") or "abt"


class Store:
    """A store read through `abt data query`."""

    def __init__(self, store: str, abt: str | None = None):
        self.store = store
        self.abt = abt_path(abt)

    def query(self, text: str, view: str | None = None) -> pd.DataFrame:
        with tempfile.TemporaryDirectory() as tmp:
            out = os.path.join(tmp, "answer.parquet")
            cmd = [self.abt, "data", "query", "--store", self.store, text, "--parquet", out]
            if view:
                cmd += ["--view", view]
            p = subprocess.run(cmd, capture_output=True, text=True)
            if p.returncode != 0:
                raise RuntimeError(f"abt data query {text!r}: {p.stderr.strip() or p.stdout.strip()}")
            return pd.read_parquet(out) if os.path.exists(out) else pd.DataFrame()


def export(store: str, run: str, out: str, abt: str | None = None) -> None:
    s = Store(store, abt)
    os.makedirs(os.path.join(out, "data"), exist_ok=True)
    binding = s.query(f"kernel.{run}.binding")
    if binding.empty:
        raise RuntimeError(f"no run {run} in {store}")
    versions = dict(zip(binding["relation"], binding["version"]))
    cfg = json.loads(binding["configuration"].iloc[0])

    # The instruments by name: the first key of each id, as the kernel takes it.
    names: dict[int, str] = {}
    if "vendor_id" in versions:
        for _, r in s.query("vendor_id", versions["vendor_id"]).iterrows():
            names.setdefault(int(r["id"]), str(r["key"]))

    def named(frame: pd.DataFrame, col: str) -> pd.DataFrame:
        frame = frame.copy()
        frame[col] = [names.get(int(v), str(v)) if pd.notna(v) else "" for v in frame[col]]
        return frame

    def stored(rel: str) -> str | None:
        """The binding's stored relation of that name (`equity.close` for `close`)."""
        found = [r for r in versions if "." in r and r.split(".")[-1] == rel and not r.startswith("kernel.")]
        return found[0] if found else None

    # The price and volume relations the executor fills at: the execution
    # block's, else `close` and `volume` where the program reads them, as
    # the kernel takes them.
    price = cfg.get("price_relation") or ("close" if stored("close") else None)
    volume = cfg.get("volume_relation") or ("volume" if stored("volume") else None)

    # The data the reference replays the decisions over, at the binding's versions.
    for rel in dict.fromkeys(r for r in [price, volume, "split", "dividend", "delisted"] if r):
        name = stored(rel)
        if name is None:
            continue
        frame = s.query(name, versions[name])
        frame = frame.drop(columns=[c for c in ("r", "seq") if c in frame.columns])
        if not frame.empty:
            frame = named(frame, frame.columns[0])
        frame.to_parquet(os.path.join(out, "data", f"{rel}.parquet"), index=False)

    # The kernel's outputs.
    book = s.query(f"kernel.{run}.book").sort_values("t", kind="stable")
    config = {
        "exec": cfg,
        "price_relation": price,
        "volume_relation": volume,
        "symbols": sorted(set(names.values())),
        "bars": [_stamp(t) for t in book["t"]],
    }
    with open(os.path.join(out, "config.json"), "w") as f:
        json.dump(config, f, indent=2)

    decisions = s.query(f"kernel.{run}.decisions").sort_values("seq", kind="stable")
    decisions = named(decisions, "instrument").rename(columns={"instrument": "equity"})
    decisions[["t", "equity", "ctor", "amount", "order", "rule"]].to_parquet(os.path.join(out, "decisions.parquet"), index=False)

    events = s.query(f"kernel.{run}.events").sort_values("seq", kind="stable")
    events = named(events, "instrument").rename(columns={"instrument": "equity"})
    fills = events[events["kind"].isin(["fill", "partial_fill"])].copy()
    fills["at_last_price"] = [bool(json.loads(d).get("at_last_price", False)) if isinstance(d, str) and d else False for d in fills["detail"]]
    cols = ["t", "equity", "quantity", "price", "commission", "fee", "slippage", "impact", "participation", "at_last_price", "partial", "forced"]
    fills[cols].to_parquet(os.path.join(out, "fills.parquet"), index=False)
    dropped = events[events["kind"].isin(["dropped", "expired_order", "rejected"])]
    dropped[["t", "equity", "reason"]].to_parquet(os.path.join(out, "dropped.parquet"), index=False)
    actions = events[events["kind"].isin(["split", "dividend_paid", "delisted", "dividend_reinvested"])]
    actions = actions.rename(columns={"kind": "action", "cash_flow": "cash"})
    actions[["t", "equity", "action", "detail", "cash"]].to_parquet(os.path.join(out, "actions.parquet"), index=False)

    nav = book.rename(columns={"nav": "equity"})
    nav[["t", "equity", "cash", "gross", "net", "leverage"]].to_parquet(os.path.join(out, "nav.parquet"), index=False)

    # The end of the run: its cash, and the positions held at its last bar
    # (none when the book is empty there: `holdings` has a row per position
    # held at a bar, so its own last bar may be an earlier one).
    report = s.query(f"kernel.{run}.report")
    values = dict(zip(report["key"], report["value"]))
    keys, nums = ["cash"], [float(json.loads(values["final_cash"]))]
    holdings = s.query(f"kernel.{run}.holdings")
    if not holdings.empty and not book.empty:
        last = holdings[holdings["t"] == book["t"].max()]
        for _, r in named(last, "instrument").sort_values("instrument").iterrows():
            if float(r["position"]) != 0.0:
                keys.append(r["instrument"])
                nums.append(float(r["position"]))
    pd.DataFrame({"key": keys, "value": nums}).to_parquet(os.path.join(out, "final.parquet"), index=False)


def _stamp(t) -> str:
    t = pd.Timestamp(t)
    if t.hour == 0 and t.minute == 0 and t.second == 0:
        return t.strftime("%Y-%m-%d")
    return t.strftime("%Y-%m-%dT%H:%M:%S")


def main(argv: list[str]) -> int:
    abt = None
    if "--abt" in argv:
        i = argv.index("--abt")
        abt = argv[i + 1]
        argv = argv[:i] + argv[i + 2 :]
    if len(argv) != 3:
        print(__doc__)
        return 2
    export(argv[0], argv[1], argv[2], abt)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
