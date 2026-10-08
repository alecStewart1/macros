//! The unlocked `authinfo.gpg` secrets store behind scheme/secrets.scm.
//!
//! The file is netrc/authinfo lines (`machine H login U port P password S`),
//! encrypted symmetrically with one master passphrase — the user types it once
//! per session and every package asks this store (via `secret-get`) instead of
//! prompting. gpg does the crypto: `--pinentry-mode loopback --passphrase-fd 0`
//! takes the passphrase on stdin (then, for encryption, the plaintext after it),
//! so neither the passphrase nor a password ever reaches argv or a temp file.
//! `--no-symkey-cache` keeps gpg-agent from caching the passphrase on its own:
//! locking here really locks.
//!
//! Decrypted entries, the passphrase (needed to re-encrypt on save) and any
//! passwords typed but not saved this session live only in engine-thread memory; Scheme sees one password per lookup, never the
//! whole file. Lines that aren't entries (comments, blanks, `default …`) are
//! kept verbatim, so hand edits survive a save.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One authinfo line: a parsed entry, or anything else kept as-is.
#[derive(Clone, Debug, PartialEq)]
enum Line {
    Entry(Vec<(String, String)>),
    Raw(String),
}

impl Line {
    fn get(&self, key: &str) -> Option<&str> {
        match self {
            Line::Entry(fields) => fields
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str()),
            Line::Raw(_) => None,
        }
    }

    /// Does this entry serve (host, user, port)? An empty `user` / `port` in
    /// the query matches anything; an entry with no `port` matches any port.
    fn matches(&self, host: &str, user: &str, port: &str) -> bool {
        let Line::Entry(_) = self else { return false };
        if self.get("machine") != Some(host) || self.get("password").is_none() {
            return false;
        }
        let user_ok = user.is_empty() || self.get("login") == Some(user);
        let port_ok = port.is_empty() || self.get("port").is_none_or(|p| p == port);
        user_ok && port_ok
    }
}

/// Split a line into tokens; `"…"` quotes a token with spaces (`\"`, `\\` escape).
fn tokenize(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            let mut tok = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => tok.extend(chars.next()),
                    c => tok.push(c),
                }
            }
            tokens.push(tok);
        } else {
            let mut tok = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                tok.push(c);
                chars.next();
            }
            tokens.push(tok);
        }
    }
    tokens
}

fn quote(value: &str) -> String {
    let plain = !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\\');
    if plain {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn parse(text: &str) -> Vec<Line> {
    text.lines()
        .map(|line| {
            let tokens = tokenize(line);
            let is_entry = !line.trim_start().starts_with('#')
                && tokens.len() >= 2
                && tokens.len().is_multiple_of(2)
                && tokens[0] == "machine";
            if is_entry {
                Line::Entry(
                    tokens
                        .chunks(2)
                        .map(|kv| (kv[0].clone(), kv[1].clone()))
                        .collect(),
                )
            } else {
                Line::Raw(line.to_string())
            }
        })
        .collect()
}

fn serialize(lines: &[Line]) -> String {
    let mut out = String::new();
    for line in lines {
        match line {
            Line::Raw(raw) => out.push_str(raw),
            Line::Entry(fields) => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{k} {}", quote(v)))
                    .collect();
                out.push_str(&parts.join(" "));
            }
        }
        out.push('\n');
    }
    out
}

/// The gpg binary: `gpg_command` if given, else the first that exists of the
/// usual install locations (a GUI app doesn't inherit the shell PATH), else
/// plain `gpg` for PATH lookup.
fn resolve_gpg(gpg_command: &str) -> PathBuf {
    if !gpg_command.is_empty() {
        return crate::util::expand_tilde(gpg_command).into();
    }
    [
        "/opt/homebrew/bin/gpg",
        "/usr/local/bin/gpg",
        "/usr/bin/gpg",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
    .unwrap_or_else(|| PathBuf::from("gpg"))
}

/// Run gpg with the passphrase (then `data`) on stdin → stdout bytes, or an
/// error built from gpg's stderr (which never echoes the passphrase).
fn run_gpg(
    gpg: &Path,
    args: &[&str],
    passphrase: &str,
    data: &[u8],
    gnupg_home: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new(gpg);
    cmd.args([
        "--batch",
        "--quiet",
        "--yes",
        "--no-symkey-cache",
        "--pinentry-mode",
        "loopback",
        "--passphrase-fd",
        "0",
    ])
    .args(args)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    if let Some(home) = gnupg_home {
        cmd.env("GNUPGHOME", home);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("can't run {}: {e}", gpg.display()))?;
    let mut input = Vec::with_capacity(passphrase.len() + 1 + data.len());
    input.extend_from_slice(passphrase.as_bytes());
    input.push(b'\n');
    input.extend_from_slice(data);
    // On its own thread: gpg may fill its stdout pipe before draining stdin.
    let writer = child.stdin.take().map(|mut stdin| {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        })
    });
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(
        if stderr.contains("Bad session key") || stderr.contains("decryption failed") {
            "wrong passphrase".to_string()
        } else {
            stderr
                .lines()
                .rfind(|l| !l.trim().is_empty())
                .unwrap_or("gpg failed")
                .trim_start_matches("gpg: ")
                .to_string()
        },
    )
}

fn decrypt(
    gpg: &Path,
    path: &Path,
    passphrase: &str,
    home: Option<&Path>,
) -> Result<String, String> {
    let path = path.to_string_lossy();
    let bytes = run_gpg(gpg, &["--decrypt", &path], passphrase, &[], home)?;
    String::from_utf8(bytes).map_err(|_| "authinfo file is not UTF-8".to_string())
}

/// Encrypt `plaintext` to `path` atomically: gpg writes a sibling temp file
/// (mode 0600), which is then renamed over the target.
fn encrypt(
    gpg: &Path,
    path: &Path,
    passphrase: &str,
    plaintext: &str,
    home: Option<&Path>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("gpg.tmp");
    let tmp_str = tmp.to_string_lossy();
    let result = run_gpg(
        gpg,
        &[
            "--symmetric",
            "--cipher-algo",
            "AES256",
            "--output",
            &tmp_str,
        ],
        passphrase,
        plaintext.as_bytes(),
        home,
    )
    .and_then(|_| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

struct Store {
    path: PathBuf,
    gpg: PathBuf,
    passphrase: String,
    lines: Vec<Line>,
    /// GNUPGHOME override, for tests only.
    gnupg_home: Option<PathBuf>,
}

impl Store {
    fn write(&self) -> Result<(), String> {
        encrypt(
            &self.gpg,
            &self.path,
            &self.passphrase,
            &serialize(&self.lines),
            self.gnupg_home.as_deref(),
        )
    }
}

thread_local! {
    // Per engine thread (= per ScriptEngine), so parallel test engines never
    // share a store.
    static STORE: RefCell<Option<Store>> = const { RefCell::new(None) };
    // Passwords typed this session but NOT saved to the file ("Save? no"):
    // remembered in memory so the package doesn't re-prompt on every call,
    // gone at quit or `secrets-lock`. Usable without a store or passphrase.
    static SESSION: RefCell<Vec<Line>> = const { RefCell::new(Vec::new()) };
}

fn new_entry(host: &str, user: &str, port: &str, password: &str) -> Line {
    let mut fields = vec![("machine".to_string(), host.to_string())];
    if !user.is_empty() {
        fields.push(("login".into(), user.into()));
    }
    if !port.is_empty() {
        fields.push(("port".into(), port.into()));
    }
    fields.push(("password".into(), password.into()));
    Line::Entry(fields)
}

/// Replace the exact (host, user, port) entry in place, else append.
fn upsert(lines: &mut Vec<Line>, host: &str, user: &str, port: &str, password: &str) {
    let entry = new_entry(host, user, port, password);
    let exact = lines.iter().position(|l| {
        l.get("machine") == Some(host)
            && l.get("login").unwrap_or("") == user
            && l.get("port").unwrap_or("") == port
    });
    match exact {
        Some(i) => lines[i] = entry,
        None => lines.push(entry),
    }
}

fn find_password(lines: &[Line], host: &str, user: &str, port: &str) -> Option<String> {
    lines
        .iter()
        .find(|l| l.matches(host, user, port))
        .and_then(|l| l.get("password"))
        .map(str::to_string)
}

/// Unlock `path` with `passphrase`. A missing file starts an empty store
/// (created on the first save) protected by this passphrase.
fn unlock(
    path: &str,
    gpg_command: &str,
    passphrase: &str,
    gnupg_home: Option<PathBuf>,
) -> Result<(), String> {
    if passphrase.is_empty() || passphrase.contains('\n') {
        return Err("the passphrase must be one non-empty line".into());
    }
    let path = PathBuf::from(crate::util::expand_tilde(path));
    let gpg = resolve_gpg(gpg_command);
    let lines = if path.exists() {
        parse(&decrypt(&gpg, &path, passphrase, gnupg_home.as_deref())?)
    } else {
        Vec::new()
    };
    STORE.with_borrow_mut(|s| {
        *s = Some(Store {
            path,
            gpg,
            passphrase: passphrase.to_string(),
            lines,
            gnupg_home,
        })
    });
    Ok(())
}

/// The password for (host, user, port): the unlocked file first, then this
/// session's unsaved passwords. #f when locked and never typed.
pub(crate) fn lookup(host: &str, user: &str, port: &str) -> Option<String> {
    STORE
        .with_borrow(|s| {
            s.as_ref()
                .and_then(|store| find_password(&store.lines, host, user, port))
        })
        .or_else(|| SESSION.with_borrow(|lines| find_password(lines, host, user, port)))
}

/// Add or replace the entry for (host, user, port) and re-encrypt the file.
/// On a failed write the in-memory store is left as it was.
fn save(host: &str, user: &str, port: &str, password: &str) -> Result<(), String> {
    STORE.with_borrow_mut(|s| {
        let store = s.as_mut().ok_or("authinfo is locked")?;
        let before = store.lines.clone();
        upsert(&mut store.lines, host, user, port, password);
        store.write().inspect_err(|_| store.lines = before)
    })?;
    SESSION.with_borrow_mut(|lines| lines.retain(|l| !l.matches(host, user, port)));
    Ok(())
}

/// Remember an unsaved password for the rest of the session (memory only).
fn remember(host: &str, user: &str, port: &str, password: &str) {
    SESSION.with_borrow_mut(|lines| upsert(lines, host, user, port, password));
}

/// Delete matching entries — session memory, the unlocked store, and (if the
/// store had one) the file: the service rejected the password, so it's gone
/// and the next `secret-get` prompts. → Ok(true) if anything was dropped.
fn forget(host: &str, user: &str, port: &str) -> Result<bool, String> {
    let from_session = SESSION.with_borrow_mut(|lines| {
        let before = lines.len();
        lines.retain(|l| !l.matches(host, user, port));
        lines.len() != before
    });
    let from_store = STORE.with_borrow_mut(|s| {
        let Some(store) = s.as_mut() else {
            return Ok(false);
        };
        let before = store.lines.len();
        store.lines.retain(|l| !l.matches(host, user, port));
        if store.lines.len() == before {
            return Ok(false);
        }
        store.write().map(|()| true)
    })?;
    Ok(from_session || from_store)
}

fn lock() {
    STORE.with_borrow_mut(|s| *s = None);
    SESSION.with_borrow_mut(Vec::clear);
}

fn change_passphrase(new: &str) -> Result<(), String> {
    if new.is_empty() || new.contains('\n') {
        return Err("the passphrase must be one non-empty line".into());
    }
    STORE.with_borrow_mut(|s| {
        let store = s.as_mut().ok_or("authinfo is locked")?;
        let old = std::mem::replace(&mut store.passphrase, new.to_string());
        store.write().inspect_err(|_| store.passphrase = old)
    })
}

/// The gpg home for Scheme-driven unlocks: gpg's own default, except under
/// `cfg(test)`, where a fixed private dir keeps tests off the developer's real
/// ~/.gnupg and immune to tests that repoint `$HOME` process-wide (gpg derives
/// its home from it). Short and fixed: gpg-agent's socket path must fit in
/// sun_path, and repeated runs then reuse one agent instead of leaking one each.
fn primitive_gnupg_home() -> Option<PathBuf> {
    #[cfg(test)]
    {
        let home = PathBuf::from("/tmp/macros-test-gnupg");
        let _ = std::fs::create_dir_all(&home);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700));
        }
        Some(home)
    }
    #[cfg(not(test))]
    None
}

/// The reference inside a Steel value, if it is one.
pub(crate) fn secret_ref_of(v: &steel::rvals::SteelVal) -> Option<SecretRef> {
    match v {
        steel::rvals::SteelVal::Custom(c) => {
            use steel::gc::ShareableMut as _;
            c.read().as_any_ref().downcast_ref::<SecretRef>().cloned()
        }
        _ => None,
    }
}

/// A reference to a store entry, standing in for its value until the store
/// can answer: `(secret-get host user)` with no callback returns one, so
/// config can write `(set-option "psql-url" (secret-get "psql-url-1" "kyle"))`.
/// The option stores the reference (never the secret), and `get-option`
/// resolves it on each read via [`lookup`]. It's an opaque Steel value, so it
/// can't be confused with a string; it prints as `#<secret user@host>`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SecretRef {
    pub host: String,
    pub user: String,
    pub port: String,
}

impl steel::rvals::Custom for SecretRef {
    fn fmt(&self) -> Option<std::result::Result<String, std::fmt::Error>> {
        Some(Ok(format!("#<secret {}>", self.label())))
    }
}

impl SecretRef {
    /// `user@host:port`, omitting empty parts (no secret in it).
    pub fn label(&self) -> String {
        let mut out = String::new();
        if !self.user.is_empty() {
            out.push_str(&self.user);
            out.push('@');
        }
        out.push_str(&self.host);
        if !self.port.is_empty() {
            out.push(':');
            out.push_str(&self.port);
        }
        out
    }

    /// The secret, if the store (or this session) has it right now.
    pub fn resolve(&self) -> Option<String> {
        lookup(&self.host, &self.user, &self.port)
    }
}

/// Register the `authinfo-*` primitives (the Rust half of scheme/secrets.scm).
/// Fallible ones return "" on success or an error message — never containing
/// the passphrase or a password.
pub(crate) fn register(reg: &mut crate::script::Registrar) {
    // `(authinfo-unlock path gpg-command passphrase)` → "" | error. gpg-command
    // "" = auto-detect.
    reg.register_fn(
        "authinfo-unlock",
        |path: String, gpg: String, passphrase: String| -> String {
            unlock(&path, &gpg, &passphrase, primitive_gnupg_home())
                .err()
                .unwrap_or_default()
        },
    );
    // `(authinfo-secret-ref host user port)` → an opaque reference to that
    // entry (what `secret-get` returns when called without a callback).
    reg.register_fn(
        "authinfo-secret-ref",
        |host: String, user: String, port: String| -> SecretRef { SecretRef { host, user, port } },
    );
    // `(secret-ref? v)` → #t for a reference.
    reg.register_fn("secret-ref?", |v: steel::rvals::SteelVal| -> bool {
        secret_ref_of(&v).is_some()
    });
    reg.register_fn("authinfo-unlocked?", || -> bool {
        STORE.with_borrow(|s| s.is_some())
    });
    // Forget everything: the unlocked file and this session's unsaved passwords.
    reg.register_fn("authinfo-lock", lock);
    // `(authinfo-lookup host user port)` → password, or #f. "" user/port = any.
    reg.register_fn(
        "authinfo-lookup",
        |host: String, user: String, port: String| -> Option<String> {
            lookup(&host, &user, &port)
        },
    );
    reg.register_fn(
        "authinfo-save",
        |host: String, user: String, port: String, password: String| -> String {
            save(&host, &user, &port, &password)
                .err()
                .unwrap_or_default()
        },
    );
    // `(authinfo-remember host user port password)` — keep an unsaved password
    // in memory for the session.
    reg.register_fn(
        "authinfo-remember",
        |host: String, user: String, port: String, password: String| {
            remember(&host, &user, &port, &password)
        },
    );
    // `(authinfo-forget host user port)` → "" | error; deletes from the file too.
    reg.register_fn(
        "authinfo-forget",
        |host: String, user: String, port: String| -> String {
            forget(&host, &user, &port).err().unwrap_or_default()
        },
    );
    reg.register_fn("authinfo-change-passphrase", |new: String| -> String {
        change_passphrase(&new).err().unwrap_or_default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_serialize_round_trip_keeps_raw_lines() {
        let text = "# my secrets\n\
                    machine db.example.com login kyle port 5432 password \"p w\\\"x\"\n\
                    \n\
                    default login anon password guest\n";
        let lines = parse(text);
        assert_eq!(lines[0], Line::Raw("# my secrets".into()));
        assert_eq!(lines[1].get("password"), Some("p w\"x"));
        assert_eq!(
            lines[3],
            Line::Raw("default login anon password guest".into())
        );
        assert_eq!(parse(&serialize(&lines)), lines);
    }

    #[test]
    fn matching_rules() {
        let lines = parse(
            "machine h login a port 1 password one\n\
             machine h login b password two\n",
        );
        let find = |u: &str, p: &str| {
            lines
                .iter()
                .find(|l| l.matches("h", u, p))
                .and_then(|l| l.get("password"))
        };
        assert_eq!(find("a", "1"), Some("one"));
        assert_eq!(find("a", "2"), None, "port mismatch");
        assert_eq!(
            find("b", "9"),
            Some("two"),
            "portless entry matches any port"
        );
        assert_eq!(find("", ""), Some("one"), "empty user/port match the first");
        assert!(!lines[0].matches("other", "a", "1"));
    }

    /// Full cycle through real gpg: create, save, re-unlock, wrong passphrase,
    /// forget, change passphrase. Skipped where gpg isn't installed.
    #[test]
    fn gpg_store_cycle() {
        let gpg = resolve_gpg("");
        if Command::new(&gpg).arg("--version").output().is_err() {
            eprintln!("gpg not installed; skipping");
            return;
        }
        // Short path: gpg-agent's socket path must fit in sun_path.
        let dir = PathBuf::from(format!("/tmp/mai-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("g");
        std::fs::create_dir_all(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let file = dir.join("authinfo.gpg");
        let path = file.to_string_lossy().to_string();
        let h = || Some(home.clone());

        unlock(&path, "", "master", h()).unwrap();
        assert!(!file.exists(), "nothing written before the first save");
        assert_eq!(lookup("db", "kyle", ""), None);
        save("db", "kyle", "5432", "s3cret pw").unwrap();
        save("mail", "", "", "m").unwrap();
        save("db", "kyle", "5432", "newer").unwrap();

        STORE.with_borrow_mut(|s| *s = None);
        assert_eq!(
            unlock(&path, "", "nope", h()),
            Err("wrong passphrase".into())
        );
        unlock(&path, "", "master", h()).unwrap();
        assert_eq!(lookup("db", "kyle", "5432").as_deref(), Some("newer"));
        assert_eq!(lookup("mail", "", "").as_deref(), Some("m"));

        assert_eq!(forget("db", "kyle", "5432"), Ok(true));
        assert_eq!(lookup("db", "kyle", "5432"), None);
        STORE.with_borrow_mut(|s| *s = None);
        unlock(&path, "", "master", h()).unwrap();
        assert_eq!(
            lookup("db", "kyle", "5432"),
            None,
            "deleted from the file too"
        );
        assert_eq!(forget("db", "kyle", "5432"), Ok(false));

        change_passphrase("master2").unwrap();
        STORE.with_borrow_mut(|s| *s = None);
        assert!(unlock(&path, "", "master", h()).is_err());
        unlock(&path, "", "master2", h()).unwrap();
        assert_eq!(lookup("mail", "", "").as_deref(), Some("m"));

        STORE.with_borrow_mut(|s| *s = None);
        let _ = Command::new("gpgconf")
            .args(["--kill", "gpg-agent"])
            .env("GNUPGHOME", &home)
            .status();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
