//! Saving: a workbook back to file bytes.
//!
//! [`to_bytes`] keeps the file's format and changes only edited records:
//!
//! - A clean row (loaded, unedited — sorting doesn't count) is copied from the
//!   source verbatim, with its own original terminator, so quoting, spacing
//!   and mixed line endings survive.
//! - A dirty or new row is serialized with the dialect, padded to the sheet's
//!   width, quoting a field only when it contains the delimiter, the quote, CR
//!   or LF. Values are written exactly as stored — `007`, `1e5` and
//!   `12345678901234567890` are never reformatted.
//! - Trailing rows that are empty and not clean originals are dropped, so
//!   visiting or clearing cells past the data adds nothing to the file.
//! - Whether the file ended with a newline, the BOM and the encoding are kept.
//!
//! [`to_bytes_with_dialect`] rewrites every row with another dialect (save-as
//! `.tsv`/`.csv`).

use super::dialect::{self, Dialect, Escape};
use super::model::Workbook;
use super::sheet::{RowView, Terminator};

/// Sheet 0 in the workbook's own dialect, raw bytes kept for clean rows.
pub fn to_bytes(wb: &Workbook) -> Vec<u8> {
    write_sheet(wb, 0, wb.dialect(), true)
}

/// Sheet 0 with every row re-serialized in `dialect` (save-as).
pub fn to_bytes_with_dialect(wb: &Workbook, dialect: &Dialect) -> Vec<u8> {
    write_sheet(wb, 0, dialect, false)
}

/// A character typed into the sheet that its file's encoding can't store
/// (only Windows-1252 has such characters). Saving would replace it with an
/// HTML `&#NNNN;` reference — losing it, and in a `;`-delimited file
/// splitting the field — so a save refuses instead.
pub fn unencodable(wb: &Workbook, d: &Dialect) -> Option<char> {
    if d.encoding != dialect::Encoding::Windows1252 {
        return None;
    }
    let s = wb.sheet(0);
    let end = s.last_kept_row().map_or(0, |r| r + 1);
    let width = s.write_width();
    let mut found = None;
    let mut buf = [0u8; 4];
    wb.for_each_row(0, 0..end, |_, view| {
        // A clean row was decoded from Windows-1252, so it encodes back.
        if found.is_some() || view.meta().is_clean() {
            return;
        }
        found = (0..width).find_map(|col| {
            view.cell(col).chars().find(|ch| {
                !ch.is_ascii() && encoding_rs::WINDOWS_1252.encode(ch.encode_utf8(&mut buf)).2
            })
        });
    });
    found
}

fn needs_quotes(value: &str, d: &Dialect) -> bool {
    value
        .bytes()
        .any(|b| b == d.delimiter || b == d.quote || b == b'\r' || b == b'\n')
}

fn push_field(out: &mut String, value: &str, d: &Dialect) {
    if !needs_quotes(value, d) {
        out.push_str(value);
        return;
    }
    let quote = d.quote as char;
    out.push(quote);
    for ch in value.chars() {
        if ch == quote {
            out.push(match d.escape {
                Escape::Doubled => quote,
                Escape::Backslash => '\\',
            });
        } else if ch == '\\' && d.escape == Escape::Backslash {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push(quote);
}

fn push_record(out: &mut String, row: &RowView, width: usize, d: &Dialect) {
    for col in 0..width {
        if col > 0 {
            out.push(d.delimiter as char);
        }
        push_field(out, row.cell(col), d);
    }
}

fn write_sheet(wb: &Workbook, sheet: usize, d: &Dialect, keep_raw: bool) -> Vec<u8> {
    let s = wb.sheet(sheet);
    let source = s.source();
    let end = s.last_kept_row().map_or(0, |r| r + 1);
    let width = s.write_width();
    let line_ending = d.line_ending.or(Terminator::Lf);
    let mut out = String::with_capacity(source.len() + source.len() / 16 + 64);
    wb.for_each_row(sheet, 0..end, |row, view| {
        let meta = view.meta();
        let raw = if keep_raw && meta.is_clean() {
            meta.src_range()
        } else {
            None
        };
        match raw {
            Some(range) => out.push_str(&source[range]),
            None => push_record(&mut out, &view, width, d),
        }
        let own = if keep_raw {
            meta.term.or(line_ending)
        } else {
            line_ending
        };
        let term = if row + 1 < end || d.trailing_newline {
            own
        } else {
            Terminator::None
        };
        out.push_str(term.as_str());
    });
    dialect::encode(out, d.encoding, d.bom)
}

#[cfg(test)]
mod tests {

    use std::time::Instant;

    use super::super::dialect::Encoding;
    use super::super::parse::parse_now;
    use super::super::testgen::{GenOpts, Rng, gen_csv};
    use super::*;

    fn load_bytes(bytes: &[u8], ext: Option<&str>) -> Workbook {
        parse_now(bytes.to_vec(), ext).unwrap()
    }

    fn load(s: &str) -> Workbook {
        load_bytes(s.as_bytes(), None)
    }

    fn assert_round_trip(bytes: &[u8], what: &str) {
        let wb = load_bytes(bytes, None);
        for sheet in wb.sheets() {
            sheet.check_layout();
        }
        let out = to_bytes(&wb);
        assert!(
            out == bytes,
            "{what}: round trip differs\n in: {:?}\nout: {:?}",
            String::from_utf8_lossy(bytes),
            String::from_utf8_lossy(&out)
        );
    }

    #[test]
    fn grid_round_trip_generated_csv() {
        let mut rng = Rng::new(0xC5F_2024);
        for case in 0..600 {
            let opts = GenOpts {
                delimiter: b",\t;|"[case % 4],
                blank_lines: rng.chance(50),
                ragged: rng.chance(50),
                lone_cr: rng.chance(20),
                bom: rng.chance(15),
            };
            let input = gen_csv(&mut rng, &opts);
            assert_round_trip(input.as_bytes(), &format!("case {case}"));
        }
    }

    /// Arbitrary byte soup over CSV's special characters (unclosed quotes,
    /// stray CRs, quotes mid-field) still round-trips exactly.
    #[test]
    fn grid_round_trip_malformed_soup() {
        let alphabet = [
            "a", ",", "\"", "\n", "\r", ";", "\t", "é", " ", "\\", "\"\"", "日",
        ];
        let mut rng = Rng::new(42);
        for case in 0..400 {
            let s: String = (0..rng.below(60)).map(|_| *rng.pick(&alphabet)).collect();
            assert_round_trip(s.as_bytes(), &format!("soup {case}"));
        }
        assert_round_trip(b"", "empty file");
        assert_round_trip(b"\n\n\n", "only newlines");
        assert_round_trip(b"\xEF\xBB\xBF", "only a BOM");
        assert_round_trip(b"\"unclosed, quote\nto eof", "unclosed");
    }

    #[test]
    fn grid_edit_one_cell_changes_one_record() {
        let src =
            "id,name,note\r\n1,\"Smith, J\",  spaced  \r\n2,Lee,x\r\n3,\"Ray\",\"q\"\"uote\"\r\n";
        let mut wb = load(src);
        wb.set_cell(0, 2, 1, "Lee, \"Bo\"");
        let out = String::from_utf8(to_bytes(&wb)).unwrap();
        let a: Vec<&str> = src.split("\r\n").collect();
        let b: Vec<&str> = out.split("\r\n").collect();
        assert_eq!(a.len(), b.len());
        let diffs: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
        assert_eq!(diffs, [2]);
        assert_eq!(b[2], "2,\"Lee, \"\"Bo\"\"\",x");
    }

    #[test]
    fn grid_values_written_as_typed() {
        let mut wb = load("a,b,c\n1,2,3\n");
        wb.set_cell(0, 1, 0, "007");
        wb.set_cell(0, 1, 1, "1e5");
        wb.set_cell(0, 1, 2, "12345678901234567890");
        assert_eq!(to_bytes(&wb), b"a,b,c\n007,1e5,12345678901234567890\n");
    }

    #[test]
    fn grid_far_edit_then_clear_leaves_file_unchanged() {
        let src: String = (0..10).map(|i| format!("{i},row {i}\n")).collect();
        let mut wb = load(&src);
        wb.set_cell(0, 5000, 7, "far");
        assert!(to_bytes(&wb).len() > src.len());
        wb.set_cell(0, 5000, 7, "");
        assert_eq!(wb.used_rows(0), 5001, "storage stays; the writer trims");
        assert_eq!(to_bytes(&wb), src.as_bytes());
        // Same without a final newline: no newline appears.
        let src = "a,b\n1,2";
        let mut wb = load(src);
        wb.set_cell(0, 3, 0, "x");
        assert_eq!(to_bytes(&wb), b"a,b\n1,2\n,\nx,");
        wb.set_cell(0, 3, 0, "");
        assert_eq!(to_bytes(&wb), src.as_bytes());
    }

    #[test]
    fn grid_appending_and_clearing_rows() {
        // A row appended after a last line without a newline gets one between.
        let mut wb = load("a,b\n1,2");
        wb.set_cell(0, 2, 1, "z");
        assert_eq!(to_bytes(&wb), b"a,b\n1,2\n,z");
        // Emptying the last original row's cells shrinks the file.
        let mut wb = load("a,b\n1,2\n3,4\n");
        wb.set_cells(0, &[(2, 0, String::new()), (2, 1, String::new())]);
        assert_eq!(to_bytes(&wb), b"a,b\n1,2\n");
        // But original blank lines are kept, even at the end.
        assert_round_trip(b"a,b\n1,2\n\n\n", "trailing blank lines");
        // A wider edit pads only the edited row; clean rows are untouched.
        let mut wb = load("a,b\n1,2\n");
        wb.set_cell(0, 1, 3, "w");
        assert_eq!(to_bytes(&wb), b"a,b\n1,2,,w\n");
    }

    #[test]
    fn grid_dirty_rows_keep_trailing_empty_columns() {
        let mut wb = load("a,b,\n1,2,\n");
        wb.set_cell(0, 1, 0, "9");
        assert_eq!(to_bytes(&wb), b"a,b,\n9,2,\n");
    }

    #[test]
    fn grid_backslash_dialect_writes_backslash_escapes() {
        let src = "id,text\n1,\"say \\\"hi\\\"\"\n";
        let mut wb = load(src);
        assert_eq!(to_bytes(&wb), src.as_bytes());
        wb.set_cell(0, 1, 1, "a \"b\" c\\d, e");
        assert_eq!(to_bytes(&wb), b"id,text\n1,\"a \\\"b\\\" c\\\\d, e\"\n");
        // And it reads back to the same value.
        let back = load_bytes(&to_bytes(&wb), None);
        assert_eq!(back.cell(0, 1, 1), "a \"b\" c\\d, e");
    }

    #[test]
    fn grid_utf16_and_windows_1252_round_trip() {
        let text = "name,city\r\nZoë,Köln\r\n\"Smith, J\",日本\r\n";
        for (le, bom) in [(true, b"\xFF\xFE"), (false, b"\xFE\xFF")] {
            let mut bytes = bom.to_vec();
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if le {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            let mut wb = load_bytes(&bytes, None);
            assert_eq!(
                wb.dialect().encoding,
                if le {
                    Encoding::Utf16Le
                } else {
                    Encoding::Utf16Be
                }
            );
            assert_eq!(wb.cell(0, 2, 1), "日本");
            assert_eq!(to_bytes(&wb), bytes);
            // Edited, it is still UTF-16 with the BOM.
            wb.set_cell(0, 1, 1, "Wien");
            let out = to_bytes(&wb);
            assert!(out.starts_with(bom));
            let back = load_bytes(&out, None);
            assert_eq!(back.cell(0, 1, 1), "Wien");
            assert_eq!(back.cell(0, 2, 0), "Smith, J");
        }

        let latin: &[u8] = b"name;city\nZo\xEB;K\xF6ln\n\x80uro;\x81\x9D\n";
        let mut wb = load_bytes(latin, None);
        assert_eq!(wb.dialect().encoding, Encoding::Windows1252);
        assert_eq!(wb.cell(0, 1, 0), "Zoë");
        assert_eq!(wb.cell(0, 2, 0), "€uro");
        assert_eq!(to_bytes(&wb), latin);
        wb.set_cell(0, 1, 1, "Düsseldorf");
        assert_eq!(
            to_bytes(&wb),
            b"name;city\nZo\xEB;D\xFCsseldorf\n\x80uro;\x81\x9D\n"
        );

        assert_round_trip(b"\xEF\xBB\xBFa,b\n1,2\n", "UTF-8 BOM");
    }

    #[test]
    fn grid_save_as_other_dialect() {
        let wb = load("a,\"b\tc\",d\r\n1,2,3\r\n");
        let tsv = to_bytes_with_dialect(&wb, &wb.dialect().with_delimiter(b'\t'));
        assert_eq!(tsv, b"a\t\"b\tc\"\td\r\n1\t2\t3\r\n");
        let back = load_bytes(&tsv, Some("tsv"));
        assert_eq!(back.cell(0, 0, 1), "b\tc");
        let csv = to_bytes_with_dialect(&back, &back.dialect().with_delimiter(b','));
        assert_eq!(csv, b"a,b\tc,d\r\n1,2,3\r\n");
    }

    #[test]
    fn grid_edit_then_round_trip_generated() {
        // After random edits, every clean row is still byte-identical and the
        // output re-parses to the same cell values.
        let mut rng = Rng::new(7);
        for case in 0..200 {
            let opts = GenOpts {
                delimiter: b',',
                blank_lines: false,
                ragged: false,
                lone_cr: false,
                bom: false,
            };
            let input = gen_csv(&mut rng, &opts);
            let mut wb = load(&input);
            if wb.used_rows(0) == 0 {
                continue;
            }
            for _ in 0..3 {
                let r = rng.below(wb.used_rows(0) + 2);
                let c = rng.below(wb.used_cols(0) + 1);
                let v = super::super::testgen::gen_field(&mut rng, b',');
                wb.set_cell(0, r, c, &v);
            }
            if rng.chance(50) {
                let c = rng.below(wb.used_cols(0));
                wb.sort_by_column(0, c, rng.chance(50), false);
            }
            let out = to_bytes(&wb);
            let back = load_bytes(&out, None);
            let rows = wb.sheet(0).last_kept_row().map_or(0, |r| r + 1);
            for r in 0..rows {
                for c in 0..wb.used_cols(0) {
                    assert_eq!(
                        back.cell(0, r, c),
                        wb.cell(0, r, c),
                        "case {case} ({r},{c}): {input:?}"
                    );
                }
            }
        }
    }

    /// `cargo test grid_perf -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn grid_perf_1m_rows() {
        let mut rng = Rng::new(1);
        let statuses = ["active", "pending review", "closed", "archived permanently"];
        let mut s = String::from("id,name,email,status,amount,date,city,score,flag,comment\n");
        for i in 0..1_000_000 {
            let _ = std::fmt::Write::write_fmt(
                &mut s,
                format_args!(
                    "{i},User {},user{i}@example.com,{},{}.{:02},2024-{:02}-{:02},{},{},{},\"note, {}\"\n",
                    rng.below(100_000),
                    statuses[rng.below(4)],
                    rng.below(10_000),
                    rng.below(100),
                    1 + rng.below(12),
                    1 + rng.below(28),
                    ["Paris", "Berlin", "New York City", "Tokyo"][rng.below(4)],
                    rng.below(1000),
                    rng.chance(50),
                    rng.below(1_000_000),
                ),
            );
        }
        let bytes = s.into_bytes();
        let len = bytes.len();
        let t = Instant::now();
        let wb = parse_now(bytes.clone(), None).unwrap();
        let parse_time = t.elapsed();
        let t = Instant::now();
        let out = to_bytes(&wb);
        let write_time = t.elapsed();
        assert_eq!(out.len(), len);
        assert!(out == bytes);
        let t = Instant::now();
        let prefix = super::super::parse::parse_prefix(&bytes, None, 200);
        let prefix_time = t.elapsed();
        assert_eq!(prefix.used_rows(0), 200);
        let mut wb = wb;
        let t = Instant::now();
        wb.sort_by_column(0, 4, false, true);
        let sort_time = t.elapsed();
        let t = Instant::now();
        let sorted = to_bytes(&wb);
        let sorted_write = t.elapsed();
        assert_eq!(sorted.len(), len);
        let cells = wb.used_rows(0) * wb.used_cols(0);
        eprintln!(
            "grid perf: {:.1} MB, {} rows x {} cols; pool {} strings / {:.1} MB\n  \
             parse {:?} ({:.0} MB/s), write {:?}, prefix(200) {:?}, \
             sort by amount {:?}, write after sort {:?}\n  cell storage {:.1} MB",
            len as f64 / 1e6,
            wb.used_rows(0),
            wb.used_cols(0),
            wb.pool().len(),
            wb.pool().bytes() as f64 / 1e6,
            parse_time,
            len as f64 / 1e6 / parse_time.as_secs_f64(),
            write_time,
            prefix_time,
            sort_time,
            sorted_write,
            (cells * 8 + wb.used_rows(0) * std::mem::size_of::<super::super::sheet::RowMeta>())
                as f64
                / 1e6,
        );
    }
}
