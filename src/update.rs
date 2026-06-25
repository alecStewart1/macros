//! Self-update.
//!
//! Mirrors the sigma updater, minus the license/JWT layer. macros has no
//! backend database: the "latest version" lives in a static `latest.json`
//! manifest the web deploy bakes from `Cargo.toml` (see
//! `web/scripts/gen-version.mjs`), served at `https://macros.dev/latest.json`.
//! Release artifacts and their `*.sha256` sidecars live on R2 behind
//! `https://downloads.macros.dev` (see `scripts/publish-release.mjs`).
//!
//! Flow:
//!   1. GET the manifest, parse `{ version, notes, downloads }`.
//!   2. semver-compare `version` against this build's `CARGO_PKG_VERSION`.
//!   3. If newer:
//!      - macOS: download the platform `.zip`, verify its SHA-256 against the
//!        `<url>.sha256` sidecar, then atomically swap the running `.app`
//!        bundle in place ([`install`]). The caller relaunches + quits.
//!      - Windows/Linux: open the downloads page in the browser — packaged
//!        installers (`.msi`/`.deb`/`.rpm`) aren't swapped from under a running
//!        process here.
//!
//! macOS also gets a launch-time check ([`needs_relocation`]) for a bundle
//! running off a read-only volume (a mounted DMG, or Gatekeeper's App
//! Translocation mount for a quarantined download); [`relocate`] copies it into
//! `/Applications` so future auto-updates can install.
//!
//! Every HTTP call is blocking (`ureq`); run them on gpui's background executor,
//! never the main thread.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

/// The version compiled into this build (`Cargo.toml` `[package].version`).
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Where the latest-version manifest lives. Overridable via
/// `MACROS_UPDATE_MANIFEST` to point at a local `next dev` server during
/// development.
fn manifest_url() -> String {
    std::env::var("MACROS_UPDATE_MANIFEST")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://macros.dev/latest.json".to_string())
}

/// Stable per-`(os, arch)` key. Doubles as the manifest `downloads` key and the
/// artifact-name infix (`Macros-<key>-<version>.zip`).
pub fn platform_key() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "macos-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "macos-x86_64"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "windows-x86_64"
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        "windows-aarch64"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "linux-aarch64"
    }
    #[cfg(not(any(
        all(
            target_os = "macos",
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        all(
            target_os = "windows",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
    )))]
    {
        "unknown"
    }
}

/// The static manifest served at [`manifest_url`].
#[derive(Deserialize)]
struct Manifest {
    /// Latest released semver, e.g. `"0.2.0"`.
    version: String,
    /// Per-platform download URLs, keyed by [`platform_key`]-style strings.
    #[serde(default)]
    downloads: std::collections::HashMap<String, String>,
}

/// A newer release than this build, resolved from the manifest.
#[derive(Clone)]
pub struct UpdateAvailable {
    /// The latest version string from the manifest.
    pub version: String,
    /// Download URL for *this* platform, or `None` if the manifest lists no
    /// artifact for [`platform_key`]. Only read on macOS (in-place install);
    /// elsewhere `install` opens the downloads page instead.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub url: Option<String>,
}

/// What [`install`] did, so the caller knows whether to relaunch.
pub enum InstallOutcome {
    /// macOS: the new bundle is swapped in place; the caller should
    /// [`relaunch`] and quit. Only constructed on macOS.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Staged,
    /// Windows/Linux: the downloads page was opened in the browser; nothing to
    /// relaunch. Never constructed on macOS (which stages in place).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    OpenedDownloadPage,
}

/// Fetch the manifest and return a newer release if one exists. `Ok(None)` when
/// already current. Blocking — run off-thread.
pub fn check() -> Result<Option<UpdateAvailable>, String> {
    let body = http_get_string(&manifest_url())?;
    let manifest: Manifest =
        serde_json::from_str(&body).map_err(|e| format!("malformed update manifest: {e}"))?;

    let latest = semver::Version::parse(manifest.version.trim())
        .map_err(|e| format!("manifest version '{}' is not semver: {e}", manifest.version))?;
    let current = semver::Version::parse(current_version())
        .map_err(|e| format!("build version '{}' is not semver: {e}", current_version()))?;

    if latest <= current {
        return Ok(None);
    }

    Ok(Some(UpdateAvailable {
        url: manifest.downloads.get(platform_key()).cloned(),
        version: manifest.version,
    }))
}

/// Like [`check`], but skips the network entirely unless at least 24h have
/// passed since the last check (recorded in the cache dir). Used for the
/// launch-time auto-check so every start doesn't hit the network. The manual
/// `check-for-updates` command calls [`check`] directly to force a check.
pub fn check_gated() -> Result<Option<UpdateAvailable>, String> {
    if !due_for_check() {
        return Ok(None);
    }
    // Record the attempt up front so a transient network error doesn't cause a
    // check on every subsequent launch.
    record_check_now();
    check()
}

/// Download + install the update for this platform.
///
/// macOS: download the artifact, verify its SHA-256 against the `<url>.sha256`
/// sidecar, swap the `.app` bundle in place, and return [`InstallOutcome::Staged`].
/// Windows/Linux: open the downloads page and return
/// [`InstallOutcome::OpenedDownloadPage`].
///
/// `on_progress` is called with the download's percent complete (0–100) as bytes
/// arrive, so the caller can drive a progress indicator. It only fires during the
/// macOS download; other platforms open the browser and never call it.
///
/// Blocking — run off-thread.
#[cfg(target_os = "macos")]
pub fn install(
    update: &UpdateAvailable,
    on_progress: impl FnMut(u8),
) -> Result<InstallOutcome, String> {
    let url = update
        .url
        .as_deref()
        .ok_or("the update manifest lists no macOS download")?;

    let dir = macos::cache_dir()?;
    let dest = dir.join(format!("Macros-{}-{}.zip", platform_key(), update.version));

    let expected = fetch_sha256(url)?;
    download_verified(url, &expected, &dest, on_progress)?;
    macos::stage_in_place(&dest)?;
    Ok(InstallOutcome::Staged)
}

#[cfg(not(target_os = "macos"))]
pub fn install(
    _update: &UpdateAvailable,
    _on_progress: impl FnMut(u8),
) -> Result<InstallOutcome, String> {
    // No in-place swap for packaged installers (.msi/.deb/.rpm) while running —
    // hand the user the downloads page to run the installer themselves.
    open_url("https://macros.dev/download/")?;
    Ok(InstallOutcome::OpenedDownloadPage)
}

/// After [`InstallOutcome::Staged`], spawn the detached helper that re-opens the
/// app once this process exits. The caller quits immediately afterward.
#[cfg(target_os = "macos")]
pub fn relaunch() -> Result<(), String> {
    macos::relaunch()
}

#[cfg(not(target_os = "macos"))]
pub fn relaunch() -> Result<(), String> {
    Ok(())
}

/// True when the running bundle sits somewhere that blocks in-place self-update
/// and we should offer to reinstall into the system app dir. macOS: a read-only
/// volume (mounted DMG / App Translocation). Always false elsewhere, and false
/// for non-bundled dev builds (`cargo run`).
pub fn needs_relocation() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::needs_move_to_applications()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Copy the running bundle into `/Applications`, strip quarantine, and spawn a
/// helper that relaunches from there once this process exits. The caller quits
/// on success. macOS only.
pub fn relocate() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        macos::move_to_applications()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("relocation is only supported on macOS".to_string())
    }
}

// ── HTTP ─────────────────────────────────────────────────────────────────────

/// A blocking HTTP agent with sane timeouts and a descriptive User-Agent.
fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .user_agent(&format!(
            "Macros/{} ({})",
            current_version(),
            std::env::consts::OS
        ))
        .build()
}

/// GET `url` and return the body as a string (for the small JSON manifest /
/// sidecar). `ureq` caps `into_string` well above either payload.
fn http_get_string(url: &str) -> Result<String, String> {
    agent()
        .get(url)
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?
        .into_string()
        .map_err(|e| format!("reading {url}: {e}"))
}

/// Fetch and parse the `<url>.sha256` sidecar. Accepts either a bare 64-hex
/// digest or `sha256:<hex>` / `<hex>  <filename>` (sha256sum) formats — the
/// first 64-hex token wins. Only the macOS in-place install path calls this.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn fetch_sha256(url: &str) -> Result<String, String> {
    let body = http_get_string(&format!("{url}.sha256"))?;
    let token = body
        .split_whitespace()
        .map(|t| t.strip_prefix("sha256:").unwrap_or(t))
        .find(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| format!("no sha256 digest in {url}.sha256"))?;
    Ok(token.to_lowercase())
}

/// Hard cap on a downloaded artifact so a poisoned CDN can't fill the disk
/// before the checksum mismatch fires. Well above any real macros artifact.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MAX_DOWNLOAD_BYTES: u64 = 500 * 1024 * 1024;

/// Stream `url` to `dest`, hashing as we go. Writes to `<dest>.partial` and only
/// renames to `dest` once the SHA-256 matches `expected_sha256_hex`. Refuses
/// non-`https` URLs (except `http` in debug builds, for local dev servers).
/// Only the macOS in-place install path calls this.
///
/// `on_progress` receives the percent complete (0–100) each time it advances by a
/// whole point, computed from the `Content-Length`; if the server sends no length
/// it is never called (the caller leaves its indicator indeterminate).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn download_verified(
    url: &str,
    expected_sha256_hex: &str,
    dest: &Path,
    mut on_progress: impl FnMut(u8),
) -> Result<(), String> {
    use sha2::{Digest, Sha256};

    let lc = url.to_ascii_lowercase();
    let scheme_ok =
        lc.starts_with("https://") || (cfg!(debug_assertions) && lc.starts_with("http://"));
    if !scheme_ok {
        return Err(format!("refusing to download from non-https url: {url}"));
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("creating cache dir: {e}"))?;
    }
    let partial = dest.with_extension("partial");
    let _ = std::fs::remove_file(&partial);

    let resp = agent()
        .get(url)
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;

    let total = resp
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(len) = total
        && len > MAX_DOWNLOAD_BYTES
    {
        return Err(format!(
            "update artifact is {len} bytes, over the {MAX_DOWNLOAD_BYTES}-byte cap"
        ));
    }

    let mut reader = resp.into_reader();
    let mut file =
        std::fs::File::create(&partial).map_err(|e| format!("creating {partial:?}: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut written: u64 = 0;
    let mut last_pct: u8 = 0;

    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("reading {url}: {e}"))?;
        if n == 0 {
            break;
        }
        written = written.saturating_add(n as u64);
        if written > MAX_DOWNLOAD_BYTES {
            let _ = std::fs::remove_file(&partial);
            return Err(format!(
                "update artifact exceeded the {MAX_DOWNLOAD_BYTES}-byte cap"
            ));
        }
        // Report progress only when the whole-percent figure advances, so the
        // callback (and any channel behind it) isn't spammed every 64 KiB.
        if let Some(total) = total.filter(|t| *t > 0) {
            let pct = ((written.min(total) * 100) / total) as u8;
            if pct != last_pct {
                last_pct = pct;
                on_progress(pct);
            }
        }
        hasher.update(&buf[..n]);
        use std::io::Write;
        file.write_all(&buf[..n])
            .map_err(|e| format!("writing {partial:?}: {e}"))?;
    }
    file.sync_all()
        .map_err(|e| format!("flushing {partial:?}: {e}"))?;
    drop(file);

    let actual = hex::encode(hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256_hex) {
        let _ = std::fs::remove_file(&partial);
        return Err(format!(
            "checksum mismatch: expected {expected_sha256_hex}, got {actual}"
        ));
    }
    std::fs::rename(&partial, dest).map_err(|e| format!("finalizing download: {e}"))?;
    Ok(())
}

/// Open `url` in the user's default browser. Used by the non-macOS `install`
/// path and by the update pill's "download" click on those platforms (macOS
/// downloads in place instead, so it never calls this).
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn open_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("/usr/bin/open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("opening {url}: {e}"))
}

// ── Once-per-day gate ────────────────────────────────────────────────────────

/// Stamp file recording the last auto-check time (unix seconds).
fn last_check_path() -> Option<std::path::PathBuf> {
    Some(dirs::cache_dir()?.join("macros").join("last-update-check"))
}

/// Minimum interval between launch-time auto-checks.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

pub fn due_for_check() -> bool {
    let Some(path) = last_check_path() else {
        return true;
    };
    let Ok(meta) = std::fs::metadata(&path) else {
        return true; // never checked
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    modified
        .elapsed()
        .map_or(true, |elapsed| elapsed >= CHECK_INTERVAL) // clock moved backwards → just check
}

fn record_check_now() {
    if let Some(path) = last_check_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Touch: contents don't matter, only the mtime.
        let _ = std::fs::write(&path, current_version().as_bytes());
    }
}

// ── macOS platform ───────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod macos {
    //! Real in-place self-update via `.app` bundle swap. macOS's BSD layer keeps
    //! the running process's inodes alive after the bundle is renamed, so the
    //! swap-while-running technique is safe (Sparkle and friends do the same).
    //! Ported from sigma's `crates/licensing/src/platform/macos.rs`.

    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Path to the running `Macros.app` bundle. Errors for non-bundled dev
    /// builds (`cargo run`), which therefore never trigger update/relocation.
    pub fn current_app_path() -> Result<PathBuf, String> {
        let mut p = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
        // current_exe is .../Macros.app/Contents/MacOS/macros — walk up to `.app`.
        while p.extension().and_then(|s| s.to_str()) != Some("app") {
            if !p.pop() {
                return Err("not running from a .app bundle".to_string());
            }
        }
        Ok(p)
    }

    /// Per-user cache dir for staged downloads (`~/Library/Caches/macros/updates`).
    pub fn cache_dir() -> Result<PathBuf, String> {
        let d = dirs::cache_dir()
            .ok_or("no cache dir")?
            .join("macros")
            .join("updates");
        std::fs::create_dir_all(&d).map_err(|e| format!("creating {d:?}: {e}"))?;
        Ok(d)
    }

    /// Extract the verified `downloaded` zip and atomically swap its `Macros.app`
    /// into the running bundle's location.
    pub fn stage_in_place(downloaded: &Path) -> Result<(), String> {
        let app_path = current_app_path()?;
        // A bundle on a read-only volume (DMG / App Translocation mount) can't be
        // swapped in place — fail with an actionable message up front.
        if volume_is_read_only(&app_path) {
            return Err("Macros is running from a read-only location — move it to \
                 /Applications first, then update."
                .to_string());
        }
        let staging = cache_dir()?.join(format!("staging-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(|e| format!("creating staging dir: {e}"))?;

        // 1. Extract.
        let status = Command::new("/usr/bin/unzip")
            .args(["-q", "-o"])
            .arg(downloaded)
            .arg("-d")
            .arg(&staging)
            .status()
            .map_err(|e| format!("unzip: {e}"))?;
        if !status.success() {
            return Err("failed to extract the update archive".to_string());
        }
        let new_app = staging.join("Macros.app");
        if !new_app.exists() {
            return Err("update archive did not contain Macros.app".to_string());
        }
        // Defense-in-depth against zip-slip: the extracted bundle must
        // canonicalize to a path strictly under the staging directory.
        let staging_canon =
            std::fs::canonicalize(&staging).map_err(|e| format!("canonicalize staging: {e}"))?;
        let new_app_canon =
            std::fs::canonicalize(&new_app).map_err(|e| format!("canonicalize new app: {e}"))?;
        if !new_app_canon.starts_with(&staging_canon) {
            return Err("update archive escaped the staging directory".to_string());
        }

        // 2. Strip quarantine BEFORE swap so an early relaunch from Finder
        //    doesn't trip Gatekeeper.
        let _ = Command::new("/usr/bin/xattr")
            .args(["-dr", "com.apple.quarantine"])
            .arg(&new_app)
            .status();

        // 3. Swap the bundle in place. Destroying the old inode after the swap
        //    lets a pinned Dock alias re-bind to the new bundle at the same path
        //    instead of chasing the old inode (the "duplicate Dock icon" bug).
        let needs_admin = app_path.starts_with("/Applications") && !writable_by_me(&app_path);

        if needs_admin {
            // `mv old → bak; mv new → old; rm -rf bak`, quoted by AppleScript's
            // `quoted form of` (shell-injection-safe for any path).
            let bak = app_path.with_extension("app.bak");
            let bak_lit = applescript_string_literal(&bak.to_string_lossy());
            let app_lit = applescript_string_literal(&app_path.to_string_lossy());
            let new_lit = applescript_string_literal(&new_app.to_string_lossy());
            let script = format!(
                "set b to quoted form of {bak_lit}\n\
                 set a to quoted form of {app_lit}\n\
                 set n to quoted form of {new_lit}\n\
                 do shell script \"rm -rf \" & b & \" && mv \" & a & \" \" & b & \" && mv \" & n & \" \" & a & \" && rm -rf \" & b with administrator privileges"
            );
            let status = Command::new("osascript")
                .arg("-e")
                .arg(script)
                .status()
                .map_err(|e| format!("osascript: {e}"))?;
            if !status.success() {
                return Err("failed to install the update (admin step)".to_string());
            }
        } else {
            match renamex_swap(&new_app, &app_path) {
                Ok(()) => {
                    let _ = std::fs::remove_dir_all(&staging);
                }
                Err(_) => {
                    // Fallback if `renamex_np` is unavailable: move the old bundle
                    // aside *into staging* (not a sibling `.app.bak` the Dock alias
                    // would chase), move the new one in, then delete the old.
                    let aside = staging.join("old-Macros.app");
                    let _ = std::fs::remove_dir_all(&aside);
                    std::fs::rename(&app_path, &aside)
                        .map_err(|e| format!("moving old bundle aside: {e}"))?;
                    if let Err(e) = std::fs::rename(&new_app, &app_path) {
                        let _ = std::fs::rename(&aside, &app_path); // roll back
                        return Err(format!("swapping in the new bundle: {e}"));
                    }
                    let _ = std::fs::remove_dir_all(&staging);
                }
            }
        }
        Ok(())
    }

    /// Spawn the detached re-open helper. The swap already happened in
    /// `stage_in_place`, so this just waits for our pid to exit then re-opens.
    pub fn relaunch() -> Result<(), String> {
        let app_path = current_app_path()?;
        spawn_reopen_helper(&app_path)
    }

    /// True if the running bundle is on a read-only volume.
    pub fn needs_move_to_applications() -> bool {
        current_app_path().is_ok_and(|p| volume_is_read_only(&p))
    }

    /// Copy the running bundle into `/Applications` (replacing any existing
    /// copy), strip quarantine, and spawn a helper to relaunch from there.
    pub fn move_to_applications() -> Result<(), String> {
        let src = current_app_path()?;
        let file_name = src.file_name().ok_or("bundle has no file name")?.to_owned();
        let dest = Path::new("/Applications").join(&file_name);

        // Stage a copy first: `src` may be a translocation view *backed by*
        // `dest`, so nothing at the destination may be touched until the running
        // bundle has been fully copied out.
        let staging = cache_dir()?.join(format!("move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(|e| format!("creating staging dir: {e}"))?;
        let staged = staging.join(&file_name);

        // `ditto` preserves the code signature, resource forks, and permissions.
        let status = Command::new("/usr/bin/ditto")
            .arg(&src)
            .arg(&staged)
            .status()
            .map_err(|e| format!("ditto: {e}"))?;
        if !status.success() {
            return Err("failed to copy the app bundle".to_string());
        }
        let _ = Command::new("/usr/bin/xattr")
            .args(["-dr", "com.apple.quarantine"])
            .arg(&staged)
            .status();

        // Install: replace whatever sits at `dest`. Try as the current user
        // first; fall back to an admin-privileged shell if /Applications isn't
        // ours to touch.
        let direct = (|| -> std::io::Result<()> {
            match dest.symlink_metadata() {
                Ok(m) if m.is_dir() => std::fs::remove_dir_all(&dest)?,
                Ok(_) => std::fs::remove_file(&dest)?,
                Err(_) => {}
            }
            let status = Command::new("/usr/bin/ditto")
                .arg(&staged)
                .arg(&dest)
                .status()?;
            if !status.success() {
                return Err(std::io::Error::other("ditto into /Applications failed"));
            }
            Ok(())
        })();
        if direct.is_err() {
            let staged_lit = applescript_string_literal(&staged.to_string_lossy());
            let dest_lit = applescript_string_literal(&dest.to_string_lossy());
            let script = format!(
                "set s to quoted form of {staged_lit}\n\
                 set d to quoted form of {dest_lit}\n\
                 do shell script \"rm -rf \" & d & \" && /usr/bin/ditto \" & s & \" \" & d with administrator privileges"
            );
            let status = Command::new("osascript")
                .arg("-e")
                .arg(script)
                .status()
                .map_err(|e| format!("osascript: {e}"))?;
            if !status.success() {
                return Err("failed to move Macros to /Applications".to_string());
            }
        }
        let _ = std::fs::remove_dir_all(&staging);

        spawn_reopen_helper(&dest)
    }

    /// Spawn a detached helper that waits for *this* process to fully exit, then
    /// `open`s `app_path`. Plain `open` (not `open -n`) lets Launch Services
    /// re-bind the user's existing pinned Dock tile. The poll loop avoids racing
    /// the still-quitting old instance. Reparented to launchd, it outlives us.
    fn spawn_reopen_helper(app_path: &Path) -> Result<(), String> {
        let pid = std::process::id();
        let script = format!(
            "while /bin/kill -0 {pid} 2>/dev/null; do /bin/sleep 0.2; done; exec /usr/bin/open \"$1\""
        );
        Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            // App path as `$1` (after the `sh` placeholder for `$0`) so it needs
            // no shell quoting regardless of spaces/quotes.
            .arg("sh")
            .arg(app_path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("spawning relaunch helper: {e}"))
    }

    fn writable_by_me(path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o200 != 0)
    }

    /// True if the filesystem holding `path` is mounted read-only. Mode bits on
    /// DMG / App-Translocation volumes still claim writability, so the mount
    /// flags are the only reliable signal.
    fn volume_is_read_only(path: &Path) -> bool {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c_path` is a valid NUL-terminated string and `sfs` is a valid
        // out-pointer; `statfs` only writes into it on success.
        let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut sfs) };
        rc == 0 && (sfs.f_flags & libc::MNT_RDONLY as u32) != 0
    }

    /// Atomically exchange the directory entries at `from` and `to` via
    /// `renamex_np(2)` with `RENAME_SWAP` (macOS 10.12+). No window where either
    /// path is missing.
    fn renamex_swap(from: &Path, to: &Path) -> std::io::Result<()> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let from_c = CString::new(from.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL byte in path")
        })?;
        let to_c = CString::new(to.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL byte in path")
        })?;

        // SAFETY: both pointers are valid NUL-terminated C strings that outlive
        // the call; `renamex_np` only reads them.
        let rc = unsafe { libc::renamex_np(from_c.as_ptr(), to_c.as_ptr(), libc::RENAME_SWAP) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Wrap `s` in AppleScript double quotes, escaping `\` and `"`. AppleScript
    /// string literals recognise only `\\` and `\"` inside `"..."`.
    fn applescript_string_literal(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for ch in s.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}
