//! CSV export of report rows, written by Rust (DuckDB has no file access).
//!
//! Cells come from feeds and AI output, so text starting with a formula
//! trigger (`=`, `+`, `-`, `@`, tab, CR) is prefixed with `'` to stop
//! spreadsheet formula injection. Numbers are written as-is.

/// One CSV cell.
pub enum Cell {
    Text(String),
    Int(i64),
    /// Missing or unknown value (written empty, never as 0).
    Empty,
}

impl From<&str> for Cell {
    fn from(s: &str) -> Self {
        Cell::Text(s.to_string())
    }
}
impl From<String> for Cell {
    fn from(s: String) -> Self {
        Cell::Text(s)
    }
}
impl From<i64> for Cell {
    fn from(v: i64) -> Self {
        Cell::Int(v)
    }
}
impl From<Option<i64>> for Cell {
    fn from(v: Option<i64>) -> Self {
        v.map_or(Cell::Empty, Cell::Int)
    }
}

fn text_cell(s: &str) -> String {
    let guarded = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{s}") } else { s.to_string() };
    if guarded.contains([',', '"', '\n', '\r']) || guarded != guarded.trim() {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

/// RFC 4180 CSV with CRLF line endings and a UTF-8 BOM so spreadsheet
/// programs read ä/ö/å correctly.
pub fn to_csv(headers: &[&str], rows: impl IntoIterator<Item = Vec<Cell>>) -> String {
    let mut out = String::from("\u{feff}");
    out.push_str(&headers.iter().map(|h| text_cell(h)).collect::<Vec<_>>().join(","));
    out.push_str("\r\n");
    for row in rows {
        let cells: Vec<String> = row
            .into_iter()
            .map(|c| match c {
                Cell::Text(s) => text_cell(&s),
                Cell::Int(v) => v.to_string(),
                Cell::Empty => String::new(),
            })
            .collect();
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_escapes_and_guards_formulas() {
        let csv = to_csv(
            &["source", "n"],
            vec![
                vec![Cell::from("Yle, uutiset"), Cell::from(3)],
                vec![Cell::from("=HYPERLINK(\"http://evil\")"), Cell::from(None)],
                vec![Cell::from("Sää \"tänään\""), Cell::from(-5)],
            ],
        );
        let lines: Vec<&str> = csv.trim_start_matches('\u{feff}').split("\r\n").collect();
        assert_eq!(lines[0], "source,n");
        assert_eq!(lines[1], "\"Yle, uutiset\",3");
        assert_eq!(lines[2], "\"'=HYPERLINK(\"\"http://evil\"\")\",");
        assert_eq!(lines[3], "\"Sää \"\"tänään\"\"\",-5", "negative numbers stay numbers");
    }
}
