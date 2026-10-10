//! Grid cells as other table formats (org, Markdown, JSON, CSV) and back
//! (`org-table-to-grid`). Pure functions over a block of cell text.

use super::view::col_name;

/// Whether most non-empty values in column `c` (below the header) are
/// numbers — those align right, as org and spreadsheets do.
fn numeric_column(rows: &[Vec<String>], c: usize, header: bool) -> bool {
    let vals: Vec<&str> = rows
        .iter()
        .skip(usize::from(header))
        .filter_map(|r| r.get(c).map(String::as_str))
        .filter(|v| !v.trim().is_empty())
        .collect();
    let nums = vals
        .iter()
        .filter(|v| v.trim().parse::<f64>().is_ok())
        .count();
    !vals.is_empty() && nums * 2 > vals.len()
}

fn width(rows: &[Vec<String>]) -> usize {
    rows.iter().map(Vec::len).max().unwrap_or(0)
}

/// Lay out escaped cells as a pipe table with padded columns. `rule` builds
/// the separator line from the column widths and right-alignment.
fn pipe_table(
    cells: Vec<Vec<String>>,
    numeric: &[bool],
    header: bool,
    min_width: usize,
    rule: impl Fn(&[usize], &[bool]) -> String,
) -> String {
    let cols = numeric.len();
    let mut widths = vec![min_width; cols];
    for row in &cells {
        for (c, v) in row.iter().enumerate() {
            widths[c] = widths[c].max(v.chars().count());
        }
    }
    let line = |row: &[String]| {
        let parts: Vec<String> = (0..cols)
            .map(|c| {
                let v = row.get(c).map_or("", String::as_str);
                let w = widths[c];
                if numeric[c] {
                    format!("{v:>w$}")
                } else {
                    format!("{v:<w$}")
                }
            })
            .collect();
        format!("| {} |\n", parts.join(" | "))
    };
    let mut out = String::new();
    for (i, row) in cells.iter().enumerate() {
        out.push_str(&line(row));
        if i == 0 && header {
            out.push_str(&rule(&widths, numeric));
        }
    }
    out
}

/// An org table; a header row gets the `|---+---|` rule under it.
pub fn to_org(rows: &[Vec<String>], header: bool) -> String {
    let cols = width(rows);
    let numeric: Vec<bool> = (0..cols).map(|c| numeric_column(rows, c, header)).collect();
    let cells = rows
        .iter()
        .map(|r| {
            r.iter()
                .map(|v| v.replace('|', "\\vert{}").replace(['\r', '\n'], " "))
                .collect()
        })
        .collect();
    pipe_table(cells, &numeric, header, 1, |widths, _| {
        let dashes: Vec<String> = widths.iter().map(|w| "-".repeat(w + 2)).collect();
        format!("|{}|\n", dashes.join("+"))
    })
}

/// A GitHub-flavored Markdown table. Markdown needs a header row: without
/// one, the column letters (`A`, `B`, …) head it.
pub fn to_markdown(rows: &[Vec<String>], header: bool, first_col: usize) -> String {
    let cols = width(rows);
    let numeric: Vec<bool> = (0..cols).map(|c| numeric_column(rows, c, header)).collect();
    let mut cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            r.iter()
                .map(|v| {
                    v.replace('|', "\\|")
                        .replace("\r\n", "<br>")
                        .replace(['\n', '\r'], "<br>")
                })
                .collect()
        })
        .collect();
    if !header {
        cells.insert(0, (0..cols).map(|c| col_name(first_col + c)).collect());
    }
    // Columns at least 3 wide, so the `---` rule lines up with them.
    pipe_table(cells, &numeric, true, 3, |widths, numeric| {
        let parts: Vec<String> = widths
            .iter()
            .zip(numeric)
            .map(|(&w, &num)| {
                if num {
                    format!("{}:", "-".repeat(w - 1))
                } else {
                    "-".repeat(w)
                }
            })
            .collect();
        format!("| {} |\n", parts.join(" | "))
    })
}

/// JSON: with a header, an array of objects keyed by it (an empty or
/// repeated name falls back to the column letter); without, an array of
/// arrays. Values stay strings, exactly as stored.
pub fn to_json(rows: &[Vec<String>], header: bool, first_col: usize) -> String {
    use serde_json::{Map, Value};
    let value = if header && !rows.is_empty() {
        // Each column's key: its header name, or (blank or repeated) its
        // column letter — suffixed until it's unique, so no column's value
        // is overwritten by another's.
        let names: Vec<&str> = rows[0].iter().map(|k| k.trim()).collect();
        let mut keys: Vec<String> = Vec::new();
        for (c, &name) in names.iter().enumerate() {
            let base = if name.is_empty() || names[..c].contains(&name) {
                col_name(first_col + c)
            } else {
                name.to_string()
            };
            let mut key = base.clone();
            let mut n = 2;
            while keys.contains(&key) || (key != name && names.contains(&key.as_str())) {
                key = format!("{base}_{n}");
                n += 1;
            }
            keys.push(key);
        }
        Value::Array(
            rows[1..]
                .iter()
                .map(|r| {
                    let obj: Map<String, Value> = keys
                        .iter()
                        .enumerate()
                        .map(|(c, k)| {
                            (
                                k.clone(),
                                Value::String(r.get(c).cloned().unwrap_or_default()),
                            )
                        })
                        .collect();
                    Value::Object(obj)
                })
                .collect(),
        )
    } else {
        Value::Array(
            rows.iter()
                .map(|r| Value::Array(r.iter().cloned().map(Value::String).collect()))
                .collect(),
        )
    };
    let mut out = serde_json::to_string_pretty(&value).unwrap_or_default();
    out.push('\n');
    out
}

/// RFC 4180 CSV: fields holding a comma, quote, or line break are quoted.
pub fn to_csv(rows: &[Vec<String>]) -> String {
    to_delimited(rows, ',')
}

/// Rows joined by `delim`, one per line; a field holding the delimiter, a
/// quote, or a line break is quoted, its quotes doubled (RFC 4180, and how
/// Excel copies cells as tab-separated text).
pub fn to_delimited(rows: &[Vec<String>], delim: char) -> String {
    let mut out = String::new();
    for row in rows {
        for (i, v) in row.iter().enumerate() {
            if i > 0 {
                out.push(delim);
            }
            if v.contains([delim, '"', '\n', '\r']) {
                out.push('"');
                out.push_str(&v.replace('"', "\"\""));
                out.push('"');
            } else {
                out.push_str(v);
            }
        }
        out.push('\n');
    }
    out
}

/// A separator line: org's `|---+---|` (no spaces), or Markdown's
/// `| --- | :-: |` (each cell two dashes or more, or aligned with a colon,
/// so a data row of `-` placeholders isn't one).
fn is_rule(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('-') || !t.chars().all(|c| matches!(c, '|' | '-' | '+' | ':' | ' ')) {
        return false;
    }
    if !t.contains(' ') {
        return true;
    }
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split(['|', '+']).all(|cell| {
        let cell = cell.trim();
        cell.contains(':') || cell.matches('-').count() >= 2
    })
}

/// Split one `| a | b |` line into trimmed cells; `\|` and org's `\vert{}`
/// are a literal `|`.
fn table_cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    let mut cells = vec![String::new()];
    let mut chars = t.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars.peek() == Some(&'|') => {
                chars.next();
                cells.last_mut().expect("a cell").push('|');
            }
            '|' => cells.push(String::new()),
            c => cells.last_mut().expect("a cell").push(c),
        }
    }
    cells
        .into_iter()
        .map(|c| c.trim().replace("\\vert{}", "|").replace("<br>", "\n"))
        .collect()
}

/// The org/Markdown table made of `lines` (each starting with `|`): its rows
/// and whether a rule follows the first row (a header). `None` when no line
/// holds cells.
pub fn parse_table(lines: &[&str]) -> Option<(Vec<Vec<String>>, bool)> {
    let mut rows = Vec::new();
    let mut header = false;
    for line in lines {
        if is_rule(line) {
            header |= rows.len() == 1;
            continue;
        }
        rows.push(table_cells(line));
    }
    (!rows.is_empty()).then_some((rows, header))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    #[test]
    fn grid_export_org_markdown_json_csv() {
        let rows = block(&[&["name", "qty"], &["apple", "3"], &["a|b", "12"]]);
        assert_eq!(
            to_org(&rows, true),
            "| name      | qty |\n|-----------+-----|\n| apple     |   3 |\n| a\\vert{}b |  12 |\n"
        );
        assert_eq!(
            to_markdown(&rows, true, 0),
            "| name  | qty |\n| ----- | --: |\n| apple |   3 |\n| a\\|b  |  12 |\n"
        );
        let md = to_markdown(&block(&[&["x"]]), false, 2);
        assert!(md.starts_with("| C   |\n| --- |\n"), "{md}");
        let json = to_json(&rows, true, 0);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v[1]["name"], "a|b");
        assert_eq!(v[0]["qty"], "3");
        let v: serde_json::Value = serde_json::from_str(&to_json(&rows, false, 0)).unwrap();
        assert_eq!(v[0][1], "qty");
        assert_eq!(
            to_csv(&block(&[&["a,b", "say \"hi\""]])),
            "\"a,b\",\"say \"\"hi\"\"\"\n"
        );
    }

    #[test]
    fn grid_parse_org_and_markdown_tables() {
        let org = ["| name | qty |", "|------+-----|", "| a\\vert{}b | 3 |"];
        let (rows, header) = parse_table(&org).unwrap();
        assert!(header);
        assert_eq!(rows, block(&[&["name", "qty"], &["a|b", "3"]]));
        let md = ["| x | y |", "| --- | :-: |", "| 1 \\| 2 | |"];
        let (rows, header) = parse_table(&md).unwrap();
        assert!(header);
        assert_eq!(rows, block(&[&["x", "y"], &["1 | 2", ""]]));
        let (_, header) = parse_table(&["| 1 | 2 |", "| 3 | 4 |"]).unwrap();
        assert!(!header);
        // Round trip through the org writer.
        let src = block(&[&["k", "v"], &["a", "1"]]);
        let org = to_org(&src, true);
        let lines: Vec<&str> = org.lines().collect();
        assert_eq!(parse_table(&lines).unwrap(), (src, true));
    }
}
