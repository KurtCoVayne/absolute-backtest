# Task: implement the R8L intraday futures opening-range book

R8L trades US futures intraday: it reads the first 30 minutes of each
session and, on some sessions, takes one position held until the session's
close. Implement it as an abt strategy named `r8l` that reproduces the rules
exactly as specified below.

## Data

Bundle: `~/data/abt/b_r8l_fb` (environment
`futures_sessions`, see `env/futures_sessions.dsl`). 26 futures roots
(back-adjusted continuous contracts; prices may be negative), sessions from
2000 to 2026-08-14. "Minute k" is the k-th minute since the root's primary
session opened (minute 0 is the first minute). Per root:

- one daily bar per session (the session's open, high, low and close),
  labelled by the session date, and the list of session days;
- the contract multiplier (currency per point per contract);
- minute prints: the open of minute 0; the close of the last traded minute
  before minute 20 and before minute 30; the entry print (the first traded
  minute from minute 30) and the exit print (the session's last traded
  minute), each labelled at its minute's close;
- the decision times: 30 minutes after the open of every session on which
  the book may trade that root, together with that session's open time. Only
  these decision times may produce trades.
- A reporting calendar (the lead market's session days).

Commission per contract per side is in the bundle's security table and is
charged by the executor.

## Rules (per root and session)

All daily features are computed over the root's own sessions, and are read
at the decision time as of the **previous** session (a feature the previous
session lacks is missing, not carried over from an earlier session).

- **True range** of a session: max(high - low, |high - previous close|,
  |low - previous close|). A root's first session has none.
- **ATR**: the mean true range over the root's last 20 sessions (needs all 20).
  Use the previous session's ATR. An ATR <= 0 means no trade.
- **m** = (close before minute 30 - open of minute 0) / ATR.
- **g** (gap) = (open of minute 0 - previous session's close) / ATR.
- **m_med**: the median of |m| over the root's last 250 sessions (at least
  100 sessions with a value; a session with no m still takes a place in the
  250), as of the previous session.
- **latefrac** = (close before minute 30 - close before minute 20) /
  (close before minute 30 - open of minute 0).

Entries (at most one leg per root per session), decided at the decision
time:

- **Main**: |m| >= 2 x m_med, the gap is aligned (|g| < 1, or g and m have the
  same sign), and latefrac <= 1/3. Direction = sign of m.
- **Sidecar**: |m| < 2 x m_med and g <= -0.75. Direction = short.

The leg enters at the open of the entry print and exits at the session's last
print (close). No stop, no other exit.

**Sizing**: a fixed risk of D = $6,493.912071090746 per ATR per leg (0.6494%
of the fixed $1M base): contracts = direction x D / (ATR x multiplier),
fractional.

## Run conventions

- Fixed $1,000,000 base, no compounding; fractional contracts.
- Fills and marks at the minute prints (the exit print's close relation is
  the price relation).
- Commission per contract only (from the security table); no slippage, no
  financing; no margin model, leverage never constrains the book.
- Report daily returns on the reporting calendar, 252 periods a year; save
  the returns with `--returns` and the run's records with `--dump`.
- A full run takes a few seconds.
