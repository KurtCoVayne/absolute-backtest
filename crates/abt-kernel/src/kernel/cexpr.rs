//! The kernel's scalar arithmetic, and expressions compiled to run over the
//! columns of a bar (the block evaluator; ERRATA E-150). There is one
//! arithmetic: `Kernel::eval_expr` applies the functions below to a row's
//! values, and a `CExpr` applies the same functions to every row of a
//! column, so a value computed either way has the same bits. A Count with a
//! Count stays a Count for `+`, `-` and `*`; anything else numeric is a
//! number. Where the row path's operation is partial (a division by zero,
//! the log of a non-positive value, arithmetic on a time), a column's
//! evaluation gives no column, and the operator that asked declines the bar
//! to the row path, which raises or excludes the instance as it does.

use std::borrow::Cow;
use std::cmp::Ordering;

use super::col::Col;
use super::value::Value;
use crate::ir::{BinOp, CmpOp, Expr, Lit};

/// `p op q` on two Counts, for `+`, `-` and `*`; a division is a number's.
pub(crate) fn bin_count(op: BinOp, p: i64, q: i64) -> Option<i64> {
    match op {
        BinOp::Add => Some(p + q),
        BinOp::Sub => Some(p - q),
        BinOp::Mul => Some(p * q),
        BinOp::Div => None,
    }
}

/// `p op q` on two numbers; partial at a division by zero.
pub(crate) fn bin_num(op: BinOp, p: f64, q: f64) -> Result<f64, &'static str> {
    Ok(match op {
        BinOp::Add => p + q,
        BinOp::Sub => p - q,
        BinOp::Mul => p * q,
        BinOp::Div => {
            if q == 0.0 {
                return Err("division by zero");
            }
            p / q
        }
    })
}

/// `x op y` on two values: two Counts by `bin_count` where it answers, any
/// two numbers by `bin_num`; partial on anything else.
pub(crate) fn bin_value(op: BinOp, x: &Value, y: &Value) -> Result<Value, &'static str> {
    if let (Value::Count(p), Value::Count(q)) = (x, y) {
        if let Some(c) = bin_count(op, *p, *q) {
            return Ok(Value::Count(c));
        }
    }
    match (x.as_f64(), y.as_f64()) {
        (Some(p), Some(q)) => bin_num(op, p, q).map(Value::Num),
        _ => Err("arithmetic on non-numeric values"),
    }
}

/// `-x`; partial on a value that is not a number.
pub(crate) fn neg_value(x: &Value) -> Result<Value, &'static str> {
    match x {
        Value::Num(v) => Ok(Value::Num(-v)),
        Value::Count(c) => Ok(Value::Count(-c)),
        _ => Err("cannot negate a non-numeric value"),
    }
}

/// The functions of one number a compiled expression may call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fn1 {
    Log,
    Exp,
    Sqrt,
    Abs,
    Sign,
    Floor,
    Ceil,
}

impl Fn1 {
    pub(crate) fn parse(name: &str) -> Option<Fn1> {
        Some(match name {
            "log" => Fn1::Log,
            "exp" => Fn1::Exp,
            "sqrt" => Fn1::Sqrt,
            "abs" => Fn1::Abs,
            "sign" => Fn1::Sign,
            "floor" => Fn1::Floor,
            "ceil" => Fn1::Ceil,
            _ => return None,
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Fn1::Log => "log",
            Fn1::Exp => "exp",
            Fn1::Sqrt => "sqrt",
            Fn1::Abs => "abs",
            Fn1::Sign => "sign",
            Fn1::Floor => "floor",
            Fn1::Ceil => "ceil",
        }
    }
}

/// `f(x)` on a number; partial at the log of a non-positive value and the
/// square root of a negative one (the message is the row path's halt's).
pub(crate) fn call1_num(f: Fn1, x: f64) -> Result<f64, String> {
    Ok(match f {
        Fn1::Log => {
            if x <= 0.0 {
                return Err(format!("log of a non-positive value ({})", x));
            }
            x.ln()
        }
        Fn1::Exp => x.exp(),
        Fn1::Sqrt => {
            if x < 0.0 {
                return Err(format!("sqrt of a negative value ({})", x));
            }
            x.sqrt()
        }
        Fn1::Abs => x.abs(),
        Fn1::Sign => {
            if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            }
        }
        Fn1::Floor => x.floor(),
        Fn1::Ceil => x.ceil(),
    })
}

/// `f(v)`: `abs` keeps a Count a Count, every other result is a number;
/// partial on a value that is not a number.
pub(crate) fn call1_value(f: Fn1, v: &Value) -> Result<Value, String> {
    if let (Fn1::Abs, Value::Count(c)) = (f, v) {
        return Ok(Value::Count(c.abs()));
    }
    let x = v.as_f64().ok_or_else(|| format!("{} of a non-numeric value", f.name()))?;
    call1_num(f, x).map(Value::Num)
}

/// `least(...)` or `greatest(...)` of the values, the first of equals
/// kept. An integer literal is a Count and a Scalar a Num, and the checker
/// admits the mix (`least(1, W)`), so the comparison is numeric across the
/// two, never the enum's variant order; the result is a Scalar when any
/// argument is one.
pub(crate) fn least_greatest(least: bool, vals: &[Value]) -> Value {
    let num = |v: &Value| match v {
        Value::Num(x) => Some(*x),
        Value::Count(c) => Some(*c as f64),
        _ => None,
    };
    let mut best = vals[0].clone();
    for v in &vals[1..] {
        let better = match (num(v), num(&best)) {
            (Some(a), Some(b)) => {
                if least {
                    a < b
                } else {
                    a > b
                }
            }
            _ => {
                if least {
                    v < &best
                } else {
                    v > &best
                }
            }
        };
        if better {
            best = v.clone();
        }
    }
    if let (Value::Count(c), true) = (&best, vals.iter().any(|v| matches!(v, Value::Num(_)))) {
        best = Value::Num(*c as f64);
    }
    best
}

/// Whether `op` holds of an ordering.
pub(crate) fn holds(op: CmpOp, ord: Ordering) -> bool {
    match op {
        CmpOp::Lt => ord == Ordering::Less,
        CmpOp::Le => ord != Ordering::Greater,
        CmpOp::Eq => ord == Ordering::Equal,
        CmpOp::Gt => ord == Ordering::Greater,
        CmpOp::Ge => ord != Ordering::Less,
    }
}

/// `x op y` on two numbers: false when either is NaN.
pub(crate) fn compare_num(op: CmpOp, x: f64, y: f64) -> bool {
    match x.partial_cmp(&y) {
        Some(o) => holds(op, o),
        None => false,
    }
}

/// A column of a frame, or one value for every row (a parameter, a
/// literal, the bar).
#[derive(Clone, Debug)]
pub(crate) enum FCol {
    Const(Value),
    Col(Col),
}

impl FCol {
    pub(crate) fn value(&self, i: usize) -> Value {
        match self {
            FCol::Const(v) => v.clone(),
            FCol::Col(c) => c.value(i),
        }
    }

    /// The rows `idx` names; a constant stays one.
    pub(crate) fn gather(&self, idx: &[u32]) -> FCol {
        match self {
            FCol::Const(v) => FCol::Const(v.clone()),
            FCol::Col(c) => FCol::Col(c.gather(idx)),
        }
    }

    /// `n` rows of it as a column.
    pub(crate) fn into_col(self, n: usize) -> Col {
        match self {
            FCol::Const(v) => Col::repeat(&v, n),
            FCol::Col(c) => c,
        }
    }

    fn nums(&self) -> Option<Nums<'_>> {
        Some(match self {
            FCol::Const(v) => Nums::Const(v.as_f64()?),
            FCol::Col(Col::Num(c)) => Nums::Num(c),
            FCol::Col(Col::Count(c)) => Nums::Count(c),
            FCol::Col(_) => return None,
        })
    }

    fn counts(&self) -> Option<Counts<'_>> {
        Some(match self {
            FCol::Const(Value::Count(c)) => Counts::Const(*c),
            FCol::Col(Col::Count(c)) => Counts::Col(c),
            _ => return None,
        })
    }
}

/// A column read as numbers (a Count as its `f64`, as `Value::as_f64`).
enum Nums<'a> {
    Const(f64),
    Num(&'a [f64]),
    Count(&'a [i64]),
}

impl Nums<'_> {
    fn at(&self, i: usize) -> f64 {
        match self {
            Nums::Const(x) => *x,
            Nums::Num(c) => c[i],
            Nums::Count(c) => c[i] as f64,
        }
    }
}

/// A column of Counts.
enum Counts<'a> {
    Const(i64),
    Col(&'a [i64]),
}

impl Counts<'_> {
    fn at(&self, i: usize) -> i64 {
        match self {
            Counts::Const(x) => *x,
            Counts::Col(c) => c[i],
        }
    }
}

/// An expression with its variables resolved to the rule's slots and its
/// parameters and literals to values, once.
#[derive(Clone, Debug)]
pub(crate) enum CExpr {
    Slot(usize),
    Const(Value),
    Neg(Box<CExpr>),
    Bin(BinOp, Box<CExpr>, Box<CExpr>),
    Call1(Fn1, Box<CExpr>),
    /// `least` (true) or `greatest` of two.
    Call2(bool, Box<CExpr>, Box<CExpr>),
}

impl CExpr {
    /// `e` compiled, its variables through `slot` (the slot of a variable
    /// bound to a column, `None` for any other) and its parameters through
    /// `param`. `None` for what no column evaluates: a function other than
    /// the arithmetic above, a literal that is not a number or a time.
    pub(crate) fn compile(e: &Expr, slot: &dyn Fn(&str) -> Option<usize>, param: &dyn Fn(&str) -> Option<Value>) -> Option<CExpr> {
        Some(match e {
            Expr::Var(v, _) => CExpr::Slot(slot(v)?),
            Expr::Param(p, _) => CExpr::Const(constant(param(p)?)?),
            Expr::Lit(l, _) => CExpr::Const(match l {
                Lit::Int(i) => Value::Count(*i),
                Lit::Float(x) | Lit::Quantity(x, _) | Lit::Money(x, _) | Lit::Price(x, _) => Value::Num(*x),
                Lit::Time(t) => Value::Time(*t),
                Lit::Duration(_) | Lit::Str(_) | Lit::Entity(_) | Lit::Label(_) => return None,
            }),
            Expr::Neg(x, _) => CExpr::Neg(Box::new(CExpr::compile(x, slot, param)?)),
            Expr::Bin(op, a, b, _) => CExpr::Bin(*op, Box::new(CExpr::compile(a, slot, param)?), Box::new(CExpr::compile(b, slot, param)?)),
            Expr::Call(f, args, _) => match (f.as_str(), args.as_slice()) {
                ("least" | "greatest", [a, b]) => CExpr::Call2(f == "least", Box::new(CExpr::compile(a, slot, param)?), Box::new(CExpr::compile(b, slot, param)?)),
                (_, [x]) => CExpr::Call1(Fn1::parse(f)?, Box::new(CExpr::compile(x, slot, param)?)),
                _ => return None,
            },
        })
    }

    /// The slots the expression reads, appended to `out`.
    pub(crate) fn slots(&self, out: &mut Vec<usize>) {
        match self {
            CExpr::Slot(s) => out.push(*s),
            CExpr::Const(_) => {}
            CExpr::Neg(x) | CExpr::Call1(_, x) => x.slots(out),
            CExpr::Bin(_, a, b) | CExpr::Call2(_, a, b) => {
                a.slots(out);
                b.slots(out);
            }
        }
    }

    /// The expression over `n` rows whose slots are `cols`: a column, or
    /// one value when it reads no column; `None` when any row's operation
    /// is partial (or a slot it reads is unbound).
    pub(crate) fn eval(&self, cols: &[Option<FCol>], n: usize) -> Option<FCol> {
        self.eval_ref(cols, n).map(Cow::into_owned)
    }

    fn eval_ref<'a>(&self, cols: &'a [Option<FCol>], n: usize) -> Option<Cow<'a, FCol>> {
        Some(match self {
            CExpr::Slot(s) => Cow::Borrowed(cols.get(*s)?.as_ref()?),
            CExpr::Const(v) => Cow::Owned(FCol::Const(v.clone())),
            CExpr::Neg(x) => {
                let x = x.eval_ref(cols, n)?;
                Cow::Owned(match x.as_ref() {
                    FCol::Const(v) => FCol::Const(neg_value(v).ok()?),
                    FCol::Col(Col::Num(c)) => FCol::Col(Col::Num(c.iter().map(|v| -v).collect())),
                    FCol::Col(Col::Count(c)) => FCol::Col(Col::Count(c.iter().map(|v| -v).collect())),
                    x => by_row(n, |i| neg_value(&x.value(i)).ok())?,
                })
            }
            CExpr::Bin(op, a, b) => {
                let (x, y) = (a.eval_ref(cols, n)?, b.eval_ref(cols, n)?);
                if let (FCol::Const(p), FCol::Const(q)) = (x.as_ref(), y.as_ref()) {
                    return Some(Cow::Owned(FCol::Const(bin_value(*op, p, q).ok()?)));
                }
                // Two Counts stay a Count where `bin_count` answers.
                if let (Some(p), Some(q), true) = (x.counts(), y.counts(), *op != BinOp::Div) {
                    let out: Option<Vec<i64>> = (0..n).map(|i| bin_count(*op, p.at(i), q.at(i))).collect();
                    return Some(Cow::Owned(FCol::Col(Col::Count(out?))));
                }
                // A column of several kinds goes row by row.
                let (Some(p), Some(q)) = (x.nums(), y.nums()) else {
                    return Some(Cow::Owned(by_row(n, |i| bin_value(*op, &x.value(i), &y.value(i)).ok())?));
                };
                let mut out = Vec::with_capacity(n);
                for i in 0..n {
                    out.push(bin_num(*op, p.at(i), q.at(i)).ok()?);
                }
                Cow::Owned(FCol::Col(Col::Num(out)))
            }
            CExpr::Call1(f, x) => {
                let x = x.eval_ref(cols, n)?;
                Cow::Owned(match (x.as_ref(), f) {
                    (FCol::Const(v), _) => FCol::Const(call1_value(*f, v).ok()?),
                    (FCol::Col(Col::Count(c)), Fn1::Abs) => FCol::Col(Col::Count(c.iter().map(|v| v.abs()).collect())),
                    (x, _) => match x.nums() {
                        Some(p) => {
                            let mut out = Vec::with_capacity(n);
                            for i in 0..n {
                                out.push(call1_num(*f, p.at(i)).ok()?);
                            }
                            FCol::Col(Col::Num(out))
                        }
                        None => by_row(n, |i| call1_value(*f, &x.value(i)).ok())?,
                    },
                })
            }
            CExpr::Call2(least, a, b) => {
                let (x, y) = (a.eval_ref(cols, n)?, b.eval_ref(cols, n)?);
                if let (FCol::Const(p), FCol::Const(q)) = (x.as_ref(), y.as_ref()) {
                    return Some(Cow::Owned(FCol::Const(least_greatest(*least, &[p.clone(), q.clone()]))));
                }
                Cow::Owned(by_row(n, |i| Some(least_greatest(*least, &[x.value(i), y.value(i)])))?)
            }
        })
    }

    /// The expression on one row, its slots' values in `env`, through the
    /// functions `Kernel::eval_expr` applies, in its order: `None` where an
    /// operation is partial or a slot is unbound (the row path raises it).
    pub(crate) fn value_in(&self, env: &[Option<Value>]) -> Option<Value> {
        match self {
            CExpr::Slot(s) => env.get(*s)?.clone(),
            CExpr::Const(v) => Some(v.clone()),
            CExpr::Neg(x) => neg_value(&x.value_in(env)?).ok(),
            CExpr::Bin(op, a, b) => {
                let x = a.value_in(env)?;
                bin_value(*op, &x, &b.value_in(env)?).ok()
            }
            CExpr::Call1(f, x) => call1_value(*f, &x.value_in(env)?).ok(),
            CExpr::Call2(least, a, b) => {
                let x = a.value_in(env)?;
                Some(least_greatest(*least, &[x, b.value_in(env)?]))
            }
        }
    }

    /// The rows of `n` for which `lhs op rhs` holds, by `eval::compare`:
    /// `None` when either side is partial.
    pub(crate) fn test(op: CmpOp, lhs: &CExpr, rhs: &CExpr, cols: &[Option<FCol>], n: usize) -> Option<Vec<bool>> {
        let (x, y) = (lhs.eval_ref(cols, n)?, rhs.eval_ref(cols, n)?);
        // Two numbers compare as numbers (a Count as its `f64`), as
        // `compare` does; any other pair as values.
        if let (Some(p), Some(q)) = (x.nums(), y.nums()) {
            return Some((0..n).map(|i| compare_num(op, p.at(i), q.at(i))).collect());
        }
        Some((0..n).map(|i| super::eval::compare(op, &x.value(i), &y.value(i))).collect())
    }
}

/// The column of `f` on each of `n` rows, through values: `None` when a
/// row's is.
fn by_row(n: usize, f: impl Fn(usize) -> Option<Value>) -> Option<FCol> {
    let vals: Option<Vec<Value>> = (0..n).map(f).collect();
    Some(FCol::Col(Col::of_values(vals?)))
}

/// A parameter's value as a compiled constant: a number or a time.
fn constant(v: Value) -> Option<Value> {
    matches!(v, Value::Num(_) | Value::Count(_) | Value::Time(_)).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A seeded stream of the values the arithmetic meets: numbers of
    /// every sign with zeros, NaN and infinities among them, Counts with
    /// zeros, and times.
    fn values(seed: u64) -> impl FnMut(u8) -> Value {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        move |kind| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let r = s % 1000;
            match kind % 4 {
                3 if r.is_multiple_of(2) => Value::Count(r as i64 - 500),
                3 => Value::Num((r as f64 - 500.0) / 3.0),
                0 => Value::Num(match r % 10 {
                    0 => 0.0,
                    1 => -0.0,
                    2 => f64::NAN,
                    3 => f64::INFINITY,
                    _ => (r as f64 - 500.0) / 7.0,
                }),
                1 => Value::Count(if r.is_multiple_of(5) { 0 } else { r as i64 - 500 }),
                _ => Value::Time(r as i64),
            }
        }
    }

    /// The expression on one row's values through the scalar functions, as
    /// `Kernel::eval_expr` applies them: `None` where it halts.
    fn by_row(e: &CExpr, row: &[Value]) -> Option<Value> {
        match e {
            CExpr::Slot(s) => Some(row[*s].clone()),
            CExpr::Const(v) => Some(v.clone()),
            CExpr::Neg(x) => neg_value(&by_row(x, row)?).ok(),
            CExpr::Bin(op, a, b) => bin_value(*op, &by_row(a, row)?, &by_row(b, row)?).ok(),
            CExpr::Call1(f, x) => call1_value(*f, &by_row(x, row)?).ok(),
            CExpr::Call2(least, a, b) => Some(least_greatest(*least, &[by_row(a, row)?, by_row(b, row)?])),
        }
    }

    fn same(a: &Value, b: &Value) -> bool {
        std::mem::discriminant(a) == std::mem::discriminant(b)
            && match (a, b) {
                (Value::Num(x), Value::Num(y)) => x.to_bits() == y.to_bits(),
                _ => a == b,
            }
    }

    fn column(kind: u8, n: usize, next: &mut impl FnMut(u8) -> Value) -> FCol {
        let first = next(kind);
        let mut c = Col::like(&first, n);
        c.push(&first);
        for _ in 1..n {
            c.push(&next(kind));
        }
        FCol::Col(c)
    }

    fn shapes() -> Vec<CExpr> {
        let s = |i| Box::new(CExpr::Slot(i));
        let mut out = Vec::new();
        for op in [BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div] {
            out.push(CExpr::Bin(op, s(0), s(1)));
            out.push(CExpr::Bin(op, s(0), Box::new(CExpr::Const(Value::Count(0)))));
            out.push(CExpr::Bin(op, Box::new(CExpr::Const(Value::Num(2.5))), s(1)));
            out.push(CExpr::Bin(op, Box::new(CExpr::Bin(BinOp::Mul, s(0), s(1))), Box::new(CExpr::Neg(s(0)))));
        }
        for f in [Fn1::Log, Fn1::Exp, Fn1::Sqrt, Fn1::Abs, Fn1::Sign, Fn1::Floor, Fn1::Ceil] {
            out.push(CExpr::Call1(f, s(0)));
            out.push(CExpr::Call1(f, Box::new(CExpr::Bin(BinOp::Div, s(0), s(1)))));
        }
        out.push(CExpr::Neg(s(1)));
        out.push(CExpr::Call2(true, s(0), s(1)));
        out.push(CExpr::Call2(false, s(0), Box::new(CExpr::Const(Value::Count(1)))));
        out
    }

    /// Every shape over every pairing of kinds: the column has, row by row,
    /// the bits the scalar functions give, or there is none exactly where
    /// a row's operation is partial.
    #[test]
    fn a_compiled_expression_gives_the_row_paths_bits_or_declines_where_it_halts() {
        let mut next = values(11);
        let (mut computed, mut declined) = (0, 0);
        for e in shapes() {
            for (k0, k1) in [(0, 0), (0, 1), (1, 0), (1, 1), (0, 2), (2, 1), (3, 0), (1, 3), (3, 3)] {
                for n in [1, 3, 40] {
                    let cols = vec![Some(column(k0, n, &mut next)), Some(column(k1, n, &mut next))];
                    let rows: Vec<Vec<Value>> = (0..n).map(|i| vec![cols[0].as_ref().unwrap().value(i), cols[1].as_ref().unwrap().value(i)]).collect();
                    let expected: Vec<Option<Value>> = rows.iter().map(|r| by_row(&e, r)).collect();
                    match e.eval(&cols, n) {
                        Some(out) => {
                            computed += 1;
                            for (i, want) in expected.iter().enumerate() {
                                let want = want.as_ref().unwrap_or_else(|| panic!("{:?} on {:?}: a column where row {} halts", e, rows[i], i));
                                assert!(same(&out.value(i), want), "{:?} on {:?}: {:?} against {:?}", e, rows[i], out.value(i), want);
                            }
                        }
                        None => {
                            declined += 1;
                            assert!(expected.iter().any(|v| v.is_none()), "{:?} declined though no row halts: {:?}", e, rows);
                        }
                    }
                    // A constant operand: the whole column through the same function.
                    let constant = vec![Some(cols[0].clone().unwrap()), Some(FCol::Const(rows[0][1].clone()))];
                    let want: Vec<Option<Value>> = rows.iter().map(|r| by_row(&e, &[r[0].clone(), rows[0][1].clone()])).collect();
                    match e.eval(&constant, n) {
                        Some(out) => (0..n).for_each(|i| assert!(same(&out.value(i), want[i].as_ref().expect("no halt")), "{:?}", e)),
                        None => assert!(want.iter().any(|v| v.is_none()), "{:?} declined on a constant", e),
                    }
                }
            }
        }
        assert!(computed > 100 && declined > 20, "{} computed, {} declined", computed, declined);
    }

    #[test]
    fn a_test_keeps_the_rows_compare_keeps() {
        let mut next = values(5);
        for op in [CmpOp::Lt, CmpOp::Le, CmpOp::Eq, CmpOp::Gt, CmpOp::Ge] {
            for (k0, k1) in [(0, 0), (0, 1), (1, 1), (2, 2), (2, 0), (3, 1), (3, 3)] {
                let n = 50;
                let cols = vec![Some(column(k0, n, &mut next)), Some(column(k1, n, &mut next))];
                let kept = CExpr::test(op, &CExpr::Slot(0), &CExpr::Slot(1), &cols, n).unwrap();
                for (i, k) in kept.iter().enumerate() {
                    let (a, b) = (cols[0].as_ref().unwrap().value(i), cols[1].as_ref().unwrap().value(i));
                    assert_eq!(*k, super::super::eval::compare(op, &a, &b), "{:?} {:?} {:?}", a, op, b);
                }
            }
        }
        // A partial side is no test.
        let cols = vec![Some(FCol::Col(Col::Num(vec![1.0]))), Some(FCol::Col(Col::Num(vec![0.0])))];
        assert!(CExpr::test(CmpOp::Lt, &CExpr::Bin(BinOp::Div, Box::new(CExpr::Slot(0)), Box::new(CExpr::Slot(1))), &CExpr::Slot(0), &cols, 1).is_none());
    }
}
