//! The dimensional type algebra of section 2 as a set of total functions
//! from operand types to a result type or a diagnostic message.

use crate::ir::{BinOp, CmpOp, Dim, Ty};

/// Whether a value of type `actual` may be used where `expected` is required.
/// The only coercion is a bare integer literal standing for a Count or a Scalar.
pub fn compat(expected: &Ty, actual: &Ty) -> bool {
    if expected == actual {
        return true;
    }
    match (expected, actual) {
        (Ty::Count, Ty::IntLit) | (Ty::IntLit, Ty::Count) => true,
        (Ty::Quantity(d), Ty::IntLit) | (Ty::IntLit, Ty::Quantity(d)) => d.is_scalar(),
        // A string literal is an Equity or a Label from context.
        (Ty::Equity, Ty::StrLit) | (Ty::StrLit, Ty::Equity) | (Ty::Label, Ty::StrLit) | (Ty::StrLit, Ty::Label) => true,
        _ => false,
    }
}

/// Resolve an integer literal against the other operand of a binary form.
fn coerce_lit(lit_side: &Ty, other: &Ty) -> Ty {
    if *lit_side != Ty::IntLit {
        return lit_side.clone();
    }
    match other {
        Ty::Count => Ty::Count,
        _ => Ty::scalar(),
    }
}

pub fn bin_type(op: BinOp, l: &Ty, r: &Ty) -> Result<Ty, String> {
    if *l == Ty::IntLit && *r == Ty::IntLit {
        return Ok(if op == BinOp::Div { Ty::scalar() } else { Ty::IntLit });
    }
    // An integer literal next to a dimensioned quantity: fine as a factor,
    // meaningless as a summand (a bare number has no unit).
    if *l == Ty::IntLit || *r == Ty::IntLit {
        let (lit, other) = if *l == Ty::IntLit { (l, r) } else { (r, l) };
        if let Ty::Quantity(d) = other {
            if !d.is_scalar() && matches!(op, BinOp::Add | BinOp::Sub) {
                return Err(format!("cannot {} a bare number and a {}: literals carry units (write e.g. `1 USD` or `1 shares`)", verb(op), other));
            }
        }
        let lc = coerce_lit(lit, other);
        let (l2, r2) = if *l == Ty::IntLit { (lc, r.clone()) } else { (l.clone(), lc) };
        return bin_type(op, &l2, &r2);
    }
    match (l, r) {
        (Ty::Count, Ty::Count) => Ok(match op {
            BinOp::Div => Ty::scalar(),
            _ => Ty::Count,
        }),
        (Ty::Count, Ty::Quantity(d)) => match op {
            BinOp::Div => Ok(Ty::Quantity(Dim::scalar().div(d).ok_or("currency mismatch")?)),
            _ => Err(format!("Count converts to Scalar only through division; cannot {} Count and {}", verb(op), r)),
        },
        (Ty::Quantity(d), Ty::Count) => match op {
            BinOp::Div => Ok(Ty::Quantity(d.clone())),
            _ => Err(format!("Count converts to Scalar only through division; cannot {} {} and Count", verb(op), l)),
        },
        (Ty::Quantity(a), Ty::Quantity(b)) => match op {
            BinOp::Add | BinOp::Sub => {
                if a == b {
                    Ok(Ty::Quantity(a.clone()))
                } else {
                    Err(format!("dimensions do not balance: cannot {} {} and {}", verb(op), a, b))
                }
            }
            BinOp::Mul => a.mul(b).map(Ty::Quantity).ok_or_else(|| format!("currency mismatch: cannot multiply {} and {}", a, b)),
            BinOp::Div => a.div(b).map(Ty::Quantity).ok_or_else(|| format!("currency mismatch: cannot divide {} by {}", a, b)),
        },
        (Ty::Timestamp, _) | (_, Ty::Timestamp) => Err("Timestamp admits only comparison and the temporal builtins; use prev/lag/window for bar distances".into()),
        (Ty::Duration, _) | (_, Ty::Duration) => Err("Duration admits no arithmetic in v1; use it as a window or lag length".into()),
        _ => Err(format!("cannot {} {} and {}", verb(op), l, r)),
    }
}

fn verb(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Sub => "subtract",
        BinOp::Mul => "multiply",
        BinOp::Div => "divide",
    }
}

pub fn neg_type(t: &Ty) -> Result<Ty, String> {
    match t {
        Ty::IntLit | Ty::Quantity(_) => Ok(t.clone()),
        Ty::Count => Err("Count is non-negative; negate it after dividing to a Scalar".into()),
        _ => Err(format!("cannot negate a {}", t)),
    }
}

pub fn call_type(name: &str, args: &[Ty]) -> Result<Ty, String> {
    let scalar_arg = |t: &Ty| -> bool { matches!(t, Ty::IntLit) || matches!(t, Ty::Quantity(d) if d.is_scalar()) };
    match name {
        "log" | "exp" => {
            if args.len() != 1 {
                return Err(format!("{} takes one argument", name));
            }
            if !scalar_arg(&args[0]) {
                return Err(format!("{} requires a Scalar argument, found {} (take the ratio of two like-dimensioned values first)", name, args[0]));
            }
            Ok(Ty::scalar())
        }
        "sqrt" => {
            if args.len() != 1 {
                return Err("sqrt takes one argument".into());
            }
            match &args[0] {
                Ty::IntLit => Ok(Ty::scalar()),
                Ty::Quantity(d) => d.sqrt().map(Ty::Quantity).ok_or_else(|| format!("sqrt of {} would leave the half-integer exponent lattice", d)),
                t => Err(format!("sqrt requires a dimensioned quantity, found {}", t)),
            }
        }
        "abs" => {
            if args.len() != 1 {
                return Err("abs takes one argument".into());
            }
            match &args[0] {
                Ty::IntLit | Ty::Quantity(_) | Ty::Count => Ok(args[0].clone()),
                t => Err(format!("abs requires a quantity, found {}", t)),
            }
        }
        "least" | "greatest" => {
            if args.len() < 2 {
                return Err(format!("{} takes at least two arguments", name));
            }
            let mut acc = args[0].clone();
            for a in &args[1..] {
                if !compat(&acc, a) {
                    return Err(format!("{} requires arguments of one dimension, found {} and {}", name, acc, a));
                }
                if acc == Ty::IntLit {
                    acc = a.clone();
                }
            }
            match acc {
                Ty::IntLit | Ty::Quantity(_) | Ty::Count => Ok(acc),
                t => Err(format!("{} requires quantities, found {}", name, t)),
            }
        }
        _ => Err(format!("unknown scalar function `{}`", name)),
    }
}

/// Whether two operand types may be compared with `op`.
pub fn cmp_ok(op: CmpOp, l: &Ty, r: &Ty) -> Result<(), String> {
    if *l == Ty::StrLit && *r == Ty::StrLit {
        return Err("a string literal is an Equity or a Label from context; compare it with a typed variable or declare a param".into());
    }
    if !compat(l, r) {
        return Err(format!("cannot compare {} with {}: both sides must share one dimension and currency", l, r));
    }
    match l {
        Ty::Equity | Ty::Decision | Ty::Label | Ty::StrLit => {
            if op != CmpOp::Eq {
                return Err(format!("{} admits only `=`", l));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Result type of an aggregate over argument types (section 2).
pub fn agg_type(name: &str, args: &[Ty]) -> Result<Ty, String> {
    let one = |what: &str| -> Result<&Ty, String> {
        if args.len() != 1 {
            Err(format!("{} takes exactly one argument", what))
        } else {
            Ok(&args[0])
        }
    };
    let two = |what: &str| -> Result<(&Ty, &Ty), String> {
        if args.len() != 2 {
            Err(format!("{} takes exactly two arguments", what))
        } else {
            Ok((&args[0], &args[1]))
        }
    };
    let as_q = |t: &Ty| -> Result<Ty, String> {
        match t {
            Ty::IntLit => Ok(Ty::scalar()),
            Ty::Quantity(_) | Ty::Count => Ok(t.clone()),
            t => Err(format!("aggregate over a {} is not defined", t)),
        }
    };
    match name {
        "count" => {
            one("count")?;
            Ok(Ty::Count)
        }
        "sum" | "max" | "min" | "first" | "last" => as_q(one(name)?),
        "mean" | "std" | "median" => {
            let t = as_q(one(name)?)?;
            Ok(if t == Ty::Count { Ty::scalar() } else { t })
        }
        "quantile" => {
            let (e, q) = two("quantile")?;
            if !matches!(q, Ty::IntLit) && !matches!(q, Ty::Quantity(d) if d.is_scalar()) {
                return Err("quantile level must be a Scalar in [0, 1]".into());
            }
            as_q(e)
        }
        "corr" => {
            let (a, b) = two("corr")?;
            as_q(a)?;
            as_q(b)?;
            Ok(Ty::scalar())
        }
        "cov" => {
            let (a, b) = two("cov")?;
            let (a, b) = (as_q(a)?, as_q(b)?);
            bin_type(BinOp::Mul, &a, &b)
        }
        "ols_beta" => {
            let (y, x) = two("ols_beta")?;
            let (y, x) = (as_q(y)?, as_q(x)?);
            bin_type(BinOp::Div, &y, &x)
        }
        _ => Err(format!("unknown aggregate `{}`", name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price() -> Ty {
        Ty::Quantity(Dim::price("USD"))
    }
    fn shares() -> Ty {
        Ty::Quantity(Dim::shares())
    }
    fn notional() -> Ty {
        Ty::Quantity(Dim::notional("USD"))
    }

    #[test]
    fn dimensions_balance_by_arithmetic() {
        assert_eq!(bin_type(BinOp::Mul, &price(), &shares()).unwrap(), notional());
        assert_eq!(bin_type(BinOp::Div, &notional(), &price()).unwrap(), shares());
        assert_eq!(bin_type(BinOp::Div, &price(), &price()).unwrap(), Ty::scalar());
        assert!(bin_type(BinOp::Add, &price(), &Ty::scalar()).is_err());
        assert!(bin_type(BinOp::Add, &price(), &Ty::IntLit).is_err());
        assert_eq!(bin_type(BinOp::Mul, &price(), &Ty::IntLit).unwrap(), price());
        assert!(bin_type(BinOp::Add, &Ty::Quantity(Dim::price("USD")), &Ty::Quantity(Dim::price("EUR"))).is_err());
    }

    #[test]
    fn count_converts_only_through_division() {
        assert_eq!(bin_type(BinOp::Div, &Ty::IntLit, &Ty::Count).unwrap(), Ty::scalar());
        assert_eq!(bin_type(BinOp::Div, &Ty::scalar(), &Ty::Count).unwrap(), Ty::scalar());
        assert!(bin_type(BinOp::Add, &Ty::Count, &Ty::scalar()).is_err());
        assert!(bin_type(BinOp::Mul, &Ty::Count, &price()).is_err());
        assert_eq!(bin_type(BinOp::Add, &Ty::Count, &Ty::IntLit).unwrap(), Ty::Count);
    }

    #[test]
    fn scalar_functions_and_sqrt() {
        assert!(call_type("log", &[price()]).is_err());
        assert_eq!(call_type("log", &[Ty::scalar()]).unwrap(), Ty::scalar());
        assert_eq!(
            call_type("sqrt", &[notional()]).unwrap(),
            Ty::Quantity(Dim {
                c2: 1,
                s2: 0,
                t2: 0,
                currency: Some("USD".into())
            })
        );
        assert_eq!(call_type("greatest", &[price(), price()]).unwrap(), price());
        assert!(call_type("greatest", &[price(), shares()]).is_err());
        assert_eq!(call_type("least", &[Ty::IntLit, Ty::scalar()]).unwrap(), Ty::scalar());
    }

    #[test]
    fn aggregates_keep_or_combine_dimensions() {
        assert_eq!(agg_type("std", &[price()]).unwrap(), price());
        assert_eq!(agg_type("count", &[Ty::Equity]).unwrap(), Ty::Count);
        assert_eq!(agg_type("corr", &[price(), shares()]).unwrap(), Ty::scalar());
        assert_eq!(agg_type("cov", &[price(), shares()]).unwrap(), notional());
        assert_eq!(agg_type("ols_beta", &[notional(), price()]).unwrap(), shares());
        assert_eq!(agg_type("mean", &[Ty::Count]).unwrap(), Ty::scalar());
    }

    #[test]
    fn comparisons_need_one_dimension() {
        assert!(cmp_ok(CmpOp::Lt, &price(), &price()).is_ok());
        assert!(cmp_ok(CmpOp::Lt, &price(), &notional()).is_err());
        assert!(cmp_ok(CmpOp::Lt, &Ty::Equity, &Ty::Equity).is_err());
        assert!(cmp_ok(CmpOp::Eq, &Ty::Equity, &Ty::Equity).is_ok());
        assert!(cmp_ok(CmpOp::Ge, &Ty::Count, &Ty::IntLit).is_ok());
    }
}
