"""High-precision known answers for tests/pricing.rs, from mpmath (30+
significant digits), independent of the module's double-precision algorithms:

- normal.csv: x, Phi(x), phi(x) and erfcx(x) = exp(x^2) erfc(x), x from -38
  (where Phi underflows) to +9, including the tails;
- quantile.csv: p and Phi^{-1}(p) from p = 1e-300 to 1 - 1e-16;
- bivariate.csv: h, k, rho, P(X <= h, Y <= k) for the standard bivariate
  normal, by the conditional integral of phi(u) Phi((k - rho u)/sqrt(1 - rho^2))
  over u <= h (a positive integrand, so no cancellation), closed forms at
  rho = +-1;
- bs2002.csv: the Bjerksund-Stensland (2002) American price exactly as
  src/functions/american.rs defines it (two-step flat boundary,
  t1 = (sqrt(5) - 1)/2 T, X = I(T), x = min(I(T - t1), X) with h capped at
  0, the put by the put-call transformation, the price at least
  max(intrinsic, European)), evaluated term by term in the paper's form with
  40-digit arithmetic. It checks the module's numerics (logs, grouped
  differences), not the approximation.
- black.csv: Jaeckel's normalised Black call b(x, s) and e^{x/2} - b over
  the four regions of src/functions/iv.rs, for its unit tests;
- implied.csv: European BSM prices and vegas at known volatilities over
  K/F from 0.5 to 2, tau from 1 day to 5 years and sigma from 0.01 to 3, for
  the implied volatility test (prices below 1e-280 are left out).

Every input is a double; mpmath converts it exactly. Values are written with
repr(float(...)), i.e. correctly rounded to the nearest double.

Run with mpmath unpacked from its wheel into a directory of its own:
    python3 -I gen_mpmath.py /path/to/extracted/mpmath [table ...]
where the tables (default: all) are normal, quantile, bivariate, bs2002,
implied and black. The output is deterministic; bs2002 takes ~25 minutes
(each bivariate value is an adaptive quadrature), the rest a few.
"""
import csv
import os
import sys

sys.path.insert(0, sys.argv[1])
import mpmath as mp  # noqa: E402

mp.mp.dps = 40
HERE = os.path.dirname(os.path.abspath(__file__))
HALF = mp.mpf(1) / 2


def f(x):
    return repr(float(x))


# ---------------------------------------------------------------- normal
def normal_rows():
    xs = [-38.0, -37.5, -37.0, -35.0, -30.0, -27.0, -26.6, -26.0, -20.0, -15.5, -12.0, -10.0, -8.25, -7.0]
    xs += [-6.0, -5.0, -4.5, -4.0, -3.5, -3.0, -2.5, -2.0, -1.5, -1.2345, -1.0, -0.75, -0.6629, -0.5, -0.25]
    xs += [-1e-3, -1e-9, 0.0, 1e-9, 0.1, 0.3, 0.5, 0.6629, 0.7, 1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 7.5, 8.0, 9.0]
    # Points just either side of the boundaries of Cody's intervals for erfc(-x/sqrt2):
    # |x|/sqrt2 = 0.46875 and 4.
    for z in (0.46875, 4.0):
        for e in (-1e-12, 1e-12):
            xs.append(-float(mp.sqrt(2) * (z + e)))
    rows = []
    for x in xs:
        X = mp.mpf(x)
        rows.append([f(X), f(mp.ncdf(X)), f(mp.npdf(X)), f(mp.erfc(X) * mp.exp(X * X)) if x > -26.5 else "inf"])
    return rows


# ---------------------------------------------------------------- bivariate
def bvn(h, k, rho):
    """P(X <= h, Y <= k) for a standard bivariate normal with correlation rho."""
    h, k, rho = mp.mpf(h), mp.mpf(k), mp.mpf(rho)
    if rho == 1:
        return mp.ncdf(min(h, k))
    if rho == -1:
        return max(mp.mpf(0), mp.ncdf(h) - mp.ncdf(-k))
    s = mp.sqrt(1 - rho * rho)

    def g(u):
        return mp.npdf(u) * mp.ncdf((k - rho * u) / s)

    pts = {h - mp.mpf("0.01"), h - mp.mpf("0.1"), h - 1, h - 3, h - 10}
    pts |= {mp.mpf(p) for p in (-40, -20, -10, -5, -2, 0, 2, 5)}
    if rho != 0:
        c = k / rho
        pts |= {c - s, c, c + s}
    inner = sorted(p for p in pts if p < h)
    return mp.quad(g, [-mp.inf] + inner + [h])


def bivariate_rows():
    rows = []
    hs = [-8.0, -3.0, -1.5, -0.4, 0.0, 0.7, 2.0, 5.0]
    rhos = [-1.0, -0.999, -0.95, -0.925, -0.9249, -0.8, -0.75, -0.7499, -0.5, -0.3, -0.2999, 0.0, 0.2999, 0.3, 0.5, 0.7499, 0.75]
    rhos += [0.7861513777574233, -0.7861513777574233, 0.9249, 0.925, 0.95, 0.99, 0.999999, 1.0]
    for i, h in enumerate(hs):
        for j, k in enumerate(hs):
            for n, rho in enumerate(rhos):
                # A deterministic thinning of the full product keeps the file small.
                if (i + 2 * j + 3 * n) % 4 != 0 and abs(rho) not in (1.0, 0.925, 0.7861513777574233):
                    continue
                rows.append([f(h), f(k), f(rho), f(bvn(h, k, rho))])
    # Tails and the arguments BS2002 meets.
    for h, k, rho in [(-10.0, -10.0, 0.7861513777574233), (-12.0, -9.0, 0.7861513777574233), (-6.0, -6.0, -0.7861513777574233),
                      (-20.0, -20.0, 0.5), (-5.0, 3.0, -0.95), (3.0, -5.0, 0.95), (-37.0, 0.0, 0.3), (-2.0, -2.0, 0.9999)]:
        rows.append([f(h), f(k), f(rho), f(bvn(h, k, rho))])
    return rows


# ---------------------------------------------------------------- BS2002
def bsm(S, K, T, r, q, v, call):
    w = 1 if call else -1
    vt = v * mp.sqrt(T)
    d1 = (mp.log(S / K) + (r - q) * T) / vt + vt / 2
    d2 = d1 - vt
    return w * (S * mp.exp(-q * T) * mp.ncdf(w * d1) - K * mp.exp(-r * T) * mp.ncdf(w * d2))


def phi_term(S, t, g, H, X, r, b, v):
    lam = -r + g * b + g * (g - 1) * v * v / 2
    kap = 2 * b / (v * v) + 2 * g - 1
    m = b + (g - HALF) * v * v
    d1 = -(mp.log(S / H) + m * t) / (v * mp.sqrt(t))
    d2 = -(mp.log(X * X / (S * H)) + m * t) / (v * mp.sqrt(t))
    return mp.exp(lam * t) * S**g * (mp.ncdf(d1) - (X / S) ** kap * mp.ncdf(d2))


def psi_term(S, T, g, H, X, x, t1, r, b, v):
    m = b + (g - HALF) * v * v
    a1, aT = v * mp.sqrt(t1), v * mp.sqrt(T)
    e1 = (mp.log(S / x) + m * t1) / a1
    e2 = (mp.log(X * X / (S * x)) + m * t1) / a1
    e3 = (mp.log(S / x) - m * t1) / a1
    e4 = (mp.log(X * X / (S * x)) - m * t1) / a1
    f1 = (mp.log(S / H) + m * T) / aT
    f2 = (mp.log(X * X / (S * H)) + m * T) / aT
    f3 = (mp.log(x * x / (S * H)) + m * T) / aT
    f4 = (mp.log(S * x * x / (H * X * X)) + m * T) / aT
    rho = mp.sqrt(t1 / T)
    lam = -r + g * b + g * (g - 1) * v * v / 2
    kap = 2 * b / (v * v) + 2 * g - 1
    return mp.exp(lam * T) * S**g * (bvn(-e1, -f1, rho) - (X / S) ** kap * bvn(-e2, -f2, rho)
                                     - (x / S) ** kap * bvn(-e3, -f3, -rho) + (x / X) ** kap * bvn(-e4, -f4, -rho))


def call2002(S, K, T, r, b, v):
    if b >= r:
        return bsm(S, K, T, r, r - b, v, True)
    t1 = (mp.sqrt(5) - 1) / 2 * T
    beta = (HALF - b / (v * v)) + mp.sqrt((b / (v * v) - HALF) ** 2 + 2 * r / (v * v))
    binf = beta / (beta - 1) * K
    b0 = max(K, r / (r - b) * K)

    def trigger(tau):
        h = -(b * tau + 2 * v * mp.sqrt(tau)) * K * K / ((binf - b0) * b0)
        return b0 + (binf - b0) * (1 - mp.exp(min(h, 0)))

    X = trigger(T)
    x = min(trigger(T - t1), X)
    if S >= X:
        return S - K

    def alpha(y):
        return (y - K) * y ** (-beta)

    return (alpha(X) * S**beta - alpha(X) * phi_term(S, t1, beta, X, X, r, b, v)
            + phi_term(S, t1, 1, X, X, r, b, v) - phi_term(S, t1, 1, x, X, r, b, v)
            - K * phi_term(S, t1, 0, X, X, r, b, v) + K * phi_term(S, t1, 0, x, X, r, b, v)
            + alpha(x) * phi_term(S, t1, beta, x, X, r, b, v) - alpha(x) * psi_term(S, T, beta, x, X, x, t1, r, b, v)
            + psi_term(S, T, 1, x, X, x, t1, r, b, v) - psi_term(S, T, 1, K, X, x, t1, r, b, v)
            - K * psi_term(S, T, 0, x, X, x, t1, r, b, v) + K * psi_term(S, T, 0, K, X, x, t1, r, b, v))


def american(S, K, T, r, q, v, call):
    S, K, T, r, q, v = (mp.mpf(z) for z in (S, K, T, r, q, v))
    if call and q <= 0:
        return bsm(S, K, T, r, q, v, True)
    if not call and r <= 0:
        return bsm(S, K, T, r, q, v, False)
    raw = call2002(S, K, T, r, r - q, v) if call else call2002(K, S, T, q, q - r, v)
    intrinsic = max(S - K, 0) if call else max(K - S, 0)
    return max(raw, intrinsic, bsm(S, K, T, r, q, v, call))


def bs2002_rows():
    rows = []
    n = 0
    for S in (60.0, 85.0, 100.0, 110.0, 140.0):
        for T in (7 / 365, 0.25, 1.0, 3.0):
            for r in (0.0, 0.03, 0.08):
                for q in (0.0, 0.02, 0.07, 0.12):
                    for v in (0.05, 0.15, 0.3, 0.6, 1.2):
                        for call in (True, False):
                            n += 1
                            # A deterministic thinning to ~1 case in 11.
                            if n % 11:
                                continue
                            p = american(S, 100.0, T, r, q, v, call)
                            rows.append([f(S), "100.0", f(T), f(r), f(q), f(v), "call" if call else "put", f(p)])
    # TF Quant Finance's documented example: S = F e^{0.01}.
    for F in (80.0, 90.0, 100.0, 110.0, 120.0):
        S = float(mp.mpf(F) * mp.exp(mp.mpf("0.01")))
        rows.append([f(S), "100.0", "0.25", "0.08", "0.12", "0.2", "call", f(american(S, 100.0, 0.25, 0.08, 0.12, 0.2, True))])
    return rows


def quantile_rows():
    """p and Phi^{-1}(p) for p from 1e-300 to 1 - 1e-16, solved at 400 digits
    (1 - 2p must keep the tail's digits)."""
    rows = []
    ps = [1e-300, 1e-200, 1e-100, 1e-50, 1e-20, 1e-10, 1e-5, 1e-3, 0.0125, 0.02425, 0.05, 0.075, 0.1, 0.2, 0.3, 0.4, 0.45, 0.4999]
    ps += [0.5, 0.5001, 0.55, 0.6, 0.7, 0.8, 0.9, 0.925, 0.95, 0.975, 0.97575, 0.99, 0.999, 0.99999, 1 - 1e-10, 1 - 1e-14, 1 - 1e-16]
    with mp.workdps(400):
        for p in ps:
            P = mp.mpf(p)
            x = -mp.sqrt(2) * mp.erfinv(1 - 2 * P) if P <= 0.5 else mp.sqrt(2) * mp.erfinv(2 * P - 1)
            rows.append([f(P), f(x)])
    return rows


def black_rows():
    """Jaeckel's normalised Black call b(x, s) = e^{x/2} N(x/s + s/2) -
    e^{-x/2} N(x/s - s/2) for x <= 0 and its complement e^{x/2} - b, at 80
    digits (b is a difference of nearly equal terms where s is small), over
    all four of the module's regions; values below 1e-300 are left out."""
    rows = []
    with mp.workdps(80):
        for x in (-1e-12, -1e-6, -0.01, -0.1, -0.5, -1.0, -3.0, -10.0, -30.0, -100.0, -500.0):
            for s in (1e-6, 1e-4, 1e-3, 0.01, 0.05, 0.1, 0.2, 0.3, 0.41, 0.43, 0.5, 1.0, 1.7, 2.0, 5.0, 10.0, 20.0, 50.0):
                X, S = mp.mpf(x), mp.mpf(s)
                b = mp.exp(X / 2) * mp.ncdf(X / S + S / 2) - mp.exp(-X / 2) * mp.ncdf(X / S - S / 2)
                c = mp.exp(X / 2) * mp.ncdf(-(X / S + S / 2)) + mp.exp(-X / 2) * mp.ncdf(X / S - S / 2)
                if b < mp.mpf("1e-300"):
                    continue
                rows.append([f(X), f(S), f(b), f(c)])
    return rows


def implied_rows():
    """European prices and vegas at known volatilities, exact to the nearest
    double: the implied volatility test measures its price error against
    these, not against the module's own (cancelling) price formula."""
    rows = []
    S, r, q = mp.mpf(100), mp.mpf("0.04"), mp.mpf("0.015")
    for kf in (0.5, 0.6, 0.8, 0.9, 0.97, 1.0, 1.03, 1.1, 1.25, 1.5, 2.0):
        for tau in (1 / 365, 7 / 365, 30 / 365, 0.25, 1.0, 2.0, 5.0):
            T = mp.mpf(tau)
            K = float(kf * S * mp.exp((r - q) * T))
            for v in (0.01, 0.03, 0.1, 0.2, 0.5, 1.0, 2.0, 3.0):
                for call in (True, False):
                    V = mp.mpf(v)
                    p = bsm(S, mp.mpf(K), T, r, q, V, call)
                    if p < mp.mpf("1e-280"):
                        continue  # the double would be denormal
                    vt = V * mp.sqrt(T)
                    d1 = (mp.log(S / K) + (r - q) * T) / vt + vt / 2
                    vega = S * mp.exp(-q * T) * mp.npdf(d1) * mp.sqrt(T)
                    rows.append(["100.0", f(K), f(T), "0.04", "0.015", f(V), "call" if call else "put", f(p), f(vega)])
    return rows


def write(name, header, rows):
    with open(os.path.join(HERE, name), "w", newline="") as fh:
        w = csv.writer(fh, lineterminator="\n")
        w.writerow(header)
        w.writerows(rows)
    print(name, len(rows), "rows")


if __name__ == "__main__":
    which = sys.argv[2:] or ["normal", "quantile", "bivariate", "bs2002", "implied", "black"]
    if "black" in which:
        write("black.csv", ["x", "s", "b", "complement"], black_rows())
    if "quantile" in which:
        write("quantile.csv", ["p", "x"], quantile_rows())
    if "implied" in which:
        write("implied.csv", ["s", "k", "tau", "r", "q", "sigma", "right", "price", "vega"], implied_rows())
    if "normal" in which:
        write("normal.csv", ["x", "cdf", "pdf", "erfcx"], normal_rows())
    if "bivariate" in which:
        write("bivariate.csv", ["h", "k", "rho", "cdf"], bivariate_rows())
    if "bs2002" in which:
        write("bs2002.csv", ["s", "k", "tau", "r", "q", "sigma", "right", "price"], bs2002_rows())
