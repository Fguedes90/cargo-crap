//! Optional persistent configuration via `.cargo-crap.toml`.
//!
//! The file is searched for by walking up from the current working directory.
//! CLI flags always take precedence over values in the config file — the
//! config only fills in values the user did not explicitly provide.
//!
//! ## Example `.cargo-crap.toml`
//!
//! ```toml
//! threshold = 30.0
//! fail-above = true
//! missing = "pessimistic"
//! # Appends to the default exclusions (tests/**, benches/**, examples/**).
//! exclude = ["src/generated/**"]
//! # Replaces the default-exclude list. `[]` disables it entirely.
//! default-excludes = ["benches/**", "examples/**"]
//! # `allow` accepts both function-name globs and path globs (any entry
//! # containing `/` or `**` is treated as a path glob).
//! allow = ["generated::*", "src/generated/**"]
//! # Final entry ordering: "crap" (default) or "file" (stable for baselines).
//! sort = "file"
//! # Show Unchanged rows in --baseline mode (human / markdown).
//! show_unchanged = true
//! # Append an Uncovered column (uncovered line ranges) to the human,
//! # markdown, and pr-comment outputs.
//! uncovered-hints = true
//! # Metric contract: "classic" (default) or "strict".
//! profile = "strict"
//! # Per-rule overrides on top of the profile's defaults.
//! abort-weight = 2.0
//! max-abort-ok = 12
//! ```

use crate::complexity::{CountOptions, Profile};
use crate::merge::{MissingCoveragePolicy, SortOrder};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

/// Persistent settings loaded from `.cargo-crap.toml`.
///
/// All fields are optional — only the keys present in the config file override
/// the built-in defaults. CLI flags take precedence over every field here.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// CRAP score above which a function is considered "crappy".
    pub threshold: Option<f64>,

    /// Exit non-zero if any function's CRAP score exceeds `threshold`.
    pub fail_above: Option<bool>,

    /// How to handle functions with no coverage data.
    /// One of `"pessimistic"` (default), `"optimistic"`, or `"skip"`.
    pub missing: Option<MissingCoveragePolicy>,

    /// Glob patterns for source files to skip (relative to `--path`).
    #[serde(default)]
    pub exclude: Vec<String>,

    /// Replaces the built-in default-exclude list (`tests/**`, `benches/**`,
    /// `examples/**`) wholesale. `[]` disables default exclusions; a subset
    /// re-includes some directories; a superset extends the defaults.
    /// Accepted as `default-excludes` (house style) or `default_excludes`.
    /// Unlike `exclude`, which appends, this key replaces.
    #[serde(alias = "default_excludes")]
    pub default_excludes: Option<Vec<String>>,

    /// Only show the top N crappiest functions.
    pub top: Option<usize>,

    /// Only show functions with a CRAP score at or above this value.
    pub min: Option<f64>,

    /// Glob patterns for function names to suppress from the report.
    /// Supports `*` (matches any chars including `::`) and `?`.
    /// Example: `"Foo::*"` suppresses all methods on `Foo`.
    #[serde(default)]
    pub allow: Vec<String>,

    /// Exit non-zero if any function regressed since `--baseline`.
    pub fail_regression: Option<bool>,

    /// Maximum number of threads used by `analyze_tree` for parallel file
    /// analysis. `None` lets rayon size the pool to the host. Must be
    /// non-zero when set.
    pub jobs: Option<usize>,

    /// Tolerance for the regression detector. Score deltas with absolute
    /// value at or below this are reported as `Unchanged`. Must be
    /// non-negative when set.
    pub epsilon: Option<f64>,

    /// Final ordering of report entries. One of `"crap"` (default, CRAP score
    /// descending) or `"file"` (`(file, function, line)` ascending).
    pub sort: Option<SortOrder>,

    /// In `--baseline` mode, show `Unchanged` rows in the human and markdown
    /// tables. Defaults to false: only changed functions are listed.
    /// Accepted as `show-unchanged` (house style) or `show_unchanged`.
    #[serde(alias = "show_unchanged")]
    pub show_unchanged: Option<bool>,

    /// Append an `Uncovered` column (uncovered line ranges per function) to
    /// the human, markdown, and pr-comment outputs. Defaults to false.
    /// Config-only — there is deliberately no CLI flag. Accepted as
    /// `uncovered-hints` (house style) or `uncovered_hints`.
    #[serde(alias = "uncovered_hints")]
    pub uncovered_hints: Option<bool>,

    /// Metric contract for the run: `"classic"` (default — `McCabe`
    /// decision points only) or `"strict"` (also charges hidden aborts,
    /// closure branches and `let … else`, and counts a total `match`
    /// once). Config-only, like every knob below it: a score weight
    /// flipped per run would make two baselines incomparable.
    pub profile: Option<Profile>,

    /// Charge for a hidden abort — `.unwrap()`, `.expect(…)`, indexing,
    /// `/` or `%` by a non-literal divisor. Overrides the profile default
    /// (0.0 classic, 2.0 strict). Must be finite and non-negative.
    pub abort_weight: Option<f64>,

    /// Charge for a self-naming abort — the `panic!` / `assert!` macro
    /// family. Overrides the profile default (0.0 classic, 1.0 strict).
    /// Must be finite and non-negative.
    pub documented_abort_weight: Option<f64>,

    /// Charge for an `unsafe` block or `unsafe fn`. Overrides the profile
    /// default (0.0 classic, 2.0 strict). Must be finite and non-negative.
    pub unsafe_weight: Option<f64>,

    /// Count decision points inside closure bodies toward the enclosing
    /// function. Overrides the profile default (false classic, true strict).
    pub count_closures: Option<bool>,

    /// Count `let … else` as a decision point. Overrides the profile
    /// default (false classic, true strict).
    pub count_let_else: Option<bool>,

    /// Charge a `match` that is total by construction once instead of once
    /// per arm. Overrides the profile default (false classic, true strict).
    pub total_match_once: Option<bool>,

    /// Ratchet on `// crap-ok:` exonerations: the run fails when more
    /// markers are in effect than this. Absent means no enforcement — the
    /// count is still reported under `strict`, because an escape hatch
    /// nobody counts rots.
    pub max_abort_ok: Option<usize>,
}

impl Config {
    /// Resolve the effective counting options: the profile supplies every
    /// default, and each explicit key overrides its own field.
    ///
    /// Weights are validated here rather than at the use site because a
    /// negative or non-finite weight is a tool error (exit 2), not a score:
    /// it would silently invert the gate instead of tripping it.
    pub fn count_options(&self) -> Result<CountOptions> {
        let mut opts = CountOptions::for_profile(self.profile.unwrap_or_default());
        for (value, key) in [
            (self.abort_weight, "abort-weight"),
            (self.documented_abort_weight, "documented-abort-weight"),
            (self.unsafe_weight, "unsafe-weight"),
        ] {
            if let Some(w) = value
                && (!w.is_finite() || w < 0.0)
            {
                anyhow::bail!("{key} must be finite and non-negative, got {w}");
            }
        }
        if let Some(w) = self.abort_weight {
            opts.abort_weight = w;
        }
        if let Some(w) = self.documented_abort_weight {
            opts.documented_abort_weight = w;
        }
        if let Some(w) = self.unsafe_weight {
            opts.unsafe_weight = w;
        }
        if let Some(b) = self.count_closures {
            opts.count_closures = b;
        }
        if let Some(b) = self.count_let_else {
            opts.count_let_else = b;
        }
        if let Some(b) = self.total_match_once {
            opts.total_match_once = b;
        }
        Ok(opts)
    }

    /// Everything the run needs to know about the metric contract, resolved
    /// in one place so no caller can score under one profile and report
    /// another.
    pub fn metric_settings(&self) -> Result<MetricSettings> {
        Ok(MetricSettings {
            options: self.count_options()?,
            profile: self.profile.unwrap_or_default(),
            max_abort_ok: self.max_abort_ok,
        })
    }
}

/// The resolved metric contract: what to count, under which name, against
/// which exoneration ratchet.
#[derive(Debug, Clone, Copy)]
pub struct MetricSettings {
    pub options: CountOptions,
    pub profile: Profile,
    pub max_abort_ok: Option<usize>,
}

/// Walk up from `start` until `.cargo-crap.toml` is found.
///
/// Returns [`Config::default`] when no config file exists anywhere in the
/// directory hierarchy — this means the tool works without any config file.
pub fn load(start: &Path) -> Result<Config> {
    let mut dir = if start.is_file() {
        start.parent().unwrap_or(start)
    } else {
        start
    };

    loop {
        let candidate = dir.join(".cargo-crap.toml");
        if candidate.exists() {
            let raw = fs::read_to_string(&candidate)
                .with_context(|| format!("reading {}", candidate.display()))?;
            let cfg: Config =
                toml::from_str(&raw).with_context(|| format!("parsing {}", candidate.display()))?;
            return Ok(cfg);
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => return Ok(Config::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_config(
        dir: &Path,
        content: &str,
    ) {
        let mut f = fs::File::create(dir.join(".cargo-crap.toml")).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn missing_config_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load(dir.path()).unwrap();
        assert!(cfg.threshold.is_none());
        assert!(cfg.fail_above.is_none());
        assert!(cfg.missing.is_none());
        assert!(cfg.exclude.is_empty());
        assert!(cfg.allow.is_empty());
    }

    #[test]
    fn config_file_is_parsed() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            r#"
threshold = 20.0
fail-above = true
missing = "optimistic"
exclude = ["tests/**"]
allow = ["Foo::*"]
"#,
        );
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg.threshold, Some(20.0));
        assert_eq!(cfg.fail_above, Some(true));
        assert_eq!(cfg.missing, Some(MissingCoveragePolicy::Optimistic));
        assert_eq!(cfg.exclude, ["tests/**"]);
        assert_eq!(cfg.allow, ["Foo::*"]);
    }

    #[test]
    fn default_excludes_absent_means_none() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "threshold = 20.0\n");
        let cfg = load(dir.path()).unwrap();
        assert!(cfg.default_excludes.is_none());
    }

    #[test]
    fn default_excludes_kebab_case_is_parsed() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "default-excludes = [\"benches/**\", \"examples/**\"]\n",
        );
        let cfg = load(dir.path()).unwrap();
        assert_eq!(
            cfg.default_excludes.as_deref(),
            Some(&["benches/**".to_string(), "examples/**".to_string()][..])
        );
    }

    #[test]
    fn default_excludes_snake_case_alias_is_parsed() {
        // Spec 14 scenarios write the key as `default_excludes`; both
        // spellings must work despite `deny_unknown_fields`.
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "default_excludes = [\"tests/**\"]\n");
        let cfg = load(dir.path()).unwrap();
        assert_eq!(
            cfg.default_excludes.as_deref(),
            Some(&["tests/**".to_string()][..])
        );
    }

    #[test]
    fn default_excludes_empty_list_is_some_empty() {
        // `[]` must be distinguishable from "key absent": it disables the
        // built-in defaults rather than falling back to them.
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "default-excludes = []\n");
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg.default_excludes.as_deref(), Some(&[][..]));
    }

    #[test]
    fn config_is_found_by_walking_up() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "threshold = 15.0\n");
        let subdir = dir.path().join("src");
        fs::create_dir(&subdir).unwrap();
        // Start from a subdirectory — should walk up and find the config.
        let cfg = load(&subdir).unwrap();
        assert_eq!(cfg.threshold, Some(15.0));
    }

    #[test]
    fn sort_and_show_unchanged_are_parsed() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "sort = \"file\"\nshow_unchanged = true\n");
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg.sort, Some(SortOrder::File));
        assert_eq!(cfg.show_unchanged, Some(true));
    }

    #[test]
    fn sort_and_show_unchanged_absent_means_none() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "threshold = 20.0\n");
        let cfg = load(dir.path()).unwrap();
        assert!(cfg.sort.is_none());
        assert!(cfg.show_unchanged.is_none());
        assert!(cfg.uncovered_hints.is_none());
    }

    #[test]
    fn uncovered_hints_kebab_case_is_parsed() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "uncovered-hints = true\n");
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg.uncovered_hints, Some(true));
    }

    #[test]
    fn uncovered_hints_snake_case_alias_is_parsed() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "uncovered_hints = false\n");
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg.uncovered_hints, Some(false));
    }

    #[test]
    fn unknown_key_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "unknown-key = true\n");
        let err = load(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("parsing"),
            "expected parse error, got: {err}"
        );
    }

    /// Resolve `count_options` from a TOML body.
    fn options_from(body: &str) -> Result<CountOptions> {
        let cfg: Config = toml::from_str(body).expect("parse");
        cfg.count_options()
    }

    #[test]
    fn profile_defaults_are_overridden_key_by_key() {
        let opts = options_from("profile = \"strict\"\nabort-weight = 3.5\n").expect("resolve");
        assert!((opts.abort_weight - 3.5).abs() < f64::EPSILON);
        // Untouched keys keep the profile's value, not the classic one.
        assert!((opts.unsafe_weight - 2.0).abs() < f64::EPSILON);
        assert!(opts.count_closures);
    }

    #[test]
    fn a_zero_weight_disables_its_rule_and_is_not_an_error() {
        // Zero is the documented way to switch one rule off inside an
        // otherwise strict profile — rejecting it would make the profile
        // all-or-nothing.
        let opts = options_from("profile = \"strict\"\nunsafe-weight = 0.0\n").expect("resolve");
        assert!(opts.unsafe_weight.abs() < f64::EPSILON);
    }

    #[test]
    fn a_negative_or_non_finite_weight_is_rejected_by_name() {
        for body in [
            "abort-weight = -0.5\n",
            "documented-abort-weight = nan\n",
            "unsafe-weight = inf\n",
        ] {
            let err = options_from(body).expect_err(&format!("{body} must be rejected"));
            let key = body.split_whitespace().next().expect("key");
            assert!(
                err.to_string().contains(key),
                "the message must name the offending key, got: {err}"
            );
        }
    }

    #[test]
    fn classic_is_the_default_contract() {
        let opts = options_from("").expect("resolve");
        assert_eq!(opts, CountOptions::for_profile(Profile::Classic));
        let cfg: Config = toml::from_str("").expect("parse");
        assert_eq!(
            cfg.metric_settings().expect("resolve").profile,
            Profile::Classic
        );
    }
}
