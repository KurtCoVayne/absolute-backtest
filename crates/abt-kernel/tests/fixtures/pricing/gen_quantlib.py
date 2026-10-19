"""Known answers from QuantLib 1.44 for tests/pricing.rs, an independent
full-precision oracle:

- quantlib.csv: European options from AnalyticEuropeanEngine on a
  BlackScholesMertonProcess. Model "bsm": price, delta, gamma, vega, theta,
  rho. Model "b76" (Black-76 on a futures price F): the same engine with
  S = F and q = r, which leaves F = S e^{(r-q)tau} fixed; Black's rho (dV/dr
  with F fixed) is then the engine's rho() + dividendRho().
- american.csv: American calls with q > 0 and puts, from
  QdFpAmericanEngine with highPrecisionScheme() (Andersen-Lake-Offengenden
  fixed-point iteration, accurate to ~1e-10 relative).

Conventions (the SWIG wrappers in the wheel carry no units, so they are
checked here against finite differences of NPV, in assert_conventions):
theta() is dV/dt per year (= -dV/dtau, and thetaPerDay() * 365), vega() per
unit sigma, rho() and dividendRho() per unit rate.

Every curve is flat and continuously compounded, the day count
Actual365Fixed, and every expiry a whole number of days N after the
evaluation date, so tau = N/365 exactly; the CSV records N and the test
recomputes tau = N as f64 / 365.0, the same double.

The grid is deterministic: the full product of the parameter lists below,
thinned by a fixed modulus of the running index (every value of every list
survives).

Run with the QuantLib wheel unpacked into a directory of its own:
    python3 -I gen_quantlib.py /path/to/extracted/ql
"""
import csv
import math
import os
import sys

sys.path.insert(0, sys.argv[1])
import QuantLib as ql  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
TODAY = ql.Date(1, 1, 2020)
ql.Settings.instance().evaluationDate = TODAY
DC = ql.Actual365Fixed()

SPOTS = [60.0, 75.0, 90.0, 97.5, 100.0, 103.0, 110.0, 125.0, 140.0]
DAYS = [1, 7, 30, 91, 182, 365, 730, 1825]
RATES = [0.0, 0.03, 0.08]
YIELDS = [0.0, 0.02, 0.08]
VOLS = [0.05, 0.2, 0.5, 1.0]
RIGHTS = ["call", "put"]


def option(s, k, days, r, q, vol, right, american=False):
    """A VanillaOption with its quotes, so that bumps can reprice it."""
    kind = ql.Option.Call if right == "call" else ql.Option.Put
    payoff = ql.PlainVanillaPayoff(kind, k)
    expiry = TODAY + days
    exercise = ql.AmericanExercise(TODAY, expiry) if american else ql.EuropeanExercise(expiry)
    opt = ql.VanillaOption(payoff, exercise)
    quotes = {name: ql.SimpleQuote(v) for name, v in (("s", s), ("r", r), ("q", q), ("vol", vol))}
    rts = ql.YieldTermStructureHandle(ql.FlatForward(TODAY, ql.QuoteHandle(quotes["r"]), DC, ql.Continuous))
    qts = ql.YieldTermStructureHandle(ql.FlatForward(TODAY, ql.QuoteHandle(quotes["q"]), DC, ql.Continuous))
    vts = ql.BlackVolTermStructureHandle(ql.BlackConstantVol(TODAY, ql.NullCalendar(), ql.QuoteHandle(quotes["vol"]), DC))
    process = ql.BlackScholesMertonProcess(ql.QuoteHandle(quotes["s"]), qts, rts, vts)
    if american:
        opt.setPricingEngine(ql.QdFpAmericanEngine(process, ql.QdFpAmericanEngine.highPrecisionScheme()))
    else:
        opt.setPricingEngine(ql.AnalyticEuropeanEngine(process))
    return opt, quotes


def assert_conventions():
    """vega per unit sigma, rho per unit rate (q fixed), theta = -dV/dtau per year."""
    s, k, days, r, q, vol = 103.0, 100.0, 182, 0.03, 0.02, 0.2
    opt, quotes = option(s, k, days, r, q, vol, "call")
    h = 1e-6

    def bumped(name, value):
        quotes[name].setValue(value + h)
        up = opt.NPV()
        quotes[name].setValue(value - h)
        dn = opt.NPV()
        quotes[name].setValue(value)
        return (up - dn) / (2 * h)

    assert abs(bumped("vol", vol) - opt.vega()) < 1e-6 * abs(opt.vega())
    assert abs(bumped("r", r) - opt.rho()) < 1e-6 * abs(opt.rho())
    assert abs(bumped("q", q) - opt.dividendRho()) < 1e-6 * abs(opt.dividendRho())
    # theta: one day less to expiry, per year.
    later = option(s, k, days - 1, r, q, vol, "call")[0]
    earlier = option(s, k, days + 1, r, q, vol, "call")[0]
    fd = -(earlier.NPV() - later.NPV()) / (2 / 365)
    assert abs(fd - opt.theta()) < 1e-3 * abs(opt.theta()), (fd, opt.theta())
    assert abs(opt.thetaPerDay() * 365 - opt.theta()) < 1e-12 * abs(opt.theta())


def fmt(x):
    return repr(float(x))


def european_rows():
    rows = []
    n = 0
    for s in SPOTS:
        for days in DAYS:
            for r in RATES:
                for q in YIELDS:
                    for vol in VOLS:
                        for right in RIGHTS:
                            n += 1
                            if n % 11 == 0:
                                opt = option(s, 100.0, days, r, q, vol, right)[0]
                                rows.append(["bsm", right, fmt(s), "100.0", days, fmt(r), fmt(q), fmt(vol),
                                             fmt(opt.NPV()), fmt(opt.delta()), fmt(opt.gamma()), fmt(opt.vega()), fmt(opt.theta()), fmt(opt.rho())])
                            if q == 0.0 and n % 7 == 0:
                                # Black-76: the futures price F = s, q = r.
                                opt = option(s, 100.0, days, r, r, vol, right)[0]
                                rows.append(["b76", right, fmt(s), "100.0", days, fmt(r), fmt(r), fmt(vol),
                                             fmt(opt.NPV()), fmt(opt.delta()), fmt(opt.gamma()), fmt(opt.vega()), fmt(opt.theta()),
                                             fmt(opt.rho() + opt.dividendRho())])
    return rows


AM_SPOTS = [70.0, 85.0, 95.0, 100.0, 105.0, 115.0, 130.0]
AM_DAYS = [7, 30, 91, 182, 365, 730, 1825]
AM_RATES = [0.0, 0.02, 0.05, 0.08]
AM_YIELDS = [0.0, 0.01, 0.03, 0.06, 0.1]
AM_VOLS = [0.1, 0.2, 0.35, 0.6, 1.0]


def american_rows():
    rows = []
    n = 0
    for s in AM_SPOTS:
        for days in AM_DAYS:
            for r in AM_RATES:
                for q in AM_YIELDS:
                    for vol in AM_VOLS:
                        for right in RIGHTS:
                            if right == "call" and q == 0.0:
                                continue  # never exercised early: equals the European
                            n += 1
                            if n % 5:
                                continue
                            opt = option(s, 100.0, days, r, q, vol, right, american=True)[0]
                            rows.append([right, fmt(s), "100.0", days, fmt(r), fmt(q), fmt(vol), fmt(opt.NPV())])
    return rows


def write(name, header, rows):
    with open(os.path.join(HERE, name), "w", newline="") as fh:
        w = csv.writer(fh, lineterminator="\n")
        w.writerow(header)
        w.writerows(rows)
    print(name, len(rows), "rows")


if __name__ == "__main__":
    print("QuantLib", ql.__version__)
    assert_conventions()
    write("quantlib.csv", ["model", "right", "s", "k", "days", "r", "q", "sigma", "price", "delta", "gamma", "vega", "theta", "rho"], european_rows())
    write("american.csv", ["right", "s", "k", "days", "r", "q", "sigma", "qdfp"], american_rows())
