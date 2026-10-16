//! What the time-series tests of the starting set share (Q-4.4): reading a
//! series or a system of series from the input, their parameters, the
//! augmented Dickey–Fuller regression with its lag order chosen by AIC, and
//! MacKinnon's tables of the Dickey–Fuller distribution that `adf` and
//! `engle_granger` read: the response surfaces of MacKinnon (2010) for
//! critical values, and the approximate asymptotic distribution functions
//! of MacKinnon (1994) for p-values.
//!
//! MacKinnon (1996) computes p-values from response-surface files whose
//! terms forbid incorporating them into a program without the author's
//! written consent (the header of his `urcdist` routines). The p-values here
//! are therefore the 1994 approximations, whose coefficients the paper
//! prints: asymptotic, for one to six variables. The critical values, from
//! the 2010 response surfaces, cover one to twelve.

use crate::tests::dist;
use crate::tests::framework::{param, param_f64, param_usize, Param, Params, Table, Test, TestError};
use crate::tests::regression::{ols, Covariance, Ols};

/// The one series a single-series test reads: the first column of its one
/// input table, at least `min` values, every one finite.
pub fn series<'a>(tables: &'a [Table], test: &str, min: usize) -> Result<&'a [f64], TestError> {
    let table = match tables {
        [t] => t,
        _ => return Err(TestError::Input(format!("{} takes one table, not {}", test, tables.len()))),
    };
    let x = table.first()?;
    finite(x, test)?;
    if x.len() < min {
        return Err(TestError::Precondition(format!("{} needs at least {} observations, not {}", test, min, x.len())));
    }
    Ok(x)
}

/// The series of a system: every column of the one input table, in name
/// order, of one length of at least `min`, every value finite.
pub fn system<'a>(tables: &'a [Table], test: &str, min: usize) -> Result<Vec<(&'a str, &'a [f64])>, TestError> {
    let table = match tables {
        [t] => t,
        _ => return Err(TestError::Input(format!("{} takes one table, not {}", test, tables.len()))),
    };
    let columns: Vec<(&str, &[f64])> = table.columns.iter().map(|(n, c)| (n.as_str(), c.as_slice())).collect();
    let n = columns.first().map(|(_, c)| c.len()).unwrap_or(0);
    for (name, c) in &columns {
        if c.len() != n {
            return Err(TestError::Input(format!("{}: column `{}` has {} rows, not {}", test, name, c.len(), n)));
        }
        finite(c, &format!("{} column `{}`", test, name))?;
    }
    if n < min {
        return Err(TestError::Precondition(format!("{} needs at least {} observations, not {}", test, min, n)));
    }
    Ok(columns)
}

/// An input error naming the first row of `x` that is not a finite number.
fn finite(x: &[f64], what: &str) -> Result<(), TestError> {
    match x.iter().position(|v| !v.is_finite()) {
        Some(i) => Err(TestError::Input(format!("{}: row {} is not a finite number", what, i))),
        None => Ok(()),
    }
}

/// A non-negative integer parameter: its value or declared default, `None`
/// when neither is given, an error when it is set to anything else.
pub fn usize_param(test: &dyn Test, params: &Params, name: &str) -> Result<Option<usize>, TestError> {
    match param(test, params, name) {
        None => Ok(None),
        Some(p) => param_usize(test, params, name)
            .map(Some)
            .ok_or_else(|| TestError::Input(format!("`{}` is a non-negative integer, not {}", name, p))),
    }
}

/// A numeric parameter: its value or declared default, `None` when
/// neither is given, an error when it is not a finite number.
pub fn f64_param(test: &dyn Test, params: &Params, name: &str) -> Result<Option<f64>, TestError> {
    match param(test, params, name) {
        None => Ok(None),
        Some(p) => param_f64(test, params, name)
            .filter(|v| v.is_finite())
            .map(Some)
            .ok_or_else(|| TestError::Input(format!("`{}` is a number, not {}", name, p))),
    }
}

/// A text parameter: its value or declared default, `None` when neither is
/// given, an error when it is not text.
pub fn text_param<'a>(test: &'a dyn Test, params: &'a Params, name: &str) -> Result<Option<&'a str>, TestError> {
    match param(test, params, name) {
        None => Ok(None),
        Some(Param::Text(s)) => Ok(Some(s.as_str())),
        Some(p) => Err(TestError::Input(format!("`{}` is text, not {}", name, p))),
    }
}

/// A boolean parameter: its value or declared default, `None` when neither
/// is given, an error when it is not a boolean.
pub fn bool_param(test: &dyn Test, params: &Params, name: &str) -> Result<Option<bool>, TestError> {
    match param(test, params, name) {
        None => Ok(None),
        Some(Param::Bool(b)) => Ok(Some(*b)),
        Some(p) => Err(TestError::Input(format!("`{}` is true or false, not {}", name, p))),
    }
}

/// The deterministic terms of a Dickey–Fuller regression, by MacKinnon's
/// names: none (`n`), a constant (`c`), a constant and a linear trend
/// (`ct`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deterministic {
    None,
    Constant,
    Trend,
}

impl Deterministic {
    /// The deterministic terms a test's `trend` parameter names: `c` (the
    /// default) or `ct`.
    pub fn from_trend(test: &dyn Test, params: &Params) -> Result<Deterministic, TestError> {
        match text_param(test, params, "trend")? {
            None | Some("c") => Ok(Deterministic::Constant),
            Some("ct") => Ok(Deterministic::Trend),
            Some(other) => Err(TestError::Input(format!("`trend` is `c` or `ct`, not `{}`", other))),
        }
    }

    /// The number of deterministic regressors.
    pub fn terms(self) -> usize {
        match self {
            Deterministic::None => 0,
            Deterministic::Constant => 1,
            Deterministic::Trend => 2,
        }
    }

    /// MacKinnon's name of the case.
    pub fn name(self) -> &'static str {
        match self {
            Deterministic::None => "n",
            Deterministic::Constant => "c",
            Deterministic::Trend => "ct",
        }
    }

    /// The deterministic columns over `rows` observations: a constant, and
    /// the trend 1, 2, …, rows.
    pub fn columns(self, rows: usize) -> Vec<Vec<f64>> {
        let mut c = Vec::new();
        if self != Deterministic::None {
            c.push(vec![1.0; rows]);
        }
        if self == Deterministic::Trend {
            c.push((1..=rows).map(|t| t as f64).collect());
        }
        c
    }
}

/// How the augmentation's lag order is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LagChoice {
    /// This many lagged differences.
    Fixed(usize),
    /// The order from 0 to this maximum with the smallest AIC, the orders
    /// compared on the sample the largest one leaves.
    Aic(usize),
}

/// Schwert's (1989) maximum lag order for `n` observations,
/// ⌊12 (n/100)^(1/4)⌋.
pub fn schwert_lags(n: usize) -> usize {
    (12.0 * (n as f64 / 100.0).powf(0.25)).floor() as usize
}

/// The lag choice of a Dickey–Fuller test's parameters over a series of
/// `n` values: `lags` fixes the order; otherwise AIC chooses it up to
/// `max_lags`, by default Schwert's rule bounded so that the largest
/// regression keeps about half the sample.
pub fn lag_choice(test: &dyn Test, params: &Params, n: usize, det: Deterministic) -> Result<LagChoice, TestError> {
    if let Some(p) = usize_param(test, params, "lags")? {
        return Ok(LagChoice::Fixed(p));
    }
    let max = match usize_param(test, params, "max_lags")? {
        Some(m) => m,
        None => schwert_lags(n).min((n / 2).saturating_sub(det.terms() + 1)),
    };
    Ok(LagChoice::Aic(max))
}

/// An augmented Dickey–Fuller regression: Δx_t on x_{t−1}, the lagged
/// differences Δx_{t−1}, …, Δx_{t−p} and the deterministic terms.
#[derive(Clone, Debug, PartialEq)]
pub struct DickeyFuller {
    /// τ, the t-statistic of the lagged level's coefficient.
    pub tau: f64,
    /// γ, the lagged level's coefficient: the sum of the level's
    /// autoregressive coefficients less one.
    pub gamma: f64,
    /// The number of lagged differences, p.
    pub lags: usize,
    /// The observations of the regression, n − 1 − p.
    pub nobs: usize,
}

/// The regression with `p` lagged differences over the rows `start..n` of
/// `x` (`start` > p, so that every lag exists).
fn df_regression(x: &[f64], det: Deterministic, p: usize, start: usize) -> Result<Ols, TestError> {
    let n = x.len();
    let rows = n.saturating_sub(start);
    let k = 1 + p + det.terms();
    if rows <= k {
        return Err(TestError::Precondition(format!(
            "a Dickey–Fuller regression with {} lags has {} observations for {} coefficients",
            p, rows, k
        )));
    }
    let d = |t: usize| x[t] - x[t - 1];
    let y: Vec<f64> = (start..n).map(d).collect();
    let mut columns: Vec<Vec<f64>> = Vec::with_capacity(k);
    columns.push((start..n).map(|t| x[t - 1]).collect());
    for j in 1..=p {
        columns.push((start..n).map(|t| d(t - j)).collect());
    }
    columns.extend(det.columns(rows));
    ols(&columns, &y, &Covariance::Iid).map_err(TestError::Numeric)
}

/// The augmented Dickey–Fuller regression of `x` with the deterministic
/// terms `det` and the lag order `choice` gives. Under `Aic`, the orders
/// 0..=max are compared by T ln(RSS/T) + 2k on the common sample the
/// largest order leaves (ties to the smaller order), and the chosen order is
/// refitted on all the observations it allows.
pub fn dickey_fuller(x: &[f64], det: Deterministic, choice: LagChoice) -> Result<DickeyFuller, TestError> {
    let p = match choice {
        LagChoice::Fixed(p) => p,
        LagChoice::Aic(max) => {
            let mut best = (f64::INFINITY, 0);
            for p in 0..=max {
                let fit = df_regression(x, det, p, max + 1)?;
                let t = fit.n as f64;
                let aic = t * (fit.rss / t).ln() + 2.0 * fit.p as f64;
                if aic < best.0 {
                    best = (aic, p);
                }
            }
            best.1
        }
    };
    let fit = df_regression(x, det, p, p + 1)?;
    if fit.rss.is_nan() || fit.rss <= 0.0 || !fit.t[0].is_finite() {
        return Err(TestError::Numeric("the Dickey–Fuller regression fits the series exactly".into()));
    }
    Ok(DickeyFuller {
        tau: fit.t[0],
        gamma: fit.beta[0],
        lags: p,
        nobs: fit.n,
    })
}

/// MacKinnon's (2010) critical values of τ at 1, 5 and 10 % for `n`
/// integrated variables (one for a unit-root test, the number of series for
/// an Engle–Granger test; up to twelve) with the deterministic terms of the
/// (cointegrating) regression: the response surface
/// β∞ + β₁/T + β₂/T² + β₃/T³ at `t` = T observations, or β∞ when `t` is
/// `None`. `None` beyond twelve variables or without deterministic terms.
pub fn mackinnon_critical(n: usize, det: Deterministic, t: Option<f64>) -> Option<[f64; 3]> {
    let table = match det {
        Deterministic::Constant => &SURFACE_C,
        Deterministic::Trend => &SURFACE_CT,
        Deterministic::None => return None,
    };
    let rows = table.get(n.checked_sub(1)?)?;
    let mut cv = [0.0; 3];
    for (c, b) in cv.iter_mut().zip(rows) {
        *c = match t {
            None => b[0],
            Some(t) => b[0] + b[1] / t + b[2] / (t * t) + b[3] / (t * t * t),
        };
    }
    Some(cv)
}

/// MacKinnon's (1994) approximate asymptotic p-value of a Dickey–Fuller τ
/// for `n` integrated variables with the deterministic terms of the
/// (cointegrating) regression: Φ of a quadratic in τ in the left tail
/// (τ ≤ τ*), of a cubic elsewhere, 0 below τ_min and 1 above τ_max. `None`
/// where the paper has no fit: more than six variables, or no deterministic
/// terms.
pub fn mackinnon_p(tau: f64, n: usize, det: Deterministic) -> Option<f64> {
    let (star, min, max, small, large) = match det {
        Deterministic::Constant => (&TAU_STAR_C, &TAU_MIN_C, &TAU_MAX_C, &SMALL_P_C, &LARGE_P_C),
        Deterministic::Trend => (&TAU_STAR_CT, &TAU_MIN_CT, &TAU_MAX_CT, &SMALL_P_CT, &LARGE_P_CT),
        Deterministic::None => return None,
    };
    let i = n.checked_sub(1).filter(|i| *i < 6)?;
    if tau > max[i] {
        return Some(1.0);
    }
    if tau < min[i] {
        return Some(0.0);
    }
    let z = if tau <= star[i] {
        let c = small[i];
        c[0] + tau * (c[1] + tau * c[2])
    } else {
        let c = large[i];
        c[0] + tau * (c[1] + tau * (c[2] + tau * c[3]))
    };
    Some(dist::norm_cdf(z))
}

/// The critical values as a note: `1 % a, 5 % b, 10 % c`.
pub fn critical_note(cv: &[f64; 3]) -> String {
    format!("critical values 1 % {}, 5 % {}, 10 % {}", cv[0], cv[1], cv[2])
}

// MacKinnon (2010), "Critical values for cointegration tests", Queen's
// Economics Department Working Paper 1227, Table 2: the response-surface
// coefficients (β∞, β₁, β₂, β₃) of the τ statistic for N = 1, …, 12
// variables at 1, 5 and 10 %, as transcribed in Schaffer's `egranger` for
// Stata. statsmodels' copy differs in four coefficients of the constant
// case (N = 2 at 1 % β₂, N = 3 at 5 % β₁, N = 5 at 5 % β₁, N = 7 at 10 %
// β₂ and β₃); the paper confirms the values here for the first two.
const SURFACE_C: [[[f64; 4]; 3]; 12] = [
    [[-3.43035, -6.5393, -16.786, -79.433], [-2.86154, -2.8903, -4.234, -40.04], [-2.56677, -1.5384, -2.809, 0.0]],
    [[-3.89644, -10.9519, -22.527, 0.0], [-3.33613, -6.1101, -6.823, 0.0], [-3.04445, -4.2412, -2.72, 0.0]],
    [[-4.29374, -14.4354, -33.195, 47.433], [-3.74066, -8.5631, -10.852, 27.982], [-3.45218, -6.2143, -3.718, 0.0]],
    [[-4.64332, -18.1031, -37.972, 0.0], [-4.096, -11.2349, -11.175, 0.0], [-3.8102, -8.3931, -4.137, 0.0]],
    [[-4.95756, -21.8883, -45.142, 0.0], [-4.41519, -14.0406, -12.575, 0.0], [-4.13157, -10.7417, -3.784, 0.0]],
    [[-5.24568, -25.6688, -57.737, 88.639], [-4.70693, -16.9178, -17.492, 60.007], [-4.42501, -13.1875, -5.104, 27.877]],
    [[-5.51233, -29.576, -69.398, 164.295], [-4.97684, -19.9021, -22.045, 110.761], [-4.69648, -15.7315, -6.922, 67.721]],
    [[-5.76202, -33.5258, -82.189, 256.289], [-5.22924, -23.0023, -24.646, 144.479], [-4.95007, -18.3959, -7.344, 94.872]],
    [[-5.99742, -37.6572, -87.365, 248.316], [-5.46697, -26.2057, -26.627, 176.382], [-5.18897, -21.1377, -9.484, 172.704]],
    [[-6.22103, -41.7154, -102.68, 389.33], [-5.69244, -29.4521, -30.994, 251.016], [-5.41533, -24.0006, -7.514, 163.049]],
    [[-6.43377, -46.0084, -106.809, 352.752], [-5.90714, -32.8336, -30.275, 249.994], [-5.63086, -26.9693, -4.083, 151.427]],
    [[-6.6379, -50.2095, -124.156, 579.622], [-6.11279, -36.2681, -32.505, 314.802], [-5.83724, -29.9864, -2.686, 184.116]],
];

const SURFACE_CT: [[[f64; 4]; 3]; 12] = [
    [[-3.95877, -9.0531, -28.428, -134.155], [-3.41049, -4.3904, -9.036, -45.374], [-3.12705, -2.5856, -3.925, -22.38]],
    [[-4.32762, -15.4387, -35.679, 0.0], [-3.78057, -9.5106, -12.074, 0.0], [-3.49631, -7.0815, -7.538, 21.892]],
    [[-4.66305, -18.7688, -49.793, 104.244], [-4.1189, -11.8922, -19.031, 77.332], [-3.83511, -9.0723, -8.504, 35.403]],
    [[-4.9694, -22.4694, -52.599, 51.314], [-4.42871, -14.5876, -18.228, 39.647], [-4.14633, -11.25, -9.873, 54.109]],
    [[-5.25276, -26.2183, -59.631, 50.646], [-4.71537, -17.3569, -22.66, 91.359], [-4.43422, -13.6078, -10.238, 76.781]],
    [[-5.51727, -29.976, -75.222, 202.253], [-4.98228, -20.305, -25.224, 132.03], [-4.70233, -16.1253, -9.836, 94.272]],
    [[-5.76537, -33.9165, -84.312, 245.394], [-5.23299, -23.3328, -28.955, 182.342], [-4.95405, -18.7352, -10.168, 120.575]],
    [[-6.00003, -37.8892, -96.428, 335.92], [-5.46971, -26.4771, -31.034, 220.165], [-5.19183, -21.4328, -10.726, 157.955]],
    [[-6.22288, -41.9496, -109.881, 466.068], [-5.69447, -29.7152, -33.784, 273.002], [-5.41738, -24.2882, -8.584, 169.891]],
    [[-6.43551, -46.1151, -120.814, 566.823], [-5.90887, -33.0251, -37.208, 346.189], [-5.63255, -27.2042, -6.792, 177.666]],
    [[-6.63894, -50.4287, -128.997, 642.781], [-6.11404, -36.461, -36.246, 348.554], [-5.8385, -30.1995, -5.163, 210.338]],
    [[-6.83488, -54.7119, -139.8, 736.376], [-6.31127, -39.9676, -37.021, 406.051], [-6.0365, -33.2381, -6.606, 317.776]],
];

// MacKinnon (1994), "Approximate asymptotic distribution functions for
// unit-root and cointegration tests", Journal of Business and Economic
// Statistics 12, 167–176: for N = 1, …, 6, the switch point τ*, the bounds
// τ_min and τ_max, and the coefficients of Φ⁻¹(p) as a quadratic in τ
// (left tail) and a cubic (elsewhere). Transcribed from statsmodels' and
// arch's `mackinnonp` (the two agree), which store some coefficients
// multiplied by 10 or 100; the values here are the coefficients
// themselves. Stata's `dfuller` p-values reproduce them for N = 1.
const TAU_STAR_C: [f64; 6] = [-1.61, -2.62, -3.13, -3.47, -3.78, -3.93];
const TAU_MIN_C: [f64; 6] = [-18.83, -18.86, -23.48, -28.07, -25.96, -23.27];
const TAU_MAX_C: [f64; 6] = [2.74, 0.92, 0.55, 0.61, 0.79, 1.0];
const SMALL_P_C: [[f64; 3]; 6] = [
    [2.1659, 1.4412, 0.038269],
    [2.92, 1.5012, 0.039796],
    [3.4699, 1.4856, 0.03164],
    [3.9673, 1.4777, 0.026315],
    [4.5509, 1.5338, 0.029545],
    [5.1399, 1.6036, 0.034445],
];
const LARGE_P_C: [[f64; 4]; 6] = [
    [1.7339, 0.93202, -0.12745, -0.010368],
    [2.1945, 0.64695, -0.29198, -0.042377],
    [2.5893, 0.45168, -0.36529, -0.050074],
    [3.0387, 0.45452, -0.33666, -0.041921],
    [3.5049, 0.52098, -0.29158, -0.033468],
    [3.9489, 0.58933, -0.25359, -0.02721],
];
const TAU_STAR_CT: [f64; 6] = [-2.89, -3.19, -3.5, -3.65, -3.8, -4.36];
const TAU_MIN_CT: [f64; 6] = [-16.18, -21.15, -25.37, -26.63, -26.53, -26.18];
const TAU_MAX_CT: [f64; 6] = [0.7, 0.63, 0.71, 0.93, 1.19, 1.42];
const SMALL_P_CT: [[f64; 3]; 6] = [
    [3.2512, 1.6047, 0.049588],
    [3.6646, 1.5419, 0.036448],
    [4.0983, 1.5173, 0.029898],
    [4.5844, 1.5338, 0.028796],
    [5.0722, 1.5634, 0.029472],
    [5.53, 1.5914, 0.030392],
];
const LARGE_P_CT: [[f64; 4]; 6] = [
    [2.5261, 0.61654, -0.37956, -0.060285],
    [2.85, 0.5272, -0.36622, -0.051695],
    [3.221, 0.5255, -0.32685, -0.041501],
    [3.652, 0.59758, -0.27483, -0.032081],
    [4.0712, 0.66428, -0.23464, -0.02546],
    [4.4735, 0.71757, -0.20681, -0.021196],
];
