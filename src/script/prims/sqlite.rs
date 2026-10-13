//! Embedded SQLite (bundled, with FTS5).

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, _ctx: &EngineShared) {
    reg.section(Section::Sqlite);
    // --- SQLite (generic embedded database; bundled, with FTS5) -----------
    // A real database for Scheme packages — a full-text search index, a
    // relational store, whatever. Blocking, engine-thread (like the socket
    // family). Bundled SQLite ships FTS5, so `CREATE VIRTUAL TABLE … USING
    // fts5(…)` + `… MATCH ?` give BM25-ranked full-text search with no extra
    // dependency. Params are bound (never string-spliced), so untrusted text
    // is injection-safe.
    //
    //   (sqlite-open path)          → db id, 0 on failure (":memory:" ok)
    //   (sqlite-exec db sql params) → #t; runs one or more statements. With an
    //     EMPTY params list, `sql` may contain several `;`-separated statements
    //     (schema setup); with params, it is ONE statement and `params` (a flat
    //     string list) binds ?1, ?2, … in order.
    //   (sqlite-query db sql params)→ rows of a SELECT: a list of strings, one
    //     per row, columns joined by US (0x1f); NULLs become "". Bind like exec.
    //   (sqlite-close db)           → #t if the id existed
    //   (sqlite-last-error)         → the most recent failure text
    reg.register_fn("sqlite-open", |path: String| -> usize {
        let conn =
            rusqlite::Connection::open(&path).map_err(|e| format!("sqlite-open {path}: {e}"));
        or_report(conn.map(|c| SQLITE.insert(c)), 0)
    });
    reg.register_fn(
        "sqlite-exec",
        |id: usize, sql: String, StringList(ps): StringList| -> bool {
            let conns = SQLITE.lock();
            let Some(conn) = conns.get(&id) else {
                set_last_error(format!("sqlite-exec: no db {id}"));
                return false;
            };
            let result = if ps.is_empty() {
                // No binds → allow a multi-statement batch (schema setup).
                conn.execute_batch(&sql)
            } else {
                conn.execute(&sql, rusqlite::params_from_iter(ps.iter()))
                    .map(|_| ())
            };
            or_report(
                result
                    .map(|()| true)
                    .map_err(|e| format!("sqlite-exec: {e}")),
                false,
            )
        },
    );
    reg.register_fn(
        "sqlite-query",
        |id: usize, sql: String, StringList(ps): StringList| -> Vec<String> {
            let conns = SQLITE.lock();
            let Some(conn) = conns.get(&id) else {
                set_last_error(format!("sqlite-query: no db {id}"));
                return Vec::new();
            };
            let mut stmt = match conn.prepare(&sql) {
                Ok(s) => s,
                Err(e) => {
                    set_last_error(format!("sqlite-query: {e}"));
                    return Vec::new();
                }
            };
            let ncol = stmt.column_count();
            let rows = stmt.query_map(rusqlite::params_from_iter(ps.iter()), |row| {
                let mut cells: Vec<String> = Vec::with_capacity(ncol);
                for i in 0..ncol {
                    // Coerce any column type to text; NULL → "".
                    let v: rusqlite::types::Value = row.get(i)?;
                    cells.push(match v {
                        rusqlite::types::Value::Null => String::new(),
                        rusqlite::types::Value::Integer(n) => n.to_string(),
                        rusqlite::types::Value::Real(f) => f.to_string(),
                        rusqlite::types::Value::Text(s) => s,
                        rusqlite::types::Value::Blob(b) => String::from_utf8_lossy(&b).into_owned(),
                    });
                }
                Ok(cells.join("\u{1f}"))
            });
            let rows = rows
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .map_err(|e| format!("sqlite-query: {e}"));
            or_report(rows, Vec::new())
        },
    );
    reg.register_fn("sqlite-close", |id: usize| -> bool { SQLITE.remove(id) });
    reg.register_fn("sqlite-last-error", last_error);
}
