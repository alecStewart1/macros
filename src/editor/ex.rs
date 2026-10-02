//! Vim ex command lines: line ranges (`%`, `.`, `$`, numbers, `'a`, `'<,'>`,
//! `/pat/`, offsets), `:s` with flags and confirmation, `&` / `:&&` / `g&`,
//! `:g` / `:v`, `:normal`, `:d` `:y` `:m` `:t` `:j` `:>` `:<` `:sort` `:put`,
//! and `:{n}`. Parsing and the line transformations are pure functions (unit
//! tested below); the `Editor` methods resolve addresses against the buffer
//! and apply each command as one edit of the smallest span it touches, so
//! marks and the jump list ride along.

use regex::Regex;

use super::input::Handled;
use super::*;

// --- parsing -------------------------------------------------------------------

/// What an address starts from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AddrBase {
    /// An absolute 1-based line number (`0` = before the first line, for
    /// `:m0` / `:t0`).
    Num(i64),
    Cur,
    Last,
    Mark(char),
    /// `/pat/` (forward) or `?pat?` (backward): the next matching line.
    Search(String, bool),
}

/// An address: a base plus `+n` / `-n` offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Addr {
    pub base: AddrBase,
    pub offset: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ExRange {
    /// `%` — every line.
    Whole,
    One(Addr),
    /// `a,b` — or `a;b`, where `b` is resolved with the cursor moved to `a`.
    Two(Addr, bool, Addr),
}

/// A parsed command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExLine {
    pub range: Option<ExRange>,
    pub name: String,
    pub bang: bool,
    pub arg: String,
}

/// Read a delimited pattern body up to the (unescaped) `delim`, returning it
/// and the rest after the delimiter (or the end when it is missing).
pub(super) fn take_delimited(s: &str, delim: char) -> (String, &str) {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            if let Some((_, n)) = chars.next() {
                if n != delim {
                    out.push('\\');
                }
                out.push(n);
            }
        } else if c == delim {
            return (out, &s[i + c.len_utf8()..]);
        } else {
            out.push(c);
        }
    }
    (out, "")
}

fn take_number(s: &str) -> (Option<i64>, &str) {
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        (None, s)
    } else {
        (s[..digits].parse().ok(), &s[digits..])
    }
}

/// Parse one address at the start of `s`.
pub(super) fn parse_addr(s: &str) -> Option<(Addr, &str)> {
    let mut rest = s;
    let base = if let (Some(n), r) = take_number(rest) {
        rest = r;
        Some(AddrBase::Num(n))
    } else if let Some(r) = rest.strip_prefix('.') {
        rest = r;
        Some(AddrBase::Cur)
    } else if let Some(r) = rest.strip_prefix('$') {
        rest = r;
        Some(AddrBase::Last)
    } else if let Some(r) = rest.strip_prefix('\'') {
        let c = r.chars().next()?;
        rest = &r[c.len_utf8()..];
        Some(AddrBase::Mark(c))
    } else if let Some(r) = rest.strip_prefix('/') {
        let (pat, r) = take_delimited(r, '/');
        rest = r;
        Some(AddrBase::Search(pat, true))
    } else if let Some(r) = rest.strip_prefix('?') {
        let (pat, r) = take_delimited(r, '?');
        rest = r;
        Some(AddrBase::Search(pat, false))
    } else {
        None
    };
    let mut offset = 0i64;
    let mut saw_offset = false;
    loop {
        let sign = if rest.starts_with('+') {
            1
        } else if rest.starts_with('-') {
            -1
        } else {
            break;
        };
        saw_offset = true;
        let (n, r) = take_number(&rest[1..]);
        offset += sign * n.unwrap_or(1);
        rest = r;
    }
    match base {
        Some(base) => Some((Addr { base, offset }, rest)),
        // A bare offset (`:+3`) is relative to the current line.
        None if saw_offset => Some((
            Addr {
                base: AddrBase::Cur,
                offset,
            },
            rest,
        )),
        None => None,
    }
}

/// Split a command line into range, command name, `!`, and argument.
pub(super) fn parse_ex_line(input: &str) -> ExLine {
    let mut s = input.trim_start().trim_start_matches(':').trim_start();
    let range = if let Some(r) = s.strip_prefix('%') {
        s = r;
        Some(ExRange::Whole)
    } else if let Some((a, r)) = parse_addr(s) {
        s = r;
        if let Some(sep) = s.chars().next().filter(|c| *c == ',' || *c == ';') {
            s = &s[1..];
            let b = match parse_addr(s) {
                Some((b, r)) => {
                    s = r;
                    b
                }
                // `1,` = `1,.`
                None => Addr {
                    base: AddrBase::Cur,
                    offset: 0,
                },
            };
            Some(ExRange::Two(a, sep == ';', b))
        } else {
            Some(ExRange::One(a))
        }
    } else if let Some(r) = s.strip_prefix(',') {
        // `,5` = `.,5`
        s = r;
        let cur = Addr {
            base: AddrBase::Cur,
            offset: 0,
        };
        match parse_addr(s) {
            Some((b, r)) => {
                s = r;
                Some(ExRange::Two(cur, false, b))
            }
            None => Some(ExRange::One(cur)),
        }
    } else {
        None
    };
    let s = s.trim_start();
    let first = s.chars().next();
    let (mut name, mut rest) = match first {
        Some(c) if c.is_ascii_alphabetic() => {
            let n = s.chars().take_while(|c| c.is_ascii_alphabetic()).count();
            (s[..n].to_string(), &s[n..])
        }
        // `:>>`, `:<`, `:&&`, `:=`, `:~`, `:!`.
        Some(c) if "<>&=~!#".contains(c) => {
            let n = s.chars().take_while(|x| *x == c).count();
            (s[..n].to_string(), &s[n..])
        }
        _ => (String::new(), s),
    };
    // `:ka` sets mark a (the mark letter is glued on).
    if name.len() == 2 && name.starts_with('k') && !matches!(name.as_str(), "ke") {
        rest = &s[1..];
        name = "k".into();
    }
    let bang =
        name.chars().all(|c| c.is_ascii_alphabetic()) && !name.is_empty() && rest.starts_with('!');
    if bang {
        rest = &rest[1..];
    }
    ExLine {
        range,
        name,
        bang,
        arg: rest.trim().to_string(),
    }
}

/// A `:s` command's parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SubSpec {
    pub pattern: String,
    pub replacement: String,
    pub flags: String,
    pub count: Option<usize>,
    /// `:s` with no pattern part at all (repeat the last substitute).
    pub bare: bool,
}

/// Parse `/pat/rep/flags count` (any non-alphanumeric delimiter).
pub(super) fn parse_sub(arg: &str) -> SubSpec {
    let arg = arg.trim_start();
    let Some(delim) = arg
        .chars()
        .next()
        .filter(|c| !c.is_alphanumeric() && !c.is_whitespace() && *c != '"' && *c != '|')
    else {
        // `:s`, `:s g`, `:s 3`, `:&&`: flags/count only.
        let (flags, count) = split_flags_count(arg);
        return SubSpec {
            pattern: String::new(),
            replacement: String::new(),
            flags,
            count,
            bare: true,
        };
    };
    let (pattern, rest) = take_delimited(&arg[delim.len_utf8()..], delim);
    let (replacement, rest) = take_delimited(rest, delim);
    let (flags, count) = split_flags_count(rest);
    SubSpec {
        pattern,
        replacement,
        flags,
        count,
        bare: false,
    }
}

fn split_flags_count(s: &str) -> (String, Option<usize>) {
    let s = s.trim();
    let flags: String = s
        .chars()
        .take_while(|c| "&cegiInp#lr".contains(*c))
        .collect();
    let rest = s[flags.len()..].trim();
    (flags, rest.parse().ok())
}

/// The Vim pattern dialect over Rust's regex syntax: `\<` / `\>` word
/// boundaries, a leading `\c` / `\C` case flag. Everything else is the regex
/// crate's syntax (as `/` search already is).
pub(super) fn vim_pattern(pat: &str, ignore_case: bool) -> String {
    let mut out = String::new();
    let mut icase = ignore_case;
    let mut chars = pat.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek().copied() {
                Some('<') | Some('>') => {
                    chars.next();
                    out.push_str("\\b");
                }
                Some('c') => {
                    chars.next();
                    icase = true;
                }
                Some('C') => {
                    chars.next();
                    icase = false;
                }
                Some(n) => {
                    chars.next();
                    out.push('\\');
                    out.push(n);
                }
                None => out.push_str("\\\\"),
            }
        } else {
            out.push(c);
        }
    }
    if icase { format!("(?i){out}") } else { out }
}

/// A Vim replacement string in the regex crate's expansion syntax: `\1`–`\9`
/// and `&` / `\0` (the whole match), `\&` a literal `&`, `\n` / `\r` a line
/// break, `\t` a tab, `$` escaped (it is literal in Vim).
pub(super) fn vim_replacement(rep: &str) -> String {
    let mut out = String::new();
    let mut chars = rep.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(d) if d.is_ascii_digit() => {
                    out.push_str("${");
                    out.push(d);
                    out.push('}');
                }
                Some('n') | Some('r') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('&') => out.push('&'),
                Some('$') => out.push_str("$$"),
                Some(o) => out.push(o),
                None => out.push('\\'),
            },
            '&' => out.push_str("${0}"),
            '$' => out.push_str("$$"),
            _ => out.push(c),
        }
    }
    out
}

/// Substitute in each line: the first match, or every match with `global`.
/// Returns the new lines and how many replacements were made.
pub(super) fn substitute_lines(
    lines: &[String],
    re: &Regex,
    replacement: &str,
    global: bool,
) -> (Vec<String>, usize) {
    let mut n = 0;
    let out = lines
        .iter()
        .map(|line| {
            let hits = re.find_iter(line).count();
            if hits == 0 {
                return line.clone();
            }
            if global {
                n += hits;
                re.replace_all(line, replacement).into_owned()
            } else {
                n += 1;
                re.replace(line, replacement).into_owned()
            }
        })
        .collect();
    (out, n)
}

/// `J` / `:j` joining of `lines` into one: the next line's leading blanks are
/// dropped and one space separates, except before `)`, after a trailing
/// blank, or for an empty line. `raw` (`:j!` / `gJ`) concatenates verbatim.
pub(super) fn join_lines(lines: &[String], raw: bool) -> String {
    let mut out = lines.first().cloned().unwrap_or_default();
    for next in &lines[1.min(lines.len())..] {
        if raw {
            out.push_str(next);
            continue;
        }
        let trimmed = next.trim_start();
        let sep = if trimmed.is_empty()
            || trimmed.starts_with(')')
            || out.ends_with(' ')
            || out.ends_with('\t')
            || out.is_empty()
        {
            ""
        } else {
            " "
        };
        out.push_str(sep);
        out.push_str(trimmed);
    }
    out
}

/// `:sort` over `lines`: `i` ignore case, `n` by the first number, `u`
/// unique, `reverse` for `:sort!`.
pub(super) fn sort_lines(lines: &[String], flags: &str, reverse: bool) -> Vec<String> {
    let icase = flags.contains('i');
    let numeric = flags.contains('n');
    let unique = flags.contains('u');
    let num_of = |s: &str| -> Option<i64> {
        let re = Regex::new(r"-?\d+").ok()?;
        re.find(s).and_then(|m| m.as_str().parse().ok())
    };
    let mut v: Vec<String> = lines.to_vec();
    if numeric {
        // Lines without a number sort first, in their original order.
        v.sort_by_key(|l| num_of(l).map_or((0, 0), |n| (1, n)));
    } else if icase {
        v.sort_by_key(|l| l.to_lowercase());
    } else {
        v.sort();
    }
    if reverse {
        v.reverse();
    }
    if unique {
        let key = |s: &String| {
            if icase { s.to_lowercase() } else { s.clone() }
        };
        v.dedup_by(|a, b| key(a) == key(b));
    }
    v
}

/// Split `:normal` keys into tokens: `<Esc>`, `<CR>`, `<BS>`, `<Tab>`,
/// `<C-x>`, `<lt>`, or single characters.
pub(super) fn normal_key_tokens(keys: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = keys;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
        {
            let inner = &rest[1..end];
            let known = matches!(
                inner.to_ascii_lowercase().as_str(),
                "esc" | "cr" | "enter" | "return" | "bs" | "tab" | "lt" | "space" | "del"
            ) || (inner.len() == 3
                && (inner.starts_with("C-") || inner.starts_with("c-")));
            if known {
                out.push(format!("<{inner}>"));
                rest = &rest[end + 1..];
                continue;
            }
        }
        out.push(c.to_string());
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// The keystroke a `:normal` token stands for. Named keys that are gpui
/// actions (Enter, Backspace, Tab) return `None`: the caller handles them.
pub(super) fn token_keystroke(tok: &str) -> Option<Keystroke> {
    let lower = tok.to_ascii_lowercase();
    let mut modifiers = gpui::Modifiers::default();
    let (key, key_char) = match lower.as_str() {
        "<esc>" => ("escape".to_string(), None),
        "<lt>" => ("<".to_string(), Some("<".to_string())),
        "<space>" => ("space".to_string(), Some(" ".to_string())),
        "<cr>" | "<enter>" | "<return>" | "<bs>" | "<tab>" | "<del>" => return None,
        t if t.starts_with("<c-") && t.len() == 5 => {
            modifiers.control = true;
            (t[3..4].to_string(), None)
        }
        _ => {
            let c = tok.chars().next()?;
            if c.is_ascii_uppercase() {
                modifiers.shift = true;
                (c.to_ascii_lowercase().to_string(), Some(c.to_string()))
            } else {
                (c.to_string(), Some(c.to_string()))
            }
        }
    };
    Some(Keystroke {
        modifiers,
        key,
        key_char,
    })
}

// --- the editor side -------------------------------------------------------------

/// The state of an interactive `:s///c`.
pub(super) struct SubConfirm {
    /// Remaining matches: `(start, end, replacement)` in ORIGINAL byte offsets
    /// (ascending); `shift` maps them onto the edited buffer.
    pub matches: std::collections::VecDeque<(usize, usize, String)>,
    pub shift: isize,
    pub done: usize,
    pub undo_base: UndoBase,
}

/// The last `:s`, for `:s` / `&` / `:&&` / `g&`.
#[derive(Clone, Debug)]
pub(super) struct LastSub {
    pub pattern: String,
    pub replacement: String,
    pub flags: String,
}

impl Editor {
    /// The last real line (1-based): a buffer ending in a newline has a
    /// phantom empty line after it that Vim doesn't count.
    pub(super) fn ex_last_line(&self) -> i64 {
        let n = self.active.buffer.line_count();
        let len = self.active.buffer.len();
        let trailing = len > 0 && self.active.buffer.slice_to_string(len - 1..len) == "\n";
        if trailing && n > 1 {
            (n - 1) as i64
        } else {
            n as i64
        }
    }

    fn ex_cur_line(&self) -> i64 {
        self.active.buffer.line_index_at_byte(self.point()) as i64 + 1
    }

    /// Line `l` (1-based) without its newline.
    fn ex_line(&self, l: i64) -> String {
        self.active
            .buffer
            .slice_to_string(self.active.buffer.line_range((l - 1).max(0) as usize))
    }

    fn ex_lines(&self, s: i64, e: i64) -> Vec<String> {
        (s..=e).map(|l| self.ex_line(l)).collect()
    }

    /// Byte span of lines `s..=e`' text (first line start → last line end,
    /// excluding the last newline).
    fn ex_text_span(&self, s: i64, e: i64) -> Range<usize> {
        let a = self.active.buffer.line_range((s - 1) as usize).start;
        let b = self.active.buffer.line_range((e - 1) as usize).end;
        a..b
    }

    /// Resolve one address to a 1-based line (not range-checked).
    fn ex_resolve_addr(&self, a: &Addr, cur: i64) -> Result<i64, String> {
        let base = match &a.base {
            AddrBase::Num(n) => *n,
            AddrBase::Cur => cur,
            AddrBase::Last => self.ex_last_line(),
            AddrBase::Mark(c) => {
                let c = if *c == '`' { '\'' } else { *c };
                let pos = if c.is_ascii_uppercase() {
                    self.vim
                        .global_marks
                        .get(&c)
                        .filter(|g| g.buffer == self.active.name)
                        .map(|g| g.pos)
                } else {
                    self.vim
                        .marks
                        .get(&self.active.name)
                        .and_then(|m| m.get(&c))
                        .copied()
                };
                let pos = pos.ok_or_else(|| format!("E20: Mark not set: {c}"))?;
                self.active
                    .buffer
                    .line_index_at_byte(pos.min(self.active.buffer.len())) as i64
                    + 1
            }
            AddrBase::Search(pat, forward) => {
                let pat = if pat.is_empty() {
                    self.search_query.clone().unwrap_or_default()
                } else {
                    pat.clone()
                };
                let re = Regex::new(&vim_pattern(&pat, false))
                    .map_err(|e| format!("Invalid regex: {e}"))?;
                let last = self.ex_last_line();
                let order: Vec<i64> = if *forward {
                    (cur + 1..=last).chain(1..=cur).collect()
                } else {
                    (1..cur).rev().chain((cur..=last).rev()).collect()
                };
                order
                    .into_iter()
                    .find(|&l| re.is_match(&self.ex_line(l)))
                    .ok_or_else(|| format!("E486: Pattern not found: {pat}"))?
            }
        };
        Ok(base + a.offset)
    }

    /// Resolve a range to `(start, end)` 1-based lines (swapped into order),
    /// defaulting to `default` when there is none.
    fn ex_resolve_range(
        &self,
        range: &Option<ExRange>,
        default: (i64, i64),
    ) -> Result<(i64, i64), String> {
        let cur = self.ex_cur_line();
        let last = self.ex_last_line();
        let (s, e) = match range {
            None => return Ok(default),
            Some(ExRange::Whole) => (1, last),
            Some(ExRange::One(a)) => {
                let l = self.ex_resolve_addr(a, cur)?;
                (l, l)
            }
            Some(ExRange::Two(a, semi, b)) => {
                let s = self.ex_resolve_addr(a, cur)?;
                let e = self.ex_resolve_addr(b, if *semi { s } else { cur })?;
                (s, e)
            }
        };
        let (s, e) = if s > e { (e, s) } else { (s, e) };
        if s < 0 || e > last {
            return Err("E16: Invalid range".into());
        }
        Ok((s.max(1), e.max(1)))
    }

    /// Run an ex line that has a range or names one of the line commands.
    /// Returns false when it isn't one (the caller's plain verbs take it).
    pub(super) fn ex_dispatch(&mut self, line: &ExLine, cx: &mut Context<Self>) -> bool {
        let cur = self.ex_cur_line();
        let res = match (line.name.as_str(), line.range.is_some()) {
            // `:{range}` alone: go to its last line.
            ("", true) => self.ex_goto(&line.range, cx),
            ("s" | "su" | "sub" | "substitute", _) => self.ex_sub(line, false, cx),
            ("&" | "&&", _) => self.ex_sub(line, true, cx),
            ("g" | "gl" | "global", _) => self.ex_global(line, !line.bang, cx),
            ("v" | "vg" | "vglobal", _) => self.ex_global(line, false, cx),
            ("norm" | "norma" | "normal", _) => self.ex_normal(line, cx),
            ("d" | "de" | "del" | "delete", _) => self.ex_delete(line, cx),
            ("y" | "ya" | "yank", _) => self.ex_yank(line, cx),
            ("m" | "mo" | "move", _) => self.ex_move_copy(line, true, cx),
            ("t" | "co" | "copy", _) => self.ex_move_copy(line, false, cx),
            ("j" | "jo" | "join", _) => self.ex_join(line, cx),
            (">" | ">>" | ">>>" | "<" | "<<" | "<<<", _) => self.ex_shift(line, cx),
            ("sor" | "sort", _) => self.ex_sort(line, cx),
            ("pu" | "put", _) => self.ex_put(line, cx),
            ("=", _) => {
                let l =
                    self.ex_resolve_range(&line.range, (self.ex_last_line(), self.ex_last_line()));
                l.map(|(_, e)| self.echo(e.to_string(), cx))
            }
            ("k" | "ma" | "mark", _) => {
                self.ex_resolve_range(&line.range, (cur, cur))
                    .map(|(_, e)| {
                        let pos = self.active.buffer.line_range((e - 1) as usize).start;
                        let keep = self.point();
                        self.collapse_to(pos);
                        self.evil_set_mark(&line.arg, cx);
                        self.collapse_to(keep);
                    })
            }
            _ => return false,
        };
        if let Err(e) = res {
            self.echo(e, cx);
        }
        cx.notify();
        true
    }

    fn ex_goto(&mut self, range: &Option<ExRange>, cx: &mut Context<Self>) -> Result<(), String> {
        let cur = self.ex_cur_line();
        let (_, e) = self.ex_resolve_range(range, (cur, cur))?;
        self.record_jump();
        let start = self.active.buffer.line_range((e - 1) as usize).start;
        let target = self.first_non_blank(start);
        self.move_to(target, cx);
        Ok(())
    }

    /// The count argument of `:d 3` / `:y a 3` / `:j 3`: the range becomes
    /// `count` lines from its last line.
    fn ex_apply_count(&self, s: i64, e: i64, count: Option<usize>) -> (i64, i64) {
        match count {
            Some(n) if n > 0 => (e, (e + n as i64 - 1).min(self.ex_last_line())),
            _ => (s, e),
        }
    }

    /// `[x] [count]` arguments of :d / :y.
    fn ex_reg_count(arg: &str) -> (Option<char>, Option<usize>) {
        let mut reg = None;
        let mut rest = arg.trim();
        if let Some(c) = rest.chars().next()
            && !c.is_ascii_digit()
        {
            reg = Some(c);
            rest = rest[c.len_utf8()..].trim();
        }
        (reg, rest.parse().ok())
    }

    /// `:[range]d [x] [count]` — delete lines (linewise, into register x).
    fn ex_delete(&mut self, line: &ExLine, cx: &mut Context<Self>) -> Result<(), String> {
        let cur = self.ex_cur_line();
        let (s, e) = self.ex_resolve_range(&line.range, (cur, cur))?;
        let (reg, count) = Self::ex_reg_count(&line.arg);
        let (s, e) = self.ex_apply_count(s, e, count);
        self.ex_delete_lines(s, e, reg, cx);
        Ok(())
    }

    /// Delete lines `s..=e` like `dd`: the text (with its newline) goes to the
    /// kill ring / registers as a linewise delete; on the buffer's last line the
    /// newline before goes instead.
    fn ex_delete_lines(&mut self, s: i64, e: i64, reg: Option<char>, cx: &mut Context<Self>) {
        let len = self.active.buffer.len();
        let a = self.active.buffer.line_range((s - 1) as usize).start;
        let le = self.active.buffer.line_range((e - 1) as usize).end;
        let (start, end) = if le < len {
            (a, le + 1)
        } else if a > 0 {
            (a - 1, le)
        } else {
            (a, le)
        };
        let mut text = self.ex_lines(s, e).join("\n");
        text.push('\n');
        if reg != Some('_') {
            self.set_clipboard(&text, cx);
            self.note_register_kill(registers::KillKind::Delete, reg, &text);
            self.push_kill(text);
            self.evil_yank_linewise = true;
            self.vim.registers.set_linewise_last(true);
        }
        self.edit(start..end, "");
        let line = (s - 1).min(self.ex_last_line() - 1).max(0) as usize;
        let pos = self.active.buffer.line_range(line).start;
        self.collapse_to(self.first_non_blank(pos));
    }

    /// `:[range]y [x] [count]` — yank lines.
    fn ex_yank(&mut self, line: &ExLine, cx: &mut Context<Self>) -> Result<(), String> {
        let cur = self.ex_cur_line();
        let (s, e) = self.ex_resolve_range(&line.range, (cur, cur))?;
        let (reg, count) = Self::ex_reg_count(&line.arg);
        let (s, e) = self.ex_apply_count(s, e, count);
        if reg == Some('_') {
            return Ok(());
        }
        let mut text = self.ex_lines(s, e).join("\n");
        text.push('\n');
        self.set_clipboard(&text, cx);
        self.note_register_kill(registers::KillKind::Yank, reg, &text);
        self.push_kill(text);
        self.evil_yank_linewise = true;
        self.vim.registers.set_linewise_last(true);
        Ok(())
    }

    /// `:[range]m {addr}` (move) / `:[range]t {addr}` (copy): below line
    /// `addr` (0 = above the first line).
    fn ex_move_copy(
        &mut self,
        line: &ExLine,
        mv: bool,
        _cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let cur = self.ex_cur_line();
        let (s, e) = self.ex_resolve_range(&line.range, (cur, cur))?;
        let (addr, _) = parse_addr(line.arg.trim()).ok_or("E14: Invalid address")?;
        let t = self.ex_resolve_addr(&addr, cur)?;
        if t < 0 || t > self.ex_last_line() {
            return Err("E16: Invalid range".into());
        }
        let block = self.ex_lines(s, e);
        let n = e - s + 1;
        if !mv {
            // Copy: insert after line t.
            let text = block.join("\n");
            if t == 0 {
                self.edit(0..0, &format!("{text}\n"));
            } else {
                let at = self.active.buffer.line_range((t - 1) as usize).end;
                self.edit(at..at, &format!("\n{text}"));
            }
            let last = t + n;
            let pos = self.active.buffer.line_range((last - 1) as usize).start;
            self.collapse_to(self.first_non_blank(pos));
            return Ok(());
        }
        if t >= s && t < e {
            return Err("E134: Cannot move a range of lines into itself".into());
        }
        if t == e || t == s - 1 {
            return Ok(()); // already there
        }
        // Rewrite the span covering the block and the target as one edit.
        let (lo, hi, new_lines, last) = if t > e {
            let mut v = self.ex_lines(e + 1, t);
            v.extend(block);
            (s, t, v, t)
        } else {
            let mut v = block;
            v.extend(self.ex_lines(t + 1, s - 1));
            (t + 1, e, v, t + n)
        };
        let span = self.ex_text_span(lo, hi);
        self.edit(span, &new_lines.join("\n"));
        let pos = self.active.buffer.line_range((last - 1) as usize).start;
        self.collapse_to(self.first_non_blank(pos));
        Ok(())
    }

    /// `:[range]j[!] [count]` — join lines (default: this and the next).
    fn ex_join(&mut self, line: &ExLine, _cx: &mut Context<Self>) -> Result<(), String> {
        let cur = self.ex_cur_line();
        let (mut s, mut e) = self.ex_resolve_range(&line.range, (cur, cur))?;
        if let Ok(n) = line.arg.trim().parse::<i64>() {
            s = e;
            e = s + n - 1;
        } else if s == e {
            e = s + 1;
        }
        let e = e.min(self.ex_last_line());
        if e <= s {
            return Ok(());
        }
        let joined = join_lines(&self.ex_lines(s, e), line.bang);
        let span = self.ex_text_span(s, e);
        let start = span.start;
        self.edit(span, &joined);
        self.collapse_to(self.first_non_blank(start));
        Ok(())
    }

    /// `:[range]>` / `:[range]<<` … — shift lines (one step per `>`).
    fn ex_shift(&mut self, line: &ExLine, cx: &mut Context<Self>) -> Result<(), String> {
        let cur = self.ex_cur_line();
        let (s, e) = self.ex_resolve_range(&line.range, (cur, cur))?;
        let (s, e) = self.ex_apply_count(s, e, line.arg.trim().parse().ok());
        let dir: isize = if line.name.starts_with('>') { 1 } else { -1 };
        let step = self.structural_indent_width(cx).max(1);
        for _ in 0..line.name.len() {
            self.shift_line_range((s - 1) as usize, (e - 1) as usize, dir, step);
        }
        let pos = self.active.buffer.line_range((e - 1) as usize).start;
        self.collapse_to(self.first_non_blank(pos));
        Ok(())
    }

    /// `:[range]sort[!] [i][n][u]` (default: every line).
    fn ex_sort(&mut self, line: &ExLine, _cx: &mut Context<Self>) -> Result<(), String> {
        let last = self.ex_last_line();
        let (s, e) = self.ex_resolve_range(&line.range, (1, last))?;
        let sorted = sort_lines(&self.ex_lines(s, e), &line.arg, line.bang);
        let span = self.ex_text_span(s, e);
        let start = span.start;
        self.edit(span, &sorted.join("\n"));
        self.collapse_to(start);
        Ok(())
    }

    /// `:[line]pu[!] [x]` — put register x (default: unnamed) as lines below
    /// (`!`: above) the line.
    fn ex_put(&mut self, line: &ExLine, cx: &mut Context<Self>) -> Result<(), String> {
        let cur = self.ex_cur_line();
        let (_, l) = self.ex_resolve_range(&line.range, (cur, cur))?;
        let reg = line.arg.trim().chars().next().unwrap_or('"');
        let v = self
            .evil_register_value(reg, cx)
            .ok_or_else(|| format!("E353: Nothing in register {reg}"))?;
        let text = v.text.strip_suffix('\n').unwrap_or(&v.text).to_string();
        let (at, ins, first) = if line.bang || l == 0 {
            let at = self.active.buffer.line_range((l.max(1) - 1) as usize).start;
            (at, format!("{text}\n"), l.max(1))
        } else {
            let at = self.active.buffer.line_range((l - 1) as usize).end;
            (at, format!("\n{text}"), l + 1)
        };
        self.edit(at..at, &ins);
        let pos = self.active.buffer.line_range((first - 1) as usize).start;
        self.collapse_to(self.first_non_blank(pos));
        Ok(())
    }

    /// `:s`, `:&`, `:&&`: substitute over the range (default: this line).
    fn ex_sub(&mut self, line: &ExLine, amp: bool, cx: &mut Context<Self>) -> Result<(), String> {
        let cur = self.ex_cur_line();
        let (s, e) = self.ex_resolve_range(&line.range, (cur, cur))?;
        let mut spec = parse_sub(&line.arg);
        // `:&&` / `:s&` keep the last flags.
        let keep_flags = line.name == "&&" || spec.flags.starts_with('&');
        if amp || spec.bare {
            let last = self
                .vim
                .last_sub
                .clone()
                .ok_or("E35: No previous substitute")?;
            spec.pattern = last.pattern;
            spec.replacement = last.replacement;
            if keep_flags {
                spec.flags = format!("{}{}", last.flags, spec.flags.trim_start_matches('&'));
            }
        } else if spec.flags.starts_with('&')
            && let Some(last) = &self.vim.last_sub
        {
            spec.flags = format!("{}{}", last.flags, &spec.flags[1..]);
        }
        if spec.pattern.is_empty() {
            spec.pattern = self
                .search_query
                .clone()
                .ok_or("E35: No previous regular expression")?;
        }
        self.vim.last_sub = Some(LastSub {
            pattern: spec.pattern.clone(),
            replacement: spec.replacement.clone(),
            flags: spec.flags.replace('&', ""),
        });
        self.search_query = Some(spec.pattern.clone());
        self.ex_substitute_range(s, e, &spec, cx)
    }

    /// The substitute engine over lines `s..=e`.
    pub(super) fn ex_substitute_range(
        &mut self,
        s: i64,
        e: i64,
        spec: &SubSpec,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let (s, e) = self.ex_apply_count(s, e, spec.count);
        let icase = spec.flags.contains('i') && !spec.flags.contains('I');
        let re = Regex::new(&vim_pattern(&spec.pattern, icase))
            .map_err(|err| format!("Invalid regex: {err}"))?;
        let rep = vim_replacement(&spec.replacement);
        let global = spec.flags.contains('g');
        let lines = self.ex_lines(s, e);
        if spec.flags.contains('n') {
            let n: usize = lines
                .iter()
                .map(|l| {
                    let k = re.find_iter(l).count();
                    if global { k } else { k.min(1) }
                })
                .sum();
            self.echo(format!("{n} match{}", if n == 1 { "" } else { "es" }), cx);
            return Ok(());
        }
        if spec.flags.contains('c') {
            return self.ex_sub_confirm_start(s, e, &re, &rep, global, cx);
        }
        let (new, n) = substitute_lines(&lines, &re, &rep, global);
        if n == 0 {
            if !spec.flags.contains('e') {
                return Err(format!("E486: Pattern not found: {}", spec.pattern));
            }
            return Ok(());
        }
        // One edit per changed line, bottom-up (marks on untouched lines stay
        // put), grouped into one undo step.
        self.begin_undo_group();
        let mut last_changed = s;
        for (i, (old, new)) in lines.iter().zip(&new).enumerate().rev() {
            if old != new {
                let l = s + i as i64;
                let span = self.ex_text_span(l, l);
                self.edit(span, new);
                last_changed = last_changed.max(l);
            }
        }
        self.end_undo_group();
        // The cursor goes to the last substituted line (its line count can
        // have grown with `\n` replacements — clamp).
        let line = (last_changed - 1).min(self.ex_last_line() - 1).max(0) as usize;
        let pos = self.active.buffer.line_range(line).start;
        self.collapse_to(self.first_non_blank(pos));
        if n > 1 {
            self.echo(format!("{n} substitutions"), cx);
        }
        Ok(())
    }

    /// Begin `:s///c`: collect every match (with its expanded replacement) and
    /// prompt for the first.
    fn ex_sub_confirm_start(
        &mut self,
        s: i64,
        e: i64,
        re: &Regex,
        rep: &str,
        global: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let mut matches = std::collections::VecDeque::new();
        for l in s..=e {
            let base = self.active.buffer.line_range((l - 1) as usize).start;
            let text = self.ex_line(l);
            for caps in re.captures_iter(&text) {
                let m = caps.get(0).expect("group 0");
                let mut out = String::new();
                caps.expand(rep, &mut out);
                matches.push_back((base + m.start(), base + m.end(), out));
                if !global {
                    break;
                }
            }
        }
        if matches.is_empty() {
            return Err("E486: Pattern not found".into());
        }
        self.vim.sub_confirm = Some(SubConfirm {
            matches,
            shift: 0,
            done: 0,
            undo_base: self.undo_base(),
        });
        self.ex_sub_confirm_show(cx);
        Ok(())
    }

    /// Highlight the next pending match and prompt.
    fn ex_sub_confirm_show(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.vim.sub_confirm.as_ref() else {
            return;
        };
        let Some((a, b, rep)) = c.matches.front().cloned() else {
            self.ex_sub_confirm_finish(cx);
            return;
        };
        let shift = c.shift;
        let a = (a as isize + shift).max(0) as usize;
        let b = (b as isize + shift).max(0) as usize;
        self.active.selected_range = a..b;
        self.active.selection_reversed = false;
        self.active.mark_active = true;
        self.ensure_cursor_visible();
        self.status = format!("replace with {rep} (y/n/a/q/l)?").into();
        cx.notify();
    }

    fn ex_sub_confirm_finish(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.vim.sub_confirm.take() {
            self.squash_undo_since(&c.undo_base);
            let p = self.point();
            self.collapse_to(p);
            self.status = SharedString::default();
            self.echo(
                format!(
                    "{} substitution{}",
                    c.done,
                    if c.done == 1 { "" } else { "s" }
                ),
                cx,
            );
        }
        cx.notify();
    }

    /// Replace the match at the front of the confirm queue.
    fn ex_sub_confirm_replace_front(&mut self) {
        let Some(c) = self.vim.sub_confirm.as_mut() else {
            return;
        };
        let Some((a, b, rep)) = c.matches.pop_front() else {
            return;
        };
        let shift = c.shift;
        c.shift += rep.len() as isize - (b - a) as isize;
        c.done += 1;
        let a = (a as isize + shift).max(0) as usize;
        let b = (b as isize + shift).max(0) as usize;
        self.active.mark_active = false;
        let end = self.edit(a..b, &rep);
        self.collapse_to(end);
    }

    /// Keys while `:s///c` prompts: y (replace), n (skip), a (all the rest),
    /// q / Escape (stop), l (replace this one and stop).
    pub(super) fn key_sub_confirm(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> Handled {
        if self.vim.sub_confirm.is_none() {
            return Handled::No;
        }
        cx.stop_propagation();
        let chord = chord_string(ks);
        match chord.as_str() {
            "y" => self.ex_sub_confirm_replace_front(),
            "n" => {
                if let Some(c) = self.vim.sub_confirm.as_mut() {
                    c.matches.pop_front();
                }
            }
            "a" => {
                while self
                    .vim
                    .sub_confirm
                    .as_ref()
                    .is_some_and(|c| !c.matches.is_empty())
                {
                    self.ex_sub_confirm_replace_front();
                }
            }
            "l" => {
                self.ex_sub_confirm_replace_front();
                self.ex_sub_confirm_finish(cx);
                return Handled::Yes;
            }
            "q" | "escape" | "ctrl-g" | "ctrl-c" => {
                self.ex_sub_confirm_finish(cx);
                return Handled::Yes;
            }
            _ => return Handled::Yes,
        }
        self.ex_sub_confirm_show(cx);
        Handled::Yes
    }

    /// `:[range]g/pat/cmd` (`:g!` / `:v` for non-matching lines). The
    /// matching lines are marked first; the command then runs on each still
    /// existing line, top-down. `normal` commands replay their keys through
    /// the replay queue (bottom-up, so edits don't move lines still to come).
    fn ex_global(
        &mut self,
        line: &ExLine,
        matching: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let last = self.ex_last_line();
        let (s, e) = self.ex_resolve_range(&line.range, (1, last))?;
        let arg = line.arg.trim_start();
        let delim = arg.chars().next().ok_or("E476: Invalid command")?;
        let (pat, cmd) = take_delimited(&arg[delim.len_utf8()..], delim);
        let pat = if pat.is_empty() {
            self.search_query
                .clone()
                .ok_or("E35: No previous regular expression")?
        } else {
            pat
        };
        let re =
            Regex::new(&vim_pattern(&pat, false)).map_err(|err| format!("Invalid regex: {err}"))?;
        self.search_query = Some(pat);
        let cmd = if cmd.trim().is_empty() {
            "p".to_string()
        } else {
            cmd.trim().to_string()
        };
        let lines: Vec<i64> = (s..=e)
            .filter(|&l| re.is_match(&self.ex_line(l)) == matching)
            .collect();
        if lines.is_empty() {
            return Err("E486: Pattern not found".into());
        }
        let sub = parse_ex_line(&cmd);
        if matches!(sub.name.as_str(), "norm" | "norma" | "normal") {
            let mut events = Vec::new();
            for l in lines.iter().rev() {
                events.push(MacroEvent::Command(format!("(evil-ex-goto-line \"{l}\")")));
                for tok in normal_key_tokens(&sub.arg) {
                    events.push(MacroEvent::Key(tok));
                }
                events.push(MacroEvent::Key("<Esc>".into()));
            }
            self.replay_events(events, true, cx);
            return Ok(());
        }
        if matches!(
            sub.name.as_str(),
            "p" | "pr" | "print" | "#" | "nu" | "number"
        ) {
            let text = lines
                .iter()
                .map(|&l| format!("{l:>6} {}", self.ex_line(l)))
                .collect::<Vec<_>>()
                .join("\n");
            self.render_buffer(
                "*global*".into(),
                "help-mode".into(),
                text + "\n",
                Vec::new(),
                cx,
            );
            return Ok(());
        }
        // Anchors (line starts) that ride along with the edits; one whose
        // line was deleted is dropped.
        self.vim.ex_anchors = lines
            .iter()
            .map(|&l| Some(self.active.buffer.line_range((l - 1) as usize).start))
            .collect();
        let base = self.undo_base();
        let mut i = 0;
        while i < self.vim.ex_anchors.len() {
            if let Some(pos) = self.vim.ex_anchors[i] {
                let l = self
                    .active
                    .buffer
                    .line_index_at_byte(pos.min(self.active.buffer.len()));
                let pos = self.active.buffer.line_range(l).start;
                self.collapse_to(pos);
                let mut one = sub.clone();
                if one.range.is_none() {
                    one.range = Some(ExRange::One(Addr {
                        base: AddrBase::Cur,
                        offset: 0,
                    }));
                }
                if !self.ex_dispatch(&one, cx) {
                    self.vim.ex_anchors.clear();
                    self.squash_undo_since(&base);
                    return Err(format!("Not supported under :g: {cmd}"));
                }
            }
            i += 1;
        }
        self.vim.ex_anchors.clear();
        self.squash_undo_since(&base);
        Ok(())
    }

    /// `:[range]norm[al][!] {keys}` — run Normal-state keys on each line of
    /// the range (or once, here).
    fn ex_normal(&mut self, line: &ExLine, cx: &mut Context<Self>) -> Result<(), String> {
        let tokens = normal_key_tokens(&line.arg);
        if tokens.is_empty() {
            return Err("E471: Argument required".into());
        }
        let mut events = Vec::new();
        match &line.range {
            None => {
                events.extend(tokens.into_iter().map(MacroEvent::Key));
                events.push(MacroEvent::Key("<Esc>".into()));
            }
            Some(_) => {
                let cur = self.ex_cur_line();
                let (s, e) = self.ex_resolve_range(&line.range, (cur, cur))?;
                for l in (s..=e).rev() {
                    events.push(MacroEvent::Command(format!("(evil-ex-goto-line \"{l}\")")));
                    events.extend(tokens.iter().cloned().map(MacroEvent::Key));
                    events.push(MacroEvent::Key("<Esc>".into()));
                }
            }
        }
        self.replay_events(events, true, cx);
        Ok(())
    }

    /// `(evil-ex-goto-line "N")` — move to (1-based) line N's start (replay
    /// helper for `:g … normal`).
    pub(super) fn evil_ex_goto_line(&mut self, n: &str, cx: &mut Context<Self>) {
        let Ok(n) = n.trim().parse::<i64>() else {
            return;
        };
        let line = (n - 1).clamp(0, self.ex_last_line() - 1) as usize;
        let pos = self.active.buffer.line_range(line).start;
        self.move_to(pos, cx);
    }

    /// Feed one `:normal` key token, as if typed.
    pub(super) fn replay_key(&mut self, tok: &str, cx: &mut Context<Self>) {
        // Leaving a recording-free context: replayed keys are never recorded.
        let lower = tok.to_ascii_lowercase();
        if self.evil == EvilState::Insert {
            match lower.as_str() {
                "<cr>" | "<enter>" | "<return>" => {
                    self.apply_command(EditorCommand::Newline, cx);
                    return;
                }
                "<bs>" => {
                    self.apply_command(EditorCommand::DeleteBackwardChar, cx);
                    return;
                }
                "<del>" => {
                    self.apply_command(EditorCommand::DeleteChar, cx);
                    return;
                }
                "<tab>" => {
                    self.insert_text("\t", cx);
                    return;
                }
                _ => {}
            }
        }
        let Some(ks) = token_keystroke(tok) else {
            return;
        };
        // In Insert a printable key types (the IME path does that for real
        // keystrokes); everything else goes through the key layers.
        if self.evil == EvilState::Insert
            && !ks.modifiers.control
            && let Some(text) = ks.key_char.clone()
        {
            self.type_text(None, &text, cx);
            return;
        }
        self.dispatch_keystroke(&ks, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(base: AddrBase, offset: i64) -> Addr {
        Addr { base, offset }
    }

    #[test]
    fn parses_ranges_and_commands() {
        let l = parse_ex_line("%s/a/b/g");
        assert_eq!(l.range, Some(ExRange::Whole));
        assert_eq!(l.name, "s");
        assert_eq!(l.arg, "/a/b/g");
        let l = parse_ex_line("'<,'>d");
        assert_eq!(
            l.range,
            Some(ExRange::Two(
                addr(AddrBase::Mark('<'), 0),
                false,
                addr(AddrBase::Mark('>'), 0)
            ))
        );
        assert_eq!(l.name, "d");
        let l = parse_ex_line(".,.+3j!");
        assert_eq!(
            l.range,
            Some(ExRange::Two(
                addr(AddrBase::Cur, 0),
                false,
                addr(AddrBase::Cur, 3)
            ))
        );
        assert_eq!((l.name.as_str(), l.bang), ("j", true));
        let l = parse_ex_line("/foo/;+2y a");
        assert_eq!(
            l.range,
            Some(ExRange::Two(
                addr(AddrBase::Search("foo".into(), true), 0),
                true,
                addr(AddrBase::Cur, 2)
            ))
        );
        assert_eq!((l.name.as_str(), l.arg.as_str()), ("y", "a"));
        let l = parse_ex_line("42");
        assert_eq!(l.range, Some(ExRange::One(addr(AddrBase::Num(42), 0))));
        assert_eq!(l.name, "");
        let l = parse_ex_line("$-1");
        assert_eq!(l.range, Some(ExRange::One(addr(AddrBase::Last, -1))));
        let l = parse_ex_line("m0");
        assert_eq!((l.name.as_str(), l.arg.as_str()), ("m", "0"));
        let l = parse_ex_line("t.");
        assert_eq!((l.name.as_str(), l.arg.as_str()), ("t", "."));
        let l = parse_ex_line("2,4>>");
        assert_eq!(l.name, ">>");
        let l = parse_ex_line("g!/x/d");
        assert_eq!(
            (l.name.as_str(), l.bang, l.arg.as_str()),
            ("g", true, "/x/d")
        );
        let l = parse_ex_line("&&");
        assert_eq!(l.name, "&&");
        let l = parse_ex_line("ka");
        assert_eq!((l.name.as_str(), l.arg.as_str()), ("k", "a"));
        let l = parse_ex_line("wq");
        assert_eq!((l.name.as_str(), l.range.is_none()), ("wq", true));
        let l = parse_ex_line("q!");
        assert_eq!((l.name.as_str(), l.bang), ("q", true));
    }

    #[test]
    fn parses_substitutions() {
        let s = parse_sub("/a\\/b/c/gi 3");
        assert_eq!(s.pattern, "a/b");
        assert_eq!(s.replacement, "c");
        assert_eq!(s.flags, "gi");
        assert_eq!(s.count, Some(3));
        let s = parse_sub("#x#y#");
        assert_eq!((s.pattern.as_str(), s.replacement.as_str()), ("x", "y"));
        let s = parse_sub("");
        assert!(s.bare);
        let s = parse_sub("g");
        assert!(s.bare && s.flags == "g");
        assert_eq!(vim_replacement(r"<\1>&\&$"), "<${1}>${0}&$$");
        assert_eq!(vim_pattern(r"\<foo\>", false), r"\bfoo\b");
        assert_eq!(vim_pattern(r"\cFoo", false), "(?i)Foo");
    }

    #[test]
    fn line_transforms() {
        let lines: Vec<String> = ["foo foo", "bar", "foo"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let re = Regex::new("foo").unwrap();
        let (out, n) = substitute_lines(&lines, &re, "X", false);
        assert_eq!(out, vec!["X foo", "bar", "X"]);
        assert_eq!(n, 2);
        let (out, n) = substitute_lines(&lines, &re, "X", true);
        assert_eq!(out[0], "X X");
        assert_eq!(n, 3);
        let j: Vec<String> = ["a", "   b", ")c", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(join_lines(&j, false), "a b)c");
        assert_eq!(join_lines(&j, true), "a   b)c");
        let v: Vec<String> = ["b10", "a2", "B1", "a2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(sort_lines(&v, "", false), vec!["B1", "a2", "a2", "b10"]);
        assert_eq!(sort_lines(&v, "u", false), vec!["B1", "a2", "b10"]);
        assert_eq!(sort_lines(&v, "n", false), vec!["B1", "a2", "a2", "b10"]);
        assert_eq!(sort_lines(&v, "i", true), vec!["b10", "B1", "a2", "a2"]);
        assert_eq!(
            normal_key_tokens("A;<Esc>x<lt>"),
            vec!["A", ";", "<Esc>", "x", "<lt>"]
        );
        let ks = token_keystroke("A").unwrap();
        assert!(ks.modifiers.shift && ks.key == "a");
        assert_eq!(token_keystroke("<Esc>").unwrap().key, "escape");
        assert!(token_keystroke("<CR>").is_none());
        assert_eq!(
            take_delimited("a\\/b/rest", '/'),
            ("a/b".to_string(), "rest")
        );
    }
}
