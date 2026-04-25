//! # cargo-crap
//!
//! Compute the **CRAP** (Change Risk Anti-Patterns) metric for Rust
//! projects. The score combines cyclomatic complexity and test coverage
//! into a single number that is high when code is both hard to understand
//! and poorly tested — i.e. where bugs love to hide.
//!
//! ```text
//! CRAP(m) = comp(m)² × (1 − cov(m)/100)³ + comp(m)
//! ```
//!
//! The library is split into four orthogonal layers so each can be tested
//! in isolation:
//!
//! - [`score`] — the formula itself and the threshold classifier.
//! - [`complexity`] — a wrapper over `rust-code-analysis` that produces
//!   `(file, function, span, CC)` tuples.
//! - [`coverage`] — an LCOV parser that produces `(file, line) → hits`.
//! - [`merge`] — the join between the two, including path normalization.
//! - [`report`] — human / JSON rendering.

pub mod complexity;
pub mod coverage;
pub mod merge;
pub mod report;
pub mod score;
