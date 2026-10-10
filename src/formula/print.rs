//! [`Expr`] → canonical formula text, the form a formula is stored and shown
//! in: upper-case function names, no spaces, `$` as anchored, numbers in
//! General format, the leading `=` included.

use super::ast::{Area, Coord, Expr, Ref, UnOp};
use super::value::general_format;

pub fn print(e: &Expr) -> String {
    let mut out = String::from("=");
    write_expr(e, &mut out);
    out
}

fn write_expr(e: &Expr, out: &mut String) {
    match e {
        Expr::Number(n) => out.push_str(&general_format(*n)),
        Expr::Text(s) => {
            out.push('"');
            out.push_str(&s.replace('"', "\"\""));
            out.push('"');
        }
        Expr::Bool(b) => out.push_str(if *b { "TRUE" } else { "FALSE" }),
        Expr::Error(e) => out.push_str(e.as_str()),
        Expr::Array(rows) => {
            out.push('{');
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    out.push(';');
                }
                for (j, item) in row.iter().enumerate() {
                    if j > 0 {
                        out.push(',');
                    }
                    write_expr(item, out);
                }
            }
            out.push('}');
        }
        Expr::Ref(r) => write_ref(r, out),
        Expr::Unary(op, e) => {
            out.push(match op {
                UnOp::Neg => '-',
                UnOp::Plus => '+',
            });
            write_expr(e, out);
        }
        Expr::Percent(e) => {
            write_expr(e, out);
            out.push('%');
        }
        Expr::Binary(op, a, b) => {
            write_expr(a, out);
            out.push_str(op.symbol());
            write_expr(b, out);
        }
        Expr::Call(name, args) => {
            out.push_str(name);
            out.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_expr(a, out);
            }
            out.push(')');
        }
        Expr::Missing => {}
        Expr::Paren(e) => {
            out.push('(');
            write_expr(e, out);
            out.push(')');
        }
        Expr::Name(n) => out.push_str(n),
    }
}

pub fn write_ref(r: &Ref, out: &mut String) {
    if let Some(sheet) = &r.sheet {
        write_sheet(sheet, out);
        out.push('!');
    }
    match r.area {
        Area::Cell { row, col } => write_cell(row, col, out),
        Area::Range { r0, c0, r1, c1 } => {
            write_cell(r0, c0, out);
            out.push(':');
            write_cell(r1, c1, out);
        }
        Area::Cols { c0, c1 } => {
            write_col(c0, out);
            out.push(':');
            write_col(c1, out);
        }
        Area::Rows { r0, r1 } => {
            write_row(r0, out);
            out.push(':');
            write_row(r1, out);
        }
    }
}

/// A sheet name, quoted when it isn't a plain identifier (spaces,
/// punctuation, a leading digit, or text that looks like a cell).
fn write_sheet(name: &str, out: &mut String) {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && super::parse::parse(&format!("={name}")).map_or(true, |e| !matches!(e, Expr::Ref(_)));
    if plain {
        out.push_str(name);
    } else {
        out.push('\'');
        out.push_str(&name.replace('\'', "''"));
        out.push('\'');
    }
}

fn write_cell(row: Coord, col: Coord, out: &mut String) {
    write_col(col, out);
    write_row(row, out);
}

fn write_col(col: Coord, out: &mut String) {
    if col.abs {
        out.push('$');
    }
    out.push_str(&crate::grid::view::col_name(col.index));
}

fn write_row(row: Coord, out: &mut String) {
    if row.abs {
        out.push('$');
    }
    out.push_str(&(row.index + 1).to_string());
}
