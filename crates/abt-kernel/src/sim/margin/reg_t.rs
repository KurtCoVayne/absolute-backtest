//! `reg_t`, model 2 of the parent module, whose documentation states the
//! rules and their sources. This module recognises one underlying's
//! strategies as the minimum over admissible pairings (K-5.6), a linear
//! program since quantities are divisible:
//!
//! - variables: per short option leg, its naked units; per admissible
//!   (short, long option) pair, its spread units; per (short call, short
//!   put) of one multiplier, its straddle units; per short option the stock
//!   can cover (long shares for a call, short shares for a put), its covered
//!   units;
//! - constraints: a short leg's units sum to its |qty|; a long option's
//!   spread units are at most its |qty|, the rest paid in full; covered
//!   units consume m shares each, at most the shares held;
//! - objective: the total maintenance requirement, the long options'
//!   payments entering as a constant less what their spreads replace.
//!
//! The constraint matrix is the incidence matrix of a bipartite graph (short
//! calls and long puts on one side, short puts and long calls on the other,
//! the stock on the side its covered legs are not) with unit columns
//! appended. With one multiplier and the stock a whole number of m shares it
//! is totally unimodular once the stock's row is divided by m, so the
//! optimum is then a whole allocation, which is what the brute-force tests
//! compare it with. When the program cannot be solved the legs stay naked,
//! a requirement never below the minimum.

use std::collections::BTreeMap;

use super::lp::{self, Cmp, Constraint};
use super::{underlying_of, Context, Holding, IndexClass, Requirement, Source, Terms, Underlying};
use crate::functions::Right;

/// Nine months, as 274 days: a long option expiring later has loan value.
const NINE_MONTHS: i64 = 274 * crate::kernel::time::DAY;

/// The rows of the instruments `reg_t` prices, grouped by underlying.
pub(super) fn rows(holdings: &[Holding], ctx: &Context) -> Vec<Requirement> {
    let mut groups: BTreeMap<u32, Vec<&Holding>> = BTreeMap::new();
    for h in holdings {
        groups.entry(underlying_of(h)).or_default().push(h);
    }
    groups.iter().flat_map(|(u, group)| rows_of_group(group, ctx.underlyings.get(u), ctx.t)).collect()
}

/// An option leg: its terms, its mark and its |qty|.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Leg {
    instrument: u32,
    right: Right,
    strike: f64,
    expiry: i64,
    m: f64,
    mark: f64,
    q: f64,
}

/// A short leg with the requirement of one contract held naked.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Short {
    leg: Leg,
    naked: f64,
}

/// A long leg with what one contract costs paid for.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Long {
    leg: Leg,
    paid: f64,
}

/// One underlying's option legs and its shares (signed).
#[derive(Clone, Debug, Default, PartialEq)]
struct Book {
    shorts: Vec<Short>,
    longs: Vec<Long>,
    shares: f64,
}

/// A use of a short leg's units other than naked.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pair {
    Spread { short: usize, long: usize },
    Straddle { call: usize, put: usize },
    Covered { short: usize },
}

/// An admissible pairing: its requirement per unit and the short leg the
/// requirement is attributed to.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Candidate {
    pair: Pair,
    cost: f64,
    owner: usize,
}

impl Candidate {
    fn uses_short(&self, i: usize) -> bool {
        match self.pair {
            Pair::Spread { short, .. } | Pair::Covered { short } => short == i,
            Pair::Straddle { call, put } => call == i || put == i,
        }
    }

    fn uses_long(&self, j: usize) -> bool {
        matches!(self.pair, Pair::Spread { long, .. } if long == j)
    }
}

fn rows_of_group(holdings: &[&Holding], underlying: Option<&Underlying>, t: i64) -> Vec<Requirement> {
    // The selection admits an option only when its underlying has a price.
    let (s, class) = underlying.map_or((f64::NAN, IndexClass::Equity), |u| (u.price, u.class));
    let mut rows = Vec::new();
    let mut book = Book::default();
    for h in holdings {
        match h.terms {
            Terms::Stock => {
                book.shares += h.qty;
                rows.push(stock(h));
            }
            Terms::Option {
                right, strike, expiry, multiplier, ..
            } => {
                let leg = Leg {
                    instrument: h.instrument,
                    right,
                    strike,
                    expiry,
                    m: multiplier,
                    mark: h.mark,
                    q: h.qty.abs(),
                };
                if h.qty < 0.0 {
                    book.shorts.push(Short { leg, naked: naked(&leg, s, class) });
                } else {
                    book.longs.push(Long { leg, paid: paid(&leg, t) });
                }
            }
            // The selection never gives a future to reg_t.
            Terms::Future { .. } => {}
        }
    }
    let pairs = candidates(&book);
    let units = solve(&book, &pairs);
    rows.extend(book.rows(&pairs, &units));
    rows
}

/// A stock's own requirement: initial 50 % (Regulation T), maintenance 25 %
/// long, short the greater of 30 % and $5 a share at $5 or more, of 100 %
/// and $2.50 a share below (FINRA 4210(c)).
fn stock(h: &Holding) -> Requirement {
    let shares = h.qty.abs();
    let mv = shares * h.mark.abs();
    let maintenance = if h.qty > 0.0 {
        0.25 * mv
    } else if h.mark >= 5.0 {
        (0.30 * mv).max(5.0 * shares)
    } else {
        mv.max(2.5 * shares)
    };
    Requirement {
        instrument: h.instrument,
        initial: 0.5 * mv,
        maintenance,
        source: Source::RegT,
    }
}

/// One naked short contract: m × (premium + max(r·S − OTM, floor)).
fn naked(leg: &Leg, s: f64, class: IndexClass) -> f64 {
    let rate = if class == IndexClass::BroadIndex { 0.15 } else { 0.20 };
    let (otm, floor) = match leg.right {
        Right::Call => ((leg.strike - s).max(0.0), 0.10 * s),
        Right::Put => ((s - leg.strike).max(0.0), 0.10 * leg.strike),
    };
    leg.m * (leg.mark + (rate * s - otm).max(floor))
}

/// One long contract paid for: in full when it expires within nine months
/// of `t`, 75 % beyond.
fn paid(leg: &Leg, t: i64) -> f64 {
    let part = if leg.expiry.saturating_sub(t) <= NINE_MONTHS { 1.0 } else { 0.75 };
    part * leg.m * leg.mark
}

/// One unit of a spread, when the pair is one: its maximum loss from the
/// marks.
fn spread(short: &Leg, long: &Leg) -> Option<f64> {
    if short.right != long.right || short.m != long.m || long.expiry < short.expiry {
        return None;
    }
    let width = match short.right {
        Right::Call => (long.strike - short.strike).max(0.0),
        Right::Put => (short.strike - long.strike).max(0.0),
    };
    Some((short.m * (long.mark - short.mark + width)).max(0.0))
}

/// Every admissible pairing of the book's legs.
fn candidates(book: &Book) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (i, s) in book.shorts.iter().enumerate() {
        for (j, l) in book.longs.iter().enumerate() {
            if let Some(cost) = spread(&s.leg, &l.leg) {
                out.push(Candidate {
                    pair: Pair::Spread { short: i, long: j },
                    cost,
                    owner: i,
                });
            }
        }
    }
    let side = |right: Right| book.shorts.iter().enumerate().filter(move |(_, s)| s.leg.right == right);
    for (c, call) in side(Right::Call) {
        for (p, put) in side(Right::Put).filter(|(_, put)| put.leg.m == call.leg.m) {
            let (cost, owner) = if call.naked >= put.naked {
                (call.naked + put.leg.m * put.leg.mark, c)
            } else {
                (put.naked + call.leg.m * call.leg.mark, p)
            };
            out.push(Candidate {
                pair: Pair::Straddle { call: c, put: p },
                cost,
                owner,
            });
        }
    }
    let covering = if book.shares > 0.0 {
        Some(Right::Call)
    } else if book.shares < 0.0 {
        Some(Right::Put)
    } else {
        None
    };
    for (i, _) in book.shorts.iter().enumerate().filter(|(_, s)| Some(s.leg.right) == covering) {
        out.push(Candidate {
            pair: Pair::Covered { short: i },
            cost: 0.0,
            owner: i,
        });
    }
    out
}

/// The units of each candidate at the minimum; none when the program cannot
/// be solved, every leg then staying naked.
fn solve(book: &Book, pairs: &[Candidate]) -> Vec<f64> {
    if pairs.is_empty() {
        return Vec::new();
    }
    let ns = book.shorts.len();
    let mut cost: Vec<f64> = book.shorts.iter().map(|s| s.naked).collect();
    cost.extend(pairs.iter().map(|p| match p.pair {
        Pair::Spread { long, .. } => p.cost - book.longs[long].paid,
        _ => p.cost,
    }));
    let over = |f: &dyn Fn(&Candidate) -> f64| -> Vec<f64> {
        let mut coef = vec![0.0; ns];
        coef.extend(pairs.iter().map(f));
        coef
    };
    let mut rows = Vec::new();
    for (i, s) in book.shorts.iter().enumerate() {
        let mut coef = over(&|p| if p.uses_short(i) { 1.0 } else { 0.0 });
        coef[i] = 1.0;
        rows.push(Constraint { coef, cmp: Cmp::Eq, rhs: s.leg.q });
    }
    for (j, l) in book.longs.iter().enumerate() {
        let coef = over(&|p| if p.uses_long(j) { 1.0 } else { 0.0 });
        if coef.iter().any(|&a| a != 0.0) {
            rows.push(Constraint { coef, cmp: Cmp::Le, rhs: l.leg.q });
        }
    }
    let coef = over(&|p| if let Pair::Covered { short } = p.pair { book.shorts[short].leg.m } else { 0.0 });
    if coef.iter().any(|&a| a != 0.0) {
        rows.push(Constraint {
            coef,
            cmp: Cmp::Le,
            rhs: book.shares.abs(),
        });
    }
    match lp::minimize(&cost, &rows) {
        Ok(solution) => solution.x[ns..].iter().map(|u| u.max(0.0)).collect(),
        Err(_) => vec![0.0; pairs.len()],
    }
}

impl Book {
    /// The option legs' rows at `units` of each candidate: a short pays its
    /// naked rest and what it is the owner of, a long its unpaired rest.
    fn rows(&self, pairs: &[Candidate], units: &[f64]) -> Vec<Requirement> {
        let mut used_short = vec![0.0; self.shorts.len()];
        let mut used_long = vec![0.0; self.longs.len()];
        let mut owned = vec![0.0; self.shorts.len()];
        for (p, &u) in pairs.iter().zip(units) {
            owned[p.owner] += p.cost * u;
            match p.pair {
                Pair::Spread { short, long } => {
                    used_short[short] += u;
                    used_long[long] += u;
                }
                Pair::Straddle { call, put } => {
                    used_short[call] += u;
                    used_short[put] += u;
                }
                Pair::Covered { short } => used_short[short] += u,
            }
        }
        let shorts = self
            .shorts
            .iter()
            .zip(used_short.iter().zip(&owned))
            .map(|(s, (used, owned))| option_row(s.leg.instrument, s.naked * (s.leg.q - used).max(0.0) + owned));
        let longs = self.longs.iter().zip(&used_long).map(|(l, used)| option_row(l.leg.instrument, l.paid * (l.leg.q - used).max(0.0)));
        shorts.chain(longs).collect()
    }
}

/// An option's row: initial = maintenance.
fn option_row(instrument: u32, requirement: f64) -> Requirement {
    Requirement {
        instrument,
        initial: requirement,
        maintenance: requirement,
        source: Source::RegT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = crate::kernel::time::DAY;

    fn leg(right: Right, strike: f64, days: i64, mark: f64, q: f64) -> Leg {
        Leg {
            instrument: 0,
            right,
            strike,
            expiry: days * DAY,
            m: 100.0,
            mark,
            q,
        }
    }

    #[test]
    fn naked_contracts() {
        // K-9.3 case 14's put, S = 100, K = 95, premium 1.50: OTM 5, max(20 −
        // 5, 9.5) = 15, (1.50 + 15) × 100 = 1,650 a contract.
        assert_eq!(naked(&leg(Right::Put, 95.0, 30, 1.5, 2.0), 100.0, IndexClass::Equity), 1650.0);
        // A call 10 OTM, S = 100, K = 110, premium 0.80: max(20 − 10, 10) =
        // 10, (0.80 + 10) × 100 = 1,080; the floor binds from 10 OTM on.
        assert_eq!(naked(&leg(Right::Call, 110.0, 30, 0.8, 1.0), 100.0, IndexClass::Equity), 1080.0);
        // Broad-based: S = 4,000, K = 3,800 put at 20: max(600 − 200, 380) =
        // 400, (20 + 400) × 100 = 42,000; narrow-based keeps 20 %: max(800 −
        // 200, 380) = 600, 62,000.
        assert_eq!(naked(&leg(Right::Put, 3800.0, 30, 20.0, 1.0), 4000.0, IndexClass::BroadIndex), 42_000.0);
        assert_eq!(naked(&leg(Right::Put, 3800.0, 30, 20.0, 1.0), 4000.0, IndexClass::NarrowIndex), 62_000.0);
    }

    #[test]
    fn long_options_have_loan_value_beyond_nine_months() {
        // Premium 10, m = 100: 1,000 within 274 days, 75 % = 750 beyond.
        assert_eq!(paid(&leg(Right::Call, 100.0, 274, 10.0, 1.0), 0), 1000.0);
        assert_eq!(paid(&leg(Right::Call, 100.0, 275, 10.0, 1.0), 0), 750.0);
    }

    #[test]
    fn spread_admissibility() {
        let short = leg(Right::Put, 100.0, 30, 3.0, 1.0);
        // (1.20 − 3.00 + 5) × 100 = 320.
        assert_eq!(spread(&short, &leg(Right::Put, 95.0, 30, 1.2, 1.0)).map(|c| (c * 1e6).round() / 1e6), Some(320.0));
        // A long expiring first, another right, another multiplier: none.
        assert_eq!(spread(&short, &leg(Right::Put, 95.0, 29, 1.2, 1.0)), None);
        assert_eq!(spread(&short, &leg(Right::Call, 95.0, 30, 1.2, 1.0)), None);
        assert_eq!(
            spread(
                &short,
                &Leg {
                    m: 150.0,
                    ..leg(Right::Put, 95.0, 30, 1.2, 1.0)
                }
            ),
            None
        );
        // A put debit spread, long 100 at 3.00 over short 95 at 1.20: the
        // width is 0, so the net debit, (3.00 − 1.20) × 100 = 180.
        let debit = spread(&leg(Right::Put, 95.0, 30, 1.2, 1.0), &leg(Right::Put, 100.0, 30, 3.0, 1.0));
        assert_eq!(debit.map(|c| (c * 1e6).round() / 1e6), Some(180.0));
    }

    /// SplitMix64: a small deterministic generator.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            (z ^ (z >> 31)) % n
        }
    }

    /// A random book of one to four legs, quantities 1 to 3, one multiplier,
    /// and −300 to 300 shares in whole contracts' worth.
    fn random_book(rng: &mut Rng) -> Book {
        let s = 90.0 + rng.below(21) as f64;
        let class = [IndexClass::Equity, IndexClass::BroadIndex, IndexClass::NarrowIndex][rng.below(3) as usize];
        let mut book = Book {
            shares: 100.0 * (rng.below(7) as f64 - 3.0),
            ..Book::default()
        };
        for i in 0..=rng.below(4) {
            let right = if rng.below(2) == 0 { Right::Call } else { Right::Put };
            let days = [30, 60, 400][rng.below(3) as usize];
            let l = Leg {
                instrument: i as u32,
                ..leg(right, s - 10.0 + 5.0 * rng.below(5) as f64, days, 0.5 + 0.25 * rng.below(30) as f64, 1.0 + rng.below(3) as f64)
            };
            if rng.below(2) == 0 {
                book.shorts.push(Short { leg: l, naked: naked(&l, s, class) });
            } else {
                book.longs.push(Long { leg: l, paid: paid(&l, 0) });
            }
        }
        book
    }

    /// The objective at whole units of each candidate, None when they do not
    /// fit the legs.
    fn objective(book: &Book, pairs: &[Candidate], units: &[f64]) -> Option<f64> {
        let mut total = 0.0;
        for (i, s) in book.shorts.iter().enumerate() {
            let used: f64 = pairs.iter().zip(units).filter(|(p, _)| p.uses_short(i)).map(|(_, u)| u).sum();
            if used > s.leg.q {
                return None;
            }
            total += s.naked * (s.leg.q - used);
        }
        for (j, l) in book.longs.iter().enumerate() {
            let used: f64 = pairs.iter().zip(units).filter(|(p, _)| p.uses_long(j)).map(|(_, u)| u).sum();
            if used > l.leg.q {
                return None;
            }
            total += l.paid * (l.leg.q - used);
        }
        let covered: f64 = pairs.iter().zip(units).filter(|(p, _)| matches!(p.pair, Pair::Covered { .. })).map(|(_, u)| 100.0 * u).sum();
        (covered <= book.shares.abs()).then(|| total + pairs.iter().zip(units).map(|(p, u)| p.cost * u).sum::<f64>())
    }

    /// The most whole units a candidate could take: the least |qty| of its
    /// legs (and the contracts the stock covers).
    fn cap(book: &Book, p: &Candidate) -> f64 {
        match p.pair {
            Pair::Spread { short, long } => book.shorts[short].leg.q.min(book.longs[long].leg.q),
            Pair::Straddle { call, put } => book.shorts[call].leg.q.min(book.shorts[put].leg.q),
            Pair::Covered { short } => book.shorts[short].leg.q.min(book.shares.abs() / 100.0),
        }
    }

    /// The least objective over every whole allocation of units to the
    /// candidates.
    fn brute(book: &Book, pairs: &[Candidate], units: &mut Vec<f64>) -> f64 {
        let Some(p) = pairs.get(units.len()) else {
            return objective(book, pairs, units).unwrap_or(f64::INFINITY);
        };
        let mut best = f64::INFINITY;
        for u in 0..=cap(book, p) as usize {
            units.push(u as f64);
            best = best.min(brute(book, pairs, units));
            units.pop();
        }
        best
    }

    #[test]
    fn the_program_finds_the_least_whole_pairing() {
        let mut rng = Rng(11);
        let mut paired = 0;
        for _ in 0..600 {
            let book = random_book(&mut rng);
            let pairs = candidates(&book);
            let units = solve(&book, &pairs);
            let total: f64 = book.rows(&pairs, &units).iter().map(|r| r.maintenance).sum();
            let best = brute(&book, &pairs, &mut Vec::new());
            assert!((total - best).abs() <= 1e-6 * (1.0 + best), "{book:?}: the program gives {total}, enumeration {best}");
            let naked = objective(&book, &pairs, &vec![0.0; pairs.len()]).unwrap_or(f64::INFINITY);
            paired += usize::from(best < naked - 1e-6);
        }
        assert!(paired > 100, "only {paired} books gained by pairing");
    }
}
