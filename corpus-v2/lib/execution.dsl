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

# An intraday futures book on a fixed base, commission per contract only.
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
}
