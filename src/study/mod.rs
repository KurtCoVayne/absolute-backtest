//! The study API (data-bundle doc, sections 6 and 7): the metrics library
//! over a run's NAV, fills and book, and later the trial log, lineage and
//! hold-out accounting that make a backtest a counted trial.
//!
//! Everything here is pure over `RunResult`; the kernel never reads it.

pub mod metrics;

pub use metrics::{
    block_bootstrap_sharpe, capacity, deflated_sharpe, min_track_record_length, newey_west_t, pbo_cscv, periods_per_year, probabilistic_sharpe, return_metrics, trading_metrics, DeflatedSharpe, Pbo,
    ReturnMetrics, Returns, TradingMetrics,
};
