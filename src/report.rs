//! Render [`CrapEntry`] lists as either a human-readable table or JSON.

use crate::merge::CrapEntry;
use crate::score::{Severity, DEFAULT_THRESHOLD};
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

    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(vec![
        Cell::new("CRAP").add_attribute(Attribute::Bold),
        Cell::new("CC").add_attribute(Attribute::Bold),
        Cell::new("Cov %").add_attribute(Attribute::Bold),
        Cell::new("Function").add_attribute(Attribute::Bold),
        Cell::new("Location").add_attribute(Attribute::Bold),
    ]);

    let mut crappy_count = 0usize;
    for entry in entries {
        let severity = Severity::classify(entry.crap, threshold);
        if severity == Severity::Crappy {
            crappy_count += 1;
        }

        let crap_cell = match severity {
            Severity::Crappy => Cell::new(format!("{:.1}", entry.crap)).fg(Color::Red),
            Severity::Clean => Cell::new(format!("{:.1}", entry.crap)).fg(Color::Green),
        };

        let cov_str = match entry.coverage {
            Some(c) => format!("{c:.1}"),
            None => "—".to_string(),
        };

        table.add_row(vec![
            crap_cell,
            Cell::new(format!("{:.0}", entry.cyclomatic)),
            Cell::new(cov_str),
            Cell::new(&entry.function),
            Cell::new(format!("{}:{}", entry.file.display(), entry.line)),
        ]);
    }

    writeln!(out, "{table}")?;

    let total = entries.len();
    if crappy_count == 0 {
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
            crappy_count,
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

pub const DEFAULT_THRESHOLD_CONST: f64 = DEFAULT_THRESHOLD;

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
}
