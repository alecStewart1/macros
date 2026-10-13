//! Vim ex command lines: line ranges (`%`, `.`, `$`, numbers, `'a`, `'<,'>`,
//! `/pat/`, offsets), `:s` with flags and confirmation, `&` / `:&&` / `g&`,
//! `:g` / `:v`, `:normal`, `:d` `:y` `:m` `:t` `:j` `:>` `:<` `:sort` `:put`,
//! and `:{n}`. Parsing and the line transformations are pure functions (unit
//! tested below); the `Editor` methods resolve addresses against the buffer
//! and apply each command as one edit of the smallest span it touches, so
//! marks and the jump list ride along.

use std::sync::LazyLock;

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

/// An ex command, resolved from any of its names and abbreviations by
/// [`ExCmd::lookup`] — the one alias table. The line commands (the first
/// group) take a range and run under `:g`; the rest are plain verbs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExCmd {
    Substitute,
    /// `:&` / `:&&` — repeat the last substitute.
    SubRepeat,
    Global,
    VGlobal,
    Normal,
    Delete,
    Yank,
    Move,
    Copy,
    Join,
    /// `:>` / `:<<` … (one step per character).
    Shift,
    Sort,
    Put,
    /// `:=` — echo a line number.
    LineNumber,
    Mark,
    /// `:p` / `:#` / `:nu` — only meaningful as `:g`'s command.
    Print,
    Write,
    Quit,
    WriteQuit,
    WriteQuitAll,
    Edit,
    NoHighlight,
    Buffer,
    BufferNext,
    BufferPrev,
    BufferDelete,
    Split,
    VSplit,
    Only,
    New,
    VNew,
    WinCmd,
    Close,
    Registers,
    Marks,
    Jumps,
    ClearJumps,
    DelMarks,
}

impl ExCmd {
    /// The command named `name` (as [`parse_ex_line`] splits it off).
    pub(super) fn lookup(name: &str) -> Option<Self> {
        use ExCmd::*;
        Some(match name {
            "s" | "su" | "sub" | "substitute" => Substitute,
            "&" | "&&" => SubRepeat,
            "g" | "gl" | "global" => Global,
            "v" | "vg" | "vglobal" => VGlobal,
            "norm" | "norma" | "normal" => Normal,
            "d" | "de" | "del" | "delete" => Delete,
            "y" | "ya" | "yank" => Yank,
            "m" | "mo" | "move" => Move,
            "t" | "co" | "copy" => Copy,
            "j" | "jo" | "join" => Join,
            ">" | ">>" | ">>>" | "<" | "<<" | "<<<" => Shift,
            "sor" | "sort" => Sort,
            "pu" | "put" => Put,
            "=" => LineNumber,
            "k" | "ma" | "mark" => Mark,
            "p" | "pr" | "print" | "#" | "nu" | "number" => Print,
            "w" | "write" => Write,
            "q" | "quit" | "qa" | "qall" | "quitall" => Quit,
            "wq" | "x" | "xit" => WriteQuit,
            "wqa" | "wqall" | "xa" | "xall" => WriteQuitAll,
            "e" | "edit" => Edit,
            "noh" | "nohl" | "nohlsearch" => NoHighlight,
            "b" | "bu" | "buf" | "buffer" => Buffer,
            "bn" | "bnext" => BufferNext,
            "bp" | "bN" | "bprev" | "bprevious" | "bNext" => BufferPrev,
            "bd" | "bdel" | "bdelete" => BufferDelete,
            "split" | "sp" | "spl" => Split,
            "vsplit" | "vs" | "vsp" | "vspl" => VSplit,
            "only" | "on" => Only,
            "new" => New,
            "vne" | "vnew" => VNew,
            "winc" | "wincmd" => WinCmd,
            "close" | "clo" => Close,
            "reg" | "registers" | "di" | "display" => Registers,
            "marks" => Marks,
            "ju" | "jumps" => Jumps,
            "cle" | "clearjumps" => ClearJumps,
            "delm" | "delmarks" => DelMarks,
            _ => return None,
        })
    }
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
    static NUMBER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"-?\d+").expect("valid number regex"));
    let num_of = |s: &str| -> Option<i64> { NUMBER.find(s).and_then(|m| m.as_str().parse().ok()) };
    let mut v: Vec<String> = lines.to_vec();
    // `sort_by_cached_key` is stable and computes each key once (not once
    // per comparison).
    if numeric {
        // Lines without a number sort first, in their original order.
        v.sort_by_cached_key(|l| num_of(l).map_or((0, 0), |n| (1, n)));
    } else if icase {
        v.sort_by_cached_key(|l| l.to_lowercase());
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

/// The replay events running `:normal` keys on each of `lines` in turn:
/// go to the line, type the keys, then `<Esc>` back to Normal.
fn normal_events_on_lines(lines: impl Iterator<Item = i64>, tokens: &[String]) -> Vec<MacroEvent> {
    let mut events = Vec::new();
    for l in lines {
        events.push(MacroEvent::Command(format!("(evil-ex-goto-line \"{l}\")")));
        events.extend(tokens.iter().cloned().map(MacroEvent::Key));
        events.push(MacroEvent::Key("<Esc>".into()));
    }
    events
}

// --- the editor side -------------------------------------------------------------

/// Why an ex command failed — echoed through `Display`, in Vim's wording
/// (with its `E…:` code where Vim has one).
#[derive(Debug)]
pub(super) enum ExError {
    InvalidAddress,
    InvalidRange,
    MarkNotSet(char),
    NoPreviousSubstitute,
    NoPreviousRegex,
    EmptyRegister(char),
    MoveIntoItself,
    ArgumentRequired,
    InvalidCommand,
    /// The pattern, when the message names it.
    PatternNotFound(Option<String>),
    InvalidRegex(regex::Error),
    /// The `:g` sub-command that isn't a line command.
    NotUnderGlobal(String),
}

impl std::fmt::Display for ExError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAddress => f.write_str("E14: Invalid address"),
            Self::InvalidRange => f.write_str("E16: Invalid range"),
            Self::MarkNotSet(c) => write!(f, "E20: Mark not set: {c}"),
            Self::NoPreviousSubstitute => f.write_str("E35: No previous substitute"),
            Self::NoPreviousRegex => f.write_str("E35: No previous regular expression"),
            Self::EmptyRegister(r) => write!(f, "E353: Nothing in register {r}"),
            Self::MoveIntoItself => f.write_str("E134: Cannot move a range of lines into itself"),
            Self::ArgumentRequired => f.write_str("E471: Argument required"),
            Self::InvalidCommand => f.write_str("E476: Invalid command"),
            Self::PatternNotFound(Some(pat)) => write!(f, "E486: Pattern not found: {pat}"),
            Self::PatternNotFound(None) => f.write_str("E486: Pattern not found"),
            Self::InvalidRegex(e) => write!(f, "Invalid regex: {e}"),
            Self::NotUnderGlobal(cmd) => write!(f, "Not supported under :g: {cmd}"),
        }
    }
}

impl std::error::Error for ExError {}

/// The 0-based index of 1-based line `l` (below 1 is the first line).
fn line_index(l: i64) -> usize {
    (l - 1).max(0) as usize
}

/// The state of an interactive `:s///c`.
pub(super) struct SubConfirm {
    /// Remaining matches: `(start, end, replacement)` in ORIGINAL byte offsets
    /// (ascending); `shift` maps them onto the edited buffer.
    pub matches: std::collections::VecDeque<(usize, usize, String)>,
    pub shift: isize,
    pub done: usize,
    pub undo_base: UndoBase,
}

impl SubConfirm {
    /// ORIGINAL byte span `a..b` mapped onto the edited buffer.
    fn shifted(&self, a: usize, b: usize) -> Range<usize> {
        let at = |p: usize| (p as isize + self.shift).max(0) as usize;
        at(a)..at(b)
    }
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

    /// Byte range of line `l` (1-based; below 1 is the first line), excluding
    /// its line break.
    fn ex_line_range(&self, l: i64) -> Range<usize> {
        self.active.buffer.line_range(line_index(l))
    }

    /// Line `l` (1-based) without its newline.
    fn ex_line(&self, l: i64) -> String {
        self.active.buffer.slice_to_string(self.ex_line_range(l))
    }

    /// Park the cursor on line `l`'s first non-blank — where the line
    /// commands leave it. `l` is clamped to the buffer's real lines (an edit
    /// may have shrunk them).
    fn ex_land_on(&mut self, l: i64) {
        let l = l.min(self.ex_last_line()).max(1);
        let pos = self.ex_line_range(l).start;
        self.collapse_to(self.first_non_blank(pos));
    }

    fn ex_lines(&self, s: i64, e: i64) -> Vec<String> {
        (s..=e).map(|l| self.ex_line(l)).collect()
    }

    /// Byte span of lines `s..=e`' text (first line start → last line end,
    /// excluding the last newline).
    fn ex_text_span(&self, s: i64, e: i64) -> Range<usize> {
        let a = self.ex_line_range(s).start;
        let b = self.ex_line_range(e).end;
        a..b
    }

    /// The buffer's line break: `\r\n` when its first line ends in one.
    fn ex_eol(&self) -> &'static str {
        let end = self.active.buffer.line_range(0).end;
        let len = self.active.buffer.len();
        if end < len && self.active.buffer.slice_to_string(end..end + 1) == "\r" {
            "\r\n"
        } else {
            "\n"
        }
    }

    /// Byte span removing lines `s..=e` whole, line break included (CRLF
    /// aware): the break after them, or — on the buffer's last line — the
    /// break before them.
    fn ex_line_block_span(&self, s: i64, e: i64) -> Range<usize> {
        let a = self.ex_line_range(s).start;
        let le = self.ex_line_range(e).end;
        if line_index(e) + 1 < self.active.buffer.line_count() {
            a..self.ex_line_range(e + 1).start
        } else if s > 1 {
            self.ex_line_range(s - 1).end..le
        } else {
            a..le
        }
    }

    /// Resolve one address to a 1-based line (not range-checked).
    fn ex_resolve_addr(&self, a: &Addr, cur: i64) -> Result<i64, ExError> {
        let base = match &a.base {
            AddrBase::Num(n) => *n,
            AddrBase::Cur => cur,
            AddrBase::Last => self.ex_last_line(),
            AddrBase::Mark(c) => {
                let c = super::vim::normalize_mark(*c);
                let pos = self.mark_pos_in_active(c).ok_or(ExError::MarkNotSet(c))?;
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
                let re = Regex::new(&vim_pattern(&pat, false)).map_err(ExError::InvalidRegex)?;
                let last = self.ex_last_line();
                let order: Vec<i64> = if *forward {
                    (cur + 1..=last).chain(1..=cur).collect()
                } else {
                    (1..cur).rev().chain((cur..=last).rev()).collect()
                };
                order
                    .into_iter()
                    .find(|&l| re.is_match(&self.ex_line(l)))
                    .ok_or_else(|| ExError::PatternNotFound(Some(pat.clone())))?
            }
        };
        Ok(base + a.offset)
    }

    /// Resolve a range to `(start, end)` 1-based lines (swapped into order),
    /// defaulting to `default` when there is none.
    fn ex_resolve_range(
        &self,
        range: Option<&ExRange>,
        default: (i64, i64),
    ) -> Result<(i64, i64), ExError> {
        let (s, e) = self.ex_resolve_range0(range, default)?;
        Ok((s.max(1), e.max(1)))
    }

    /// [`Self::ex_resolve_range`] keeping a line 0 (`:0put`: above the first
    /// line).
    fn ex_resolve_range0(
        &self,
        range: Option<&ExRange>,
        default: (i64, i64),
    ) -> Result<(i64, i64), ExError> {
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
            return Err(ExError::InvalidRange);
        }
        Ok((s, e))
    }

    /// Run an ex line that has a range or names one of the line commands.
    /// Returns false when it isn't one (the caller's plain verbs take it).
    pub(super) fn ex_dispatch(&mut self, line: &ExLine, cx: &mut Context<Self>) -> bool {
        let cur = self.ex_cur_line();
        // `:{range}` alone: go to its last line.
        let goto = line.name.is_empty() && line.range.is_some();
        let res = match ExCmd::lookup(&line.name) {
            _ if goto => self.ex_goto(line.range.as_ref(), cx),
            Some(ExCmd::Substitute) => self.ex_sub(line, false, cx),
            Some(ExCmd::SubRepeat) => self.ex_sub(line, true, cx),
            Some(ExCmd::Global) => self.ex_global(line, !line.bang, cx),
            Some(ExCmd::VGlobal) => self.ex_global(line, false, cx),
            Some(ExCmd::Normal) => self.ex_normal(line, cx),
            Some(ExCmd::Delete) => self.ex_delete(line, cx),
            Some(ExCmd::Yank) => self.ex_yank(line, cx),
            Some(ExCmd::Move) => self.ex_move_copy(line, true, cx),
            Some(ExCmd::Copy) => self.ex_move_copy(line, false, cx),
            Some(ExCmd::Join) => self.ex_join(line, cx),
            Some(ExCmd::Shift) => self.ex_shift(line, cx),
            Some(ExCmd::Sort) => self.ex_sort(line, cx),
            Some(ExCmd::Put) => self.ex_put(line, cx),
            Some(ExCmd::LineNumber) => {
                let l = self.ex_resolve_range(
                    line.range.as_ref(),
                    (self.ex_last_line(), self.ex_last_line()),
                );
                l.map(|(_, e)| self.echo(e.to_string(), cx))
            }
            Some(ExCmd::Mark) => {
                self.ex_resolve_range(line.range.as_ref(), (cur, cur))
                    .map(|(_, e)| {
                        let pos = self.ex_line_range(e).start;
                        let keep = self.point();
                        self.collapse_to(pos);
                        self.evil_set_mark(&line.arg, cx);
                        self.collapse_to(keep);
                    })
            }
            _ => return false,
        };
        if let Err(e) = res {
            self.echo(e.to_string(), cx);
        }
        cx.notify();
        true
    }

    fn ex_goto(&mut self, range: Option<&ExRange>, cx: &mut Context<Self>) -> Result<(), ExError> {
        let cur = self.ex_cur_line();
        let (_, e) = self.ex_resolve_range(range, (cur, cur))?;
        self.record_jump();
        let start = self.ex_line_range(e).start;
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
    fn ex_delete(&mut self, line: &ExLine, cx: &mut Context<Self>) -> Result<(), ExError> {
        let cur = self.ex_cur_line();
        let (s, e) = self.ex_resolve_range(line.range.as_ref(), (cur, cur))?;
        let (reg, count) = Self::ex_reg_count(&line.arg);
        let (s, e) = self.ex_apply_count(s, e, count);
        self.ex_delete_lines(s, e, reg, cx);
        Ok(())
    }

    /// Delete lines `s..=e` like `dd`: the text (with its newline) goes to the
    /// kill ring / registers as a linewise delete; on the buffer's last line the
    /// newline before goes instead.
    fn ex_delete_lines(&mut self, s: i64, e: i64, reg: Option<char>, cx: &mut Context<Self>) {
        let span = self.ex_line_block_span(s, e);
        let mut text = self.ex_lines(s, e).join("\n");
        text.push('\n');
        self.file_kill(registers::KillKind::Delete, reg, text, true, cx);
        self.edit(span, "");
        self.ex_land_on(s);
    }

    /// `:[range]y [x] [count]` — yank lines.
    fn ex_yank(&mut self, line: &ExLine, cx: &mut Context<Self>) -> Result<(), ExError> {
        let cur = self.ex_cur_line();
        let (s, e) = self.ex_resolve_range(line.range.as_ref(), (cur, cur))?;
        let (reg, count) = Self::ex_reg_count(&line.arg);
        let (s, e) = self.ex_apply_count(s, e, count);
        if reg == Some('_') {
            return Ok(());
        }
        let mut text = self.ex_lines(s, e).join("\n");
        text.push('\n');
        self.file_kill(registers::KillKind::Yank, reg, text, true, cx);
        Ok(())
    }

    /// `:[range]m {addr}` (move) / `:[range]t {addr}` (copy): below line
    /// `addr` (0 = above the first line).
    fn ex_move_copy(
        &mut self,
        line: &ExLine,
        mv: bool,
        _cx: &mut Context<Self>,
    ) -> Result<(), ExError> {
        let cur = self.ex_cur_line();
        let (s, e) = self.ex_resolve_range(line.range.as_ref(), (cur, cur))?;
        let (addr, _) = parse_addr(line.arg.trim()).ok_or(ExError::InvalidAddress)?;
        let t = self.ex_resolve_addr(&addr, cur)?;
        if t < 0 || t > self.ex_last_line() {
            return Err(ExError::InvalidRange);
        }
        let block = self.ex_lines(s, e);
        let n = e - s + 1;
        let eol = self.ex_eol();
        let text = block.join(eol);
        // Insert the block after line `t` (0 = above the first line).
        let insert_after = |ed: &mut Self, t: i64| {
            if t == 0 {
                ed.edit(0..0, &format!("{text}{eol}"));
            } else {
                let at = ed.ex_line_range(t).end;
                ed.edit(at..at, &format!("{eol}{text}"));
            }
        };
        if !mv {
            insert_after(self, t);
            self.ex_land_on(t + n);
            return Ok(());
        }
        if t >= s && t < e {
            return Err(ExError::MoveIntoItself);
        }
        if t == e || t == s - 1 {
            return Ok(()); // already there
        }
        // A delete plus an insert, not one rewrite of the whole span: lines
        // in between keep their marks and `:g` anchors, and the moved lines
        // take their marks along.
        let block_start = self.ex_line_range(s).start;
        let block_end = self.ex_line_range(e).end;
        let moved = self.take_vim_positions(block_start..block_end + 1);
        self.begin_undo_group();
        let last = if t > e {
            insert_after(self, t);
            let span = self.ex_line_block_span(s, e);
            self.edit(span, "");
            t
        } else {
            let span = self.ex_line_block_span(s, e);
            self.edit(span, "");
            insert_after(self, t);
            t + n
        };
        self.end_undo_group();
        let first = self.ex_line_range(last - n + 1).start;
        self.restore_vim_positions(moved, first);
        self.ex_land_on(last);
        Ok(())
    }

    /// `:[range]j[!] [count]` — join lines (default: this and the next).
    fn ex_join(&mut self, line: &ExLine, _cx: &mut Context<Self>) -> Result<(), ExError> {
        let cur = self.ex_cur_line();
        let (mut s, mut e) = self.ex_resolve_range(line.range.as_ref(), (cur, cur))?;
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
        self.edit(span, &joined);
        self.ex_land_on(s);
        Ok(())
    }

    /// `:[range]>` / `:[range]<<` … — shift lines (one step per `>`).
    fn ex_shift(&mut self, line: &ExLine, cx: &mut Context<Self>) -> Result<(), ExError> {
        let cur = self.ex_cur_line();
        let (s, e) = self.ex_resolve_range(line.range.as_ref(), (cur, cur))?;
        let (s, e) = self.ex_apply_count(s, e, line.arg.trim().parse().ok());
        let dir: isize = if line.name.starts_with('>') { 1 } else { -1 };
        let step = self.structural_indent_width(cx).max(1);
        for _ in 0..line.name.len() {
            self.shift_line_range(line_index(s), line_index(e), dir, step);
        }
        self.ex_land_on(e);
        Ok(())
    }

    /// `:[range]sort[!] [i][n][u]` (default: every line).
    fn ex_sort(&mut self, line: &ExLine, _cx: &mut Context<Self>) -> Result<(), ExError> {
        let last = self.ex_last_line();
        let (s, e) = self.ex_resolve_range(line.range.as_ref(), (1, last))?;
        let sorted = sort_lines(&self.ex_lines(s, e), &line.arg, line.bang);
        let span = self.ex_text_span(s, e);
        let start = span.start;
        self.edit(span, &sorted.join("\n"));
        self.collapse_to(start);
        Ok(())
    }

    /// `:[line]pu[!] [x]` — put register x (default: unnamed) as lines below
    /// (`!`: above) the line.
    fn ex_put(&mut self, line: &ExLine, cx: &mut Context<Self>) -> Result<(), ExError> {
        let cur = self.ex_cur_line();
        let (_, l) = self.ex_resolve_range0(line.range.as_ref(), (cur, cur))?;
        let reg = line.arg.trim().chars().next().unwrap_or('"');
        let v = self
            .evil_register_value(reg, cx)
            .ok_or(ExError::EmptyRegister(reg))?;
        let text = v.text.strip_suffix('\n').unwrap_or(&v.text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        let eol = self.ex_eol();
        let text = if eol == "\n" {
            text.to_string()
        } else {
            text.replace("\r\n", "\n").replace('\n', eol)
        };
        let (at, ins, first) = if line.bang || l == 0 {
            let at = self.ex_line_range(l.max(1)).start;
            (at, format!("{text}{eol}"), l.max(1))
        } else {
            let at = self.ex_line_range(l).end;
            (at, format!("{eol}{text}"), l + 1)
        };
        self.edit(at..at, &ins);
        self.ex_land_on(first);
        Ok(())
    }

    /// `:s`, `:&`, `:&&`: substitute over the range (default: this line).
    fn ex_sub(&mut self, line: &ExLine, amp: bool, cx: &mut Context<Self>) -> Result<(), ExError> {
        let cur = self.ex_cur_line();
        let (s, e) = self.ex_resolve_range(line.range.as_ref(), (cur, cur))?;
        let mut spec = parse_sub(&line.arg);
        // `:&&` / `:s&` keep the last flags.
        let keep_flags = line.name == "&&" || spec.flags.starts_with('&');
        if amp || spec.bare {
            let last = self
                .vim
                .last_sub
                .clone()
                .ok_or(ExError::NoPreviousSubstitute)?;
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
            spec.pattern = self.search_query.clone().ok_or(ExError::NoPreviousRegex)?;
        }
        self.vim.last_sub = Some(LastSub {
            pattern: spec.pattern.clone(),
            replacement: spec.replacement.clone(),
            flags: spec.flags.replace('&', ""),
        });
        // `n` and the search highlight compile it as a plain regex: store the
        // converted pattern (`\c` / `\<` resolved).
        self.search_query = Some(vim_pattern(&spec.pattern, false));
        self.ex_substitute_range(s, e, &spec, cx)
    }

    /// The substitute engine over lines `s..=e`.
    pub(super) fn ex_substitute_range(
        &mut self,
        s: i64,
        e: i64,
        spec: &SubSpec,
        cx: &mut Context<Self>,
    ) -> Result<(), ExError> {
        let (s, e) = self.ex_apply_count(s, e, spec.count);
        let icase = spec.flags.contains('i') && !spec.flags.contains('I');
        let re = Regex::new(&vim_pattern(&spec.pattern, icase)).map_err(ExError::InvalidRegex)?;
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
                return Err(ExError::PatternNotFound(Some(spec.pattern.clone())));
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
        // have grown with `\n` replacements — clamped).
        self.ex_land_on(last_changed);
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
    ) -> Result<(), ExError> {
        let mut matches = std::collections::VecDeque::new();
        for l in s..=e {
            let base = self.ex_line_range(l).start;
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
            return Err(ExError::PatternNotFound(None));
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
        self.active.selected_range = c.shifted(a, b);
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
        let span = c.shifted(a, b);
        c.shift += rep.len() as isize - (b - a) as isize;
        c.done += 1;
        self.active.mark_active = false;
        let end = self.edit(span, &rep);
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
    ) -> Result<(), ExError> {
        let last = self.ex_last_line();
        let (s, e) = self.ex_resolve_range(line.range.as_ref(), (1, last))?;
        let arg = line.arg.trim_start();
        let delim = arg.chars().next().ok_or(ExError::InvalidCommand)?;
        let (pat, cmd) = take_delimited(&arg[delim.len_utf8()..], delim);
        let pat = if pat.is_empty() {
            self.search_query.clone().ok_or(ExError::NoPreviousRegex)?
        } else {
            pat
        };
        let re = Regex::new(&vim_pattern(&pat, false)).map_err(ExError::InvalidRegex)?;
        self.search_query = Some(vim_pattern(&pat, false));
        let cmd = if cmd.trim().is_empty() {
            "p".to_string()
        } else {
            cmd.trim().to_string()
        };
        let lines: Vec<i64> = (s..=e)
            .filter(|&l| re.is_match(&self.ex_line(l)) == matching)
            .collect();
        if lines.is_empty() {
            return Err(ExError::PatternNotFound(None));
        }
        let sub = parse_ex_line(&cmd);
        let sub_cmd = ExCmd::lookup(&sub.name);
        if sub_cmd == Some(ExCmd::Normal) {
            let tokens = normal_key_tokens(&sub.arg);
            let events = normal_events_on_lines(lines.iter().rev().copied(), &tokens);
            self.replay_events(events, true, cx);
            return Ok(());
        }
        if sub_cmd == Some(ExCmd::Print) {
            let rows: Vec<String> = lines
                .iter()
                .map(|&l| format!("{l:>6} {}", self.ex_line(l)))
                .collect();
            self.show_listing("*global*", &rows, cx);
            return Ok(());
        }
        // Anchors (line starts) that ride along with the edits; one whose
        // line was deleted is dropped.
        self.vim.ex_anchors = lines
            .iter()
            .map(|&l| Some(self.ex_line_range(l).start))
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
                    return Err(ExError::NotUnderGlobal(cmd));
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
    fn ex_normal(&mut self, line: &ExLine, cx: &mut Context<Self>) -> Result<(), ExError> {
        let tokens = normal_key_tokens(&line.arg);
        if tokens.is_empty() {
            return Err(ExError::ArgumentRequired);
        }
        let events = match &line.range {
            None => {
                let mut events: Vec<MacroEvent> = tokens.into_iter().map(MacroEvent::Key).collect();
                events.push(MacroEvent::Key("<Esc>".into()));
                events
            }
            Some(_) => {
                let cur = self.ex_cur_line();
                let (s, e) = self.ex_resolve_range(line.range.as_ref(), (cur, cur))?;
                normal_events_on_lines((s..=e).rev(), &tokens)
            }
        };
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
        assert_eq!(ExCmd::lookup("norma"), Some(ExCmd::Normal));
        assert_eq!(ExCmd::lookup("<<<"), Some(ExCmd::Shift));
        assert_eq!(ExCmd::lookup("bN"), Some(ExCmd::BufferPrev));
        assert_eq!(ExCmd::lookup("ma"), Some(ExCmd::Mark));
        assert_eq!(ExCmd::lookup("marks"), Some(ExCmd::Marks));
        assert_eq!(ExCmd::lookup("nope"), None);
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
