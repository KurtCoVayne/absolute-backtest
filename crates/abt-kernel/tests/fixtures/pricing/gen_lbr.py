"""Cross-check cases for the European implied volatility from
py_lets_be_rational 1.1.2 (vollib's port of Jaeckel's LetsBeRational), an
independent implementation of the same algorithm: lbr.csv holds undiscounted
Black prices (F, K, T, call/put, price from its black()) and its implied
volatility of that price. tests/pricing.rs solves the same prices with
r = q = 0 (so S = F) and compares.

Run with the three wheels (py_lets_be_rational, cody_special,
piecewise_rational) unpacked into directories of their own:
    python3 -I gen_lbr.py /path/lbr /path/cody /path/pwr
"""
import csv
import os
import sys

for path in sys.argv[1:4]:
    sys.path.insert(0, path)
import py_lets_be_rational as lbr  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))

rows = []
F = 100.0
for k in (40.0, 70.0, 90.0, 99.0, 100.0, 101.0, 115.0, 150.0, 300.0):
    for T in (1 / 365, 30 / 365, 0.5, 2.0, 10.0):
        for sigma in (0.005, 0.05, 0.25, 0.8, 2.5):
            for q, right in ((1.0, "call"), (-1.0, "put")):
                price = lbr.black(F, k, sigma, T, q)
                if not price > 1e-250:
                    continue
                intrinsic = max(q * (F - k), 0.0)
                if price - intrinsic <= 1e-12 * price:
                    continue  # time value below the price's resolution
                iv = lbr.implied_volatility_from_a_transformed_rational_guess(price, F, k, T, q)
                rows.append([repr(F), repr(k), repr(T), right, repr(price), repr(sigma), repr(iv)])

with open(os.path.join(HERE, "lbr.csv"), "w", newline="") as fh:
    w = csv.writer(fh, lineterminator="\n")
    w.writerow(["f", "k", "tau", "right", "price", "sigma", "lbr_iv"])
    w.writerows(rows)
print("lbr.csv", len(rows), "rows")
