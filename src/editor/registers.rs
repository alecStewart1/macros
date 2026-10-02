//! Vim registers for evil mode. The unnamed register stays the kill ring top
//! (mirrored to the system clipboard, as before); this adds the rest of Vim's
//! set on the side: named `a`–`z` (`A`–`Z` append), `0` (last yank), `1`–`9`
//! (delete history), `-` (small delete), and the read-only `.` `:` `/`.
//! `_` (black hole) and `+`/`*` (system clipboard) are handled where the
//! kill/paste happens, since they never store anything here.

use std::collections::HashMap;

/// A register's contents and whether it pastes line-wise.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct RegValue {
    pub text: String,
    pub linewise: bool,
}

/// What filled a register: Vim routes yanks to `0` and deletes to `1`/`-`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KillKind {
    Yank,
    Delete,
}

#[derive(Default)]
pub(super) struct Registers {
    regs: HashMap<char, RegValue>,
    /// The registers the latest kill wrote — a following
    /// `evil-set-linewise` marks them (evil queues it after the kill), and a
    /// counted repeat (`3dd`) appends to them.
    last_written: Vec<char>,
}

/// Whether `c` names a register a yank/delete may target.
pub(super) fn is_writable(c: char) -> bool {
    c.is_ascii_alphabetic() || matches!(c, '"' | '_' | '+' | '*' | '-')
}

/// Whether `c` names any register `"x` accepts.
pub(super) fn is_register(c: char) -> bool {
    is_writable(c) || c.is_ascii_digit() || matches!(c, '.' | ':' | '/' | '%' | '#')
}

impl Registers {
    pub fn get(&self, r: char) -> Option<&RegValue> {
        self.regs.get(&r.to_ascii_lowercase())
    }

    pub fn set(&mut self, r: char, text: &str, linewise: bool) {
        self.regs.insert(
            r,
            RegValue {
                text: text.to_string(),
                linewise,
            },
        );
    }

    /// Shift the numbered delete history and put `v` in `1`.
    fn push_numbered(&mut self, v: RegValue) {
        for i in (1..9).rev() {
            let from = char::from(b'0' + i);
            let to = char::from(b'0' + i + 1);
            if let Some(old) = self.regs.remove(&from) {
                self.regs.insert(to, old);
            }
        }
        self.regs.insert('1', v);
    }

    /// Record a yank / delete of `text` into `target` (a named register, or
    /// `None` for the unnamed one), following Vim's routing: a yank fills `0`;
    /// a delete of a line or more (or into a named register) shifts the
    /// `1`–`9` history; a smaller unnamed delete fills `-`. An uppercase name
    /// appends to its lowercase register.
    pub fn record(&mut self, kind: KillKind, target: Option<char>, text: &str) {
        self.last_written.clear();
        let value = RegValue {
            text: text.to_string(),
            linewise: false,
        };
        if let Some(r) = target.filter(|c| c.is_ascii_alphabetic()) {
            let lower = r.to_ascii_lowercase();
            if r.is_ascii_uppercase()
                && let Some(old) = self.regs.get_mut(&lower)
            {
                old.text.push_str(text);
            } else {
                self.regs.insert(lower, value.clone());
            }
            self.last_written.push(lower);
        }
        let named = target.is_some_and(|c| c.is_ascii_alphabetic());
        match kind {
            KillKind::Yank => {
                if !named {
                    self.regs.insert('0', value);
                    self.last_written.push('0');
                }
            }
            KillKind::Delete => {
                if named || text.contains('\n') {
                    self.push_numbered(value);
                    self.last_written.push('1');
                } else {
                    self.regs.insert('-', value);
                    self.last_written.push('-');
                }
            }
        }
    }

    /// A counted repeat's later runs (`3dd`): extend what the first run wrote.
    pub fn append_last(&mut self, text: &str) {
        for r in self.last_written.clone() {
            if let Some(v) = self.regs.get_mut(&r) {
                v.text.push_str(text);
            }
        }
    }

    /// `evil-set-linewise` / `-charwise` after a kill: mark what it wrote. A
    /// line-wise delete that went to `-` (the buffer's last line, which has no
    /// newline) belongs in the numbered history instead.
    pub fn set_linewise_last(&mut self, linewise: bool) {
        if linewise && self.last_written == ['-'] {
            if let Some(v) = self.regs.remove(&'-') {
                self.push_numbered(v);
            }
            self.last_written = vec!['1'];
        }
        for r in &self.last_written {
            if let Some(v) = self.regs.get_mut(r) {
                v.linewise = linewise;
            }
        }
    }

    /// Every stored register in `:registers` order.
    pub fn listing(&self) -> Vec<(char, RegValue)> {
        let order = "0123456789abcdefghijklmnopqrstuvwxyz-.:/";
        order
            .chars()
            .filter_map(|c| self.regs.get(&c).map(|v| (c, v.clone())))
            .collect()
    }
}

/// One `:registers` display line: the text with control chars made visible
/// and long values clipped.
pub(super) fn display_value(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().take(120) {
        match c {
            '\n' => out.push_str("^J"),
            '\t' => out.push_str("^I"),
            c if c.is_control() => out.push('?'),
            c => out.push(c),
        }
    }
    if text.chars().count() > 120 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vim_register_routing() {
        let mut r = Registers::default();
        // yank → "0; small delete → "-; line delete → "1 (shifting).
        r.record(KillKind::Yank, None, "yanked");
        assert_eq!(r.get('0').unwrap().text, "yanked");
        r.record(KillKind::Delete, None, "w");
        assert_eq!(r.get('-').unwrap().text, "w");
        r.record(KillKind::Delete, None, "line one\n");
        r.set_linewise_last(true);
        r.record(KillKind::Delete, None, "line two\n");
        assert_eq!(r.get('1').unwrap().text, "line two\n");
        assert_eq!(r.get('2').unwrap().text, "line one\n");
        assert!(r.get('2').unwrap().linewise);
        assert_eq!(
            r.get('0').unwrap().text,
            "yanked",
            "deletes leave \"0 alone"
        );
        // named + append; named yank leaves "0 alone.
        r.record(KillKind::Yank, Some('a'), "foo");
        r.record(KillKind::Yank, Some('A'), "bar");
        assert_eq!(r.get('a').unwrap().text, "foobar");
        assert_eq!(r.get('0').unwrap().text, "yanked");
        // A linewise delete that landed in "- moves to the history.
        r.record(KillKind::Delete, None, "last");
        r.set_linewise_last(true);
        assert_eq!(r.get('1').unwrap().text, "last");
        assert!(r.get('1').unwrap().linewise);
        // Counted repeats append to what the first run wrote.
        r.record(KillKind::Delete, None, "a\n");
        r.append_last("b\n");
        assert_eq!(r.get('1').unwrap().text, "a\nb\n");
        assert_eq!(display_value("x\ny"), "x^Jy");
        assert!(is_register('a') && is_register('+') && is_register(':'));
        assert!(!is_register('!'));
    }
}
