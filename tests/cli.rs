//! CLI integration tests — exercise the binary end-to-end.
//!
//! Each test drives `cargo-crap` with `assert_cmd` and verifies observable
//! output (stdout text, exit code). These are the tests that catch regressions
//! in `main`'s argument parsing, filter logic, and exit-code behaviour.

use assert_cmd::Command;
use predicates::prelude::*;

fn fixture_src() -> &'static str {
    "tests/fixtures/sample_project/src"
}

fn fixture_lcov() -> &'static str {
    "tests/fixtures/sample_project/lcov.info"
}

fn cmd() -> Command {
    Command::cargo_bin("cargo-crap").expect("binary must be built")
}

// --- Basic invocation ---

#[test]
fn help_flag_exits_successfully() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("CRAP"));
}

#[test]
fn version_flag_exits_successfully() {
    cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("cargo-crap"));
}

// --- Human output ---

#[test]
fn human_output_lists_all_fixture_functions() {
    cmd()
        .arg("--path")
        .arg(fixture_src())
        .arg("--lcov")
        .arg(fixture_lcov())
        .assert()
        .stdout(predicate::str::contains("trivial"))
        .stdout(predicate::str::contains("moderate"))
        .stdout(predicate::str::contains("crappy"));
}

#[test]
fn without_lcov_functions_are_scored_pessimistically() {
    // No --lcov → every function is treated as 0% covered (pessimistic default).
    // crappy (CC=12, 0%) should score very high and appear in output.
    cmd()
        .arg("--path")
        .arg(fixture_src())
        .assert()
        .stdout(predicate::str::contains("crappy"));
}

// --- JSON output ---

#[test]
fn json_output_is_valid_json_array() {
    let output = cmd()
        .arg("--path")
        .arg(fixture_src())
        .arg("--lcov")
        .arg(fixture_lcov())
        .arg("--format")
        .arg("json")
        .output()
        .expect("run");

    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be valid JSON");
    assert!(parsed.is_array(), "JSON output must be an array");
}

#[test]
fn json_entries_have_required_fields() {
    let output = cmd()
        .arg("--path")
        .arg(fixture_src())
        .arg("--lcov")
        .arg(fixture_lcov())
        .arg("--format")
        .arg("json")
        .output()
        .expect("run");

    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let entries: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let first = &entries[0];
    assert!(
        first.get("function").is_some(),
        "entry must have 'function'"
    );
    assert!(first.get("crap").is_some(), "entry must have 'crap'");
    assert!(
        first.get("cyclomatic").is_some(),
        "entry must have 'cyclomatic'"
    );
}

// --- --fail-above ---

#[test]
fn fail_above_exits_one_when_threshold_exceeded() {
    // With default threshold 30, crappy (CC=12, 0% cov → CRAP=156) fails.
    cmd()
        .arg("--path")
        .arg(fixture_src())
        .arg("--lcov")
        .arg(fixture_lcov())
        .arg("--fail-above")
        .assert()
        .failure(); // exit code 1
}

#[test]
fn fail_above_exits_zero_when_nothing_exceeds_high_threshold() {
    // With a very high threshold, nothing is crappy.
    cmd()
        .arg("--path")
        .arg(fixture_src())
        .arg("--lcov")
        .arg(fixture_lcov())
        .arg("--fail-above")
        .arg("--threshold")
        .arg("9999")
        .assert()
        .success();
}

// --- --top ---

#[test]
fn top_limits_output_rows() {
    // --top 1 must show only the worst function.
    let output = cmd()
        .arg("--path")
        .arg(fixture_src())
        .arg("--lcov")
        .arg(fixture_lcov())
        .arg("--format")
        .arg("json")
        .arg("--top")
        .arg("1")
        .output()
        .expect("run");

    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let entries: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(
        entries.as_array().map(|a| a.len()),
        Some(1),
        "--top 1 must return exactly 1 entry"
    );
}

// --- --missing ---

#[test]
fn missing_optimistic_does_not_flag_uncovered_functions() {
    // With --missing optimistic, functions with no coverage data are treated
    // as 100% covered. trivial (CC=1) → CRAP=1.0, well below threshold.
    cmd()
        .arg("--path")
        .arg(fixture_src())
        .arg("--missing")
        .arg("optimistic")
        .arg("--fail-above")
        .arg("--threshold")
        .arg("30")
        .assert()
        .success();
}

#[test]
fn missing_skip_drops_uncovered_functions_from_output() {
    // With --missing skip, functions with no coverage data are omitted.
    // Running against fixture src without an lcov → all functions are "missing".
    let output = cmd()
        .arg("--path")
        .arg(fixture_src())
        .arg("--missing")
        .arg("skip")
        .arg("--format")
        .arg("json")
        .output()
        .expect("run");

    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let entries: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(
        entries.as_array().map(|a| a.len()),
        Some(0),
        "--missing skip with no lcov must produce empty output"
    );
}

// --- --min ---

#[test]
fn min_filter_keeps_only_entries_at_or_above_cutoff() {
    // Kills: entries.retain(|e| e.crap >= min) replaced with < min (reversed filter).
    // trivial() with CC=1, 100% cov → CRAP=1.0. --min 5 must exclude it.
    let output = cmd()
        .arg("--path")
        .arg(fixture_src())
        .arg("--lcov")
        .arg(fixture_lcov())
        .arg("--min")
        .arg("5")
        .arg("--format")
        .arg("json")
        .output()
        .expect("run");

    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let entries: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    for entry in entries.as_array().expect("array") {
        let crap = entry["crap"].as_f64().expect("crap is a number");
        assert!(
            crap >= 5.0,
            "entry '{}' with crap={crap} must not appear with --min 5",
            entry["function"]
        );
    }
    assert!(
        !stdout.contains("\"trivial\""),
        "trivial (CRAP≈1.0) must be excluded by --min 5"
    );
}

// --- --path validation ---

#[test]
fn nonexistent_path_exits_with_error() {
    cmd()
        .arg("--path")
        .arg("/this/path/does/not/exist")
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not exist"));
}

// --- cargo subcommand invocation ---

#[test]
fn cargo_subcommand_form_strips_crap_argument() {
    // When cargo invokes us as `cargo-crap crap [args...]`, the extra "crap"
    // token must be stripped. This simulates that invocation.
    Command::cargo_bin("cargo-crap")
        .expect("binary must be built")
        .args(["crap", "--path", fixture_src(), "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"^\[").expect("json starts with ["));
}
