//! The dependence treatments a test declares (Q-4.2, E-21): returns are
//! serially correlated and cross-sectionally correlated within a bar, so a
//! test states what its p-value assumes, and the result records it.

/// How a test's p-value treats dependence (Q-4.2, E-21).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Dependence {
    /// The textbook statistic: where the test's own theory covers the
    /// dependence, or the input is a cross-section at one bar.
    Iid,
    /// The statistic is robust to heteroskedasticity, its null distribution
    /// asymptotic (Lo and MacKinlay's z).
    IidRobust,
    /// An asymptotic null distribution that already models the series'
    /// dependence (unit-root and cointegration tests, Ljung–Box).
    Asymptotic,
    /// HAC standard errors with the Bartlett kernel; lags by Newey–West's
    /// 4(n/100)^(2/9) when unset.
    NeweyWest { lags: Option<usize> },
    /// The stationary bootstrap (Politis and Romano 1994); the mean block by
    /// Politis and White (2004) when unset; 2,000 replications by default.
    BlockBootstrap { block: Option<f64>, reps: usize },
    /// Standard errors clustered by entity or time (Petersen 2009).
    Clustered { by: ClusterBy },
    /// A permutation or exact test where exchangeability holds.
    Exact,
    /// A description without a p-value (the effective number of rules).
    Descriptive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterBy {
    Entity,
    Time,
}
