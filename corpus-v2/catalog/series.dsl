# Market series by name, each on its native calendar; read on a coarser or
# equal calendar as a bar view and on a finer one as of the bar (3.4).
class series {
  value(Name: Label, @T, V: Scalar)
  names "SPXTR" @1d, "VIX" @1w, "VIX_MA4" @1w
}
