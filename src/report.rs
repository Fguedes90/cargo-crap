//! Render [`CrapEntry`] lists as either a human-readable table or JSON.

use crate::merge::CrapEntry;
use crate::score::Severity;
use anyhow::Result;
use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, Table};
use owo_colors::OwoColorize;
use std::io::Write;

/// Output format for the report.
#[derive(Debug, Clone, Copy)]
pub enum Format {
    Human,
    Json,
}

/// Render `entries` in the requested format to `out`.
///
/// For `Format::Human` we emit a table and a summary line. The summary uses
/// stderr-style coloring if the output is a TTY; `owo-colors` no-ops when
/// it's not.
pub fn render<W: Write>(
    entries: &[CrapEntry],
    threshold: f64,
    format: Format,
    out: &mut W,
) -> Result<()> {
    match format {
        Format::Json => render_json(entries, out),
        Format::Human => render_human(entries, threshold, out),
    }
}

fn render_json<W: Write>(entries: &[CrapEntry], out: &mut W) -> Result<()> {
    serde_json::to_writer_pretty(&mut *out, entries)?;
    out.write_all(b"\n")?;
    Ok(())
}

fn render_human<W: Write>(entries: &[CrapEntry], threshold: f64, out: &mut W) -> Result<()> {
    if entries.is_empty() {
        writeln!(out, "No functions found.")?;
        return Ok(());
    }
    let table = build_table(entries, threshold);
    writeln!(out, "{table}")?;
    write_summary(
        out,
        crappy_count(entries, threshold),
        entries.len(),
        threshold,
    )
}

/// Build the full comfy-table for a slice of entries.
fn build_table(entries: &[CrapEntry], threshold: f64) -> Table {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(vec![
        Cell::new("CRAP").add_attribute(Attribute::Bold),
        Cell::new("CC").add_attribute(Attribute::Bold),
        Cell::new("Cov %").add_attribute(Attribute::Bold),
        Cell::new("Function").add_attribute(Attribute::Bold),
        Cell::new("Location").add_attribute(Attribute::Bold),
    ]);
    for entry in entries {
        table.add_row(build_row(entry, threshold));
    }
    table
}

/// Build one table row for a single entry.
fn build_row(entry: &CrapEntry, threshold: f64) -> Vec<Cell> {
    let severity = Severity::classify(entry.crap, threshold);
    let crap_cell = match severity {
        Severity::Crappy => Cell::new(format!("{:.1}", entry.crap)).fg(Color::Red),
        Severity::Clean => Cell::new(format!("{:.1}", entry.crap)).fg(Color::Green),
    };
    let cov_str = match entry.coverage {
        Some(c) => format!("{c:.1}"),
        None => "—".to_string(),
    };
    vec![
        crap_cell,
        Cell::new(format!("{:.0}", entry.cyclomatic)),
        Cell::new(cov_str),
        Cell::new(&entry.function),
        Cell::new(format!("{}:{}", entry.file.display(), entry.line)),
    ]
}

/// Write the one-line summary (✓ or ✗) after the table.
fn write_summary<W: Write>(out: &mut W, crappy: usize, total: usize, threshold: f64) -> Result<()> {
    if crappy == 0 {
        writeln!(
            out,
            "{} {} function(s) analyzed; none exceed CRAP threshold {}.",
            "✓".green(),
            total,
            threshold
        )?;
    } else {
        writeln!(
            out,
            "{} {}/{} function(s) exceed CRAP threshold {}.",
            "✗".red(),
            crappy,
            total,
            threshold
        )?;
    }
    Ok(())
}

/// How many entries exceed the threshold — used by the CLI to decide the
/// process exit code.
pub fn crappy_count(entries: &[CrapEntry], threshold: f64) -> usize {
    entries
        .iter()
        .filter(|e| Severity::classify(e.crap, threshold) == Severity::Crappy)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample() -> Vec<CrapEntry> {
        vec![
            CrapEntry {
                file: PathBuf::from("a.rs"),
                function: "clean".into(),
                line: 1,
                cyclomatic: 1.0,
                coverage: Some(100.0),
                crap: 1.0,
            },
            CrapEntry {
                file: PathBuf::from("a.rs"),
                function: "crappy".into(),
                line: 10,
                cyclomatic: 10.0,
                coverage: Some(0.0),
                crap: 110.0,
            },
        ]
    }

    #[test]
    fn json_output_is_valid_json() {
        let mut buf = Vec::new();
        render(&sample(), 30.0, Format::Json, &mut buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(parsed.is_array());
    }

    #[test]
    fn crappy_count_respects_threshold() {
        assert_eq!(crappy_count(&sample(), 30.0), 1);
        assert_eq!(crappy_count(&sample(), 200.0), 0);
    }

    #[test]
    fn human_output_mentions_every_function() {
        let mut buf = Vec::new();
        render(&sample(), 30.0, Format::Human, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("clean"));
        assert!(s.contains("crappy"));
    }

    #[test]
    fn human_summary_shows_tick_when_all_clean() {
        // Kills: render_human's `crappy_count == 0` replaced with `!= 0`.
        let all_clean = vec![CrapEntry {
            file: PathBuf::from("a.rs"),
            function: "clean".into(),
            line: 1,
            cyclomatic: 1.0,
            coverage: Some(100.0),
            crap: 1.0,
        }];
        let mut buf = Vec::new();
        render(&all_clean, 30.0, Format::Human, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(
            s.contains('✓'),
            "summary must show ✓ when nothing is crappy"
        );
        assert!(
            !s.contains('✗'),
            "summary must not show ✗ when nothing is crappy"
        );
    }

    #[test]
    fn human_summary_shows_cross_with_correct_count() {
        // Kills: severity check `== Crappy` replaced with `== Clean` (count stays 0),
        //        and `crappy_count += 1` replaced with *= 1 (count stays 0).
        let mut buf = Vec::new();
        render(&sample(), 30.0, Format::Human, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(
            s.contains('✗'),
            "summary must show ✗ when a function is crappy"
        );
        assert!(s.contains("1/2"), "summary must report 1 out of 2 crappy");
        assert!(
            !s.contains('✓'),
            "summary must not show ✓ when something is crappy"
        );
    }

    #[test]
    fn empty_entries_prints_no_functions_found() {
        let mut buf = Vec::new();
        render(&[], 30.0, Format::Human, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("No functions found."));
    }

    #[test]
    fn missing_coverage_shows_dash_in_table() {
        // Pins: match entry.coverage { None => "—" } in build_row.
        let entries = vec![CrapEntry {
            file: PathBuf::from("a.rs"),
            function: "foo".into(),
            line: 1,
            cyclomatic: 1.0,
            coverage: None,
            crap: 1.0,
        }];
        let mut buf = Vec::new();
        render(&entries, 30.0, Format::Human, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains('—'), "None coverage must render as —");
    }

    #[test]
    fn some_coverage_shows_formatted_number() {
        // Pins: match entry.coverage { Some(c) => format!("{c:.1}") } in build_row.
        let entries = vec![CrapEntry {
            file: PathBuf::from("a.rs"),
            function: "foo".into(),
            line: 1,
            cyclomatic: 1.0,
            coverage: Some(44.4),
            crap: 1.0,
        }];
        let mut buf = Vec::new();
        render(&entries, 30.0, Format::Human, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("44.4"), "Some(44.4) must render as 44.4");
    }

    #[test]
    fn human_summary_correct_for_all_crappy() {
        // Two entries both above threshold — count must be 2/2.
        let both_crappy = vec![
            CrapEntry {
                file: PathBuf::from("a.rs"),
                function: "bad".into(),
                line: 1,
                cyclomatic: 8.0,
                coverage: Some(0.0),
                crap: 72.0,
            },
            CrapEntry {
                file: PathBuf::from("a.rs"),
                function: "worse".into(),
                line: 10,
                cyclomatic: 10.0,
                coverage: Some(0.0),
                crap: 110.0,
            },
        ];
        let mut buf = Vec::new();
        render(&both_crappy, 30.0, Format::Human, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("2/2"), "both functions crappy, must report 2/2");
    }
}
