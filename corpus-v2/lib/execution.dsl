# Execution presets (docs/language-v2.md, 4.3). A strategy names one, with
# or without overrides, and `abt show` prints the expanded block: every field
# is in the program, none in a flag.

# The conservative defaults of the semantic model's section 6.
execution equity_default {
  price         close
  orders        market
  lot           whole
  capital       1_000_000 USD fixed
  commission    per_share 0.005 USD min 1 USD
  slippage      vol 0.1 over 20 bars
  impact        sqrt 0.1 over 20 days adv
  participation 0.1 of bar volume
  financing     cash 0 debit 0.05 short 0
  leverage      gross <= 1 halt
  actions       apply
  delisting     haircut { bankruptcy 1, regulatory 1, acquisition 0, voluntary 0, other 1 }
  oversize      halt
  ruin          halt
  numerics      running
}

# A weekly equity book on a fixed base, trading at the signal close at a
# total-return price, ten basis points a side and nothing else.
execution book_fixed_base {
  price         tr_index@weekly
  orders        moc
  lot           fractional
  capital       1_000_000 USD fixed
  commission    10 bps per side
  slippage      none
  impact        none
  participation none
  financing     none
  leverage      unconstrained
  actions       in_price
  delisting     last_price
  oversize      halt
  ruin          halt
  numerics      two_pass
}

# An intraday futures book on a fixed base, commission per contract only,
# flat at every close: no session's settlement moves its cash (K-5.4).
execution futures_fixed_base {
  price         close
  orders        moo_moc
  lot           fractional
  capital       1_000_000 USD fixed
  commission    per_contract from catalog
  slippage      none
  impact        none
  participation none
  financing     none
  leverage      unconstrained
  actions       apply
  delisting     last_price
  oversize      halt
  ruin          halt
  numerics      two_pass
  settlement    none
}

# The pipeline's house presets (absolute-pipeline, agent-guidelines.md 15;
# ERRATA E-96): `futures_fixed_base` plus the house commission, and its
# equity counterpart; no slippage for starters (PL-9.7 later). The harness
# refuses a program whose execution block is not one of these.
execution futures_house {
  price         close
  orders        moo_moc
  lot           fractional
  capital       1_000_000 USD fixed
  commission    per_contract 3.75          # 7.50 round-turn, half per side; rolls pay it per contract (K-5.4)
  slippage      none                       # for starters (owner, Oct 19); `range 0.10` on open and stop fills when PL-9.7 lands
  impact        none
  participation none
  financing     none
  leverage      unconstrained
  actions       apply
  delisting     last_price
  oversize      halt
  ruin          halt
  numerics      exact
  settlement    none
}

execution equities_house {
  price         close
  orders        moo_moc
  lot           fractional
  capital       1_000_000 USD fixed
  commission    per_share 0.005 min 1
  slippage      none
  impact        none
  participation none
  financing     none
  leverage      unconstrained
  actions       apply
  delisting     last_price
  oversize      halt
  ruin          halt
  numerics      exact
  limits        { weight <= 0.10 per name clamp }
}
