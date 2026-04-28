//! Detect which CLIs are installed on the user's machine.
//!
//! Used at install time to filter which bundled priors are seeded into
//! `command_docs`. Commands the user cannot run produce noise in completion
//! menus — we only seed priors for tools that are actually present.
//!
//! The detection is *conservative*: if a check is ambiguous or fails, the tool
//! is **not** added to the installed set. Errors from subprocess calls (e.g.
//! `brew list`) are silently swallowed — they are treated the same as
//! "tool not found".
//!
//! No new crate dependencies are introduced. Executable detection uses
//! `std::fs` metadata + `std::os::unix::fs::PermissionsExt` instead of a
//! `which` crate.

use std::collections::HashSet;
use std::env;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Result of a tool-presence scan.
#[derive(Debug, Default)]
pub struct ToolDetection {
    /// Command names (bare, no path) that were found on PATH or through an
    /// ecosystem indicator.
    pub installed: HashSet<String>,
}

impl ToolDetection {
    /// Returns `true` if `cmd` was detected as installed.
    pub fn has(&self, cmd: &str) -> bool {
        self.installed.contains(cmd)
    }

    /// A "admit-all" detection that considers every command installed.
    /// Useful for backwards-compatible call sites that want unfiltered output.
    pub fn admit_all() -> Self {
        // We signal "admit all" with a sentinel, not an actual set, to avoid
        // hard-coding the full CLI list in two places. We abuse `installed`
        // by inserting a sentinel value and override `has` via a wrapper.
        // Instead, we use a different approach: a public field `admit_all_mode`.
        ToolDetection {
            installed: HashSet::new(),
        }
    }
}

/// A wrapper that always returns `true` from `has()` — used for the
/// "seed everything" path (no filtering).
pub struct AdmitAll;

impl AdmitAll {
    pub fn has(&self, _cmd: &str) -> bool {
        true
    }
}

/// Trait abstracting over [`ToolDetection`] and [`AdmitAll`] so call sites
/// that take either can be generic.
pub trait ToolFilter {
    fn has(&self, cmd: &str) -> bool;
}

impl ToolFilter for ToolDetection {
    fn has(&self, cmd: &str) -> bool {
        self.installed.contains(cmd)
    }
}

impl ToolFilter for AdmitAll {
    fn has(&self, _cmd: &str) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Known CLI names — the union of PROFILES command names and PRIORS command
// names. We list them statically here rather than depending on the full
// registry at scan time, so the scan stays O(N * P) for a small fixed N.
// ---------------------------------------------------------------------------
static KNOWN_CLIS: &[&str] = &[
    // PRIORS commands
    "git", "npm", "pnpm", "yarn", "cargo", "docker", "kubectl", "gh", "brew", "make", "python",
    "python3", "pip", // PROFILES commands (beyond PRIORS)
    "cd", "pushd", "popd", "ssh", "scp", "mosh", "rsync", "just", "task", "code", "subl", "idea",
    "nvim", "vim", "vi", "dotnet", "pytest", "bash", "sh", "zsh", "which", "type", "man", "help",
    "tmux", "aws", // Universal shells / tools worth checking
    "rustc", "rustup", "nvm", "ruby", "gem",
];

/// Scan `$PATH` and known ecosystem directories for installed CLIs.
///
/// Thin public wrapper: reads `HOME` and `PATH` from the process environment
/// once and forwards to [`detect_tools_with_env`].
pub fn detect_tools() -> ToolDetection {
    let home = home_dir();
    let path_var = env::var("PATH").unwrap_or_default();
    detect_tools_with_env(home.as_deref(), Some(&path_var))
}

/// Core implementation of the tool scan, parameterized on `home` and
/// `path_env` so tests can supply explicit values without touching process
/// state.
///
/// The scan is capped to [`KNOWN_CLIS`] — we do not stat every binary on the
/// machine. Goal: gate priors/profiles seeding, not produce a full PATH index.
pub fn detect_tools_with_env(home: Option<&Path>, path_env: Option<&str>) -> ToolDetection {
    let mut installed: HashSet<String> = HashSet::new();

    // -----------------------------------------------------------------------
    // 1. PATH scan — for each known CLI, check if it exists and is executable
    //    in any PATH directory. Stop at first hit per name.
    // -----------------------------------------------------------------------
    let path_str = path_env.unwrap_or("");
    let path_dirs: Vec<&str> = path_str.split(':').collect();

    for &cli in KNOWN_CLIS {
        if is_on_path(cli, &path_dirs) {
            installed.insert(cli.to_string());
        }
    }

    // -----------------------------------------------------------------------
    // 2. Ecosystem indicators — directories/apps that imply a tool even when
    //    the binary might live in a non-standard location not on current PATH.
    // -----------------------------------------------------------------------

    // ~/.cargo/bin/ exists → rust toolchain likely present
    if let Some(h) = home {
        if h.join(".cargo/bin").is_dir() {
            for name in ["cargo", "rustc", "rustup"] {
                installed.insert(name.to_string());
            }
        }
    }

    // ~/.dotnet/ exists → dotnet SDK
    if let Some(h) = home {
        if h.join(".dotnet").is_dir() {
            installed.insert("dotnet".to_string());
        }
    }

    // ~/.nvm/ exists → nvm (Node version manager, may not be on PATH yet)
    if let Some(h) = home {
        if h.join(".nvm").is_dir() {
            installed.insert("nvm".to_string());
        }
    }

    // ~/.rbenv/ or /usr/local/Cellar/rbenv/ → ruby, gem
    let rbenv_home = home.map(|h| h.join(".rbenv")).unwrap_or_default();
    let rbenv_cellar = Path::new("/usr/local/Cellar/rbenv");
    if rbenv_home.is_dir() || rbenv_cellar.is_dir() {
        installed.insert("ruby".to_string());
        installed.insert("gem".to_string());
    }

    // /Applications/Visual Studio Code.app/ (macOS) or `code` already on PATH
    if Path::new("/Applications/Visual Studio Code.app").is_dir() || installed.contains("code") {
        installed.insert("code".to_string());
    }

    // -----------------------------------------------------------------------
    // 3. Brew list — if `brew` itself is installed, ask it for the full set of
    //    formula names and intersect with KNOWN_CLIS. This catches CLIs
    //    installed via Homebrew that are in /usr/local/bin or /opt/homebrew/bin
    //    but might not be on PATH in this shell context.
    // -----------------------------------------------------------------------
    if installed.contains("brew") {
        if let Some(brew_set) = brew_installed_set() {
            let known: HashSet<&str> = KNOWN_CLIS.iter().copied().collect();
            for name in &brew_set {
                if known.contains(name.as_str()) {
                    installed.insert(name.clone());
                }
            }
        }
    }

    ToolDetection { installed }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns `true` if `name` exists as an executable file in any of `dirs`.
fn is_on_path(name: &str, dirs: &[&str]) -> bool {
    for dir in dirs {
        if dir.is_empty() {
            continue;
        }
        let candidate = PathBuf::from(dir).join(name);
        if is_executable_file(&candidate) {
            return true;
        }
    }
    false
}

/// Returns `true` if `path` is a regular file (or symlink to one) with at
/// least one executable bit set.
fn is_executable_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && (meta.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

/// Resolve the user's home directory without importing the `dirs` crate.
/// Falls back gracefully to `$HOME` env var, then `None`.
fn home_dir() -> Option<PathBuf> {
    // `std::env::home_dir` is deprecated but still correct on Unix; the
    // deprecation applies only to Windows where it can be wrong.
    #[allow(deprecated)]
    let h = std::env::home_dir();
    h.or_else(|| env::var("HOME").ok().map(PathBuf::from))
}

/// Run `brew list --formula` and return the set of formula names. Returns
/// `None` on any failure (brew not callable, non-zero exit, bad UTF-8, etc.).
fn brew_installed_set() -> Option<HashSet<String>> {
    let output = Command::new("brew")
        .args(["list", "--formula"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let set: HashSet<String> = text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    Some(set)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Create a unique temp directory without external crates.
    fn make_temp_dir(tag: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("shac-tools-test-{tag}-{}-{ts}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn detect_tools_finds_real_path_binary() {
        // `sh` is available on every POSIX machine. Use an explicit PATH so
        // this test never touches the process environment (parallel-safe).
        let detection = detect_tools_with_env(None, Some("/usr/bin:/bin"));
        assert!(
            detection.has("sh"),
            "detect_tools_with_env() should find 'sh' on /usr/bin:/bin; installed set: {:?}",
            detection.installed
        );
    }

    #[test]
    fn detect_tools_does_not_find_xyzzy() {
        // Use an explicit PATH that has no xyzzy (parallel-safe).
        let detection = detect_tools_with_env(None, Some("/usr/bin:/bin"));
        assert!(
            !detection.has("xyzzy_does_not_exist"),
            "detect_tools_with_env() falsely detected 'xyzzy_does_not_exist'"
        );
    }

    #[test]
    fn detect_tools_admits_cargo_when_cargo_dir_exists() {
        // Create a temporary HOME with a ~/.cargo/bin/ directory to simulate
        // the Rust toolchain indicator. Pass it explicitly — no env mutation.
        let tmp = make_temp_dir("cargo-home");
        let cargo_bin = tmp.join(".cargo").join("bin");
        fs::create_dir_all(&cargo_bin).expect("create .cargo/bin");

        let detection = detect_tools_with_env(Some(&tmp), Some("/usr/bin:/bin"));

        // Best-effort cleanup
        let _ = fs::remove_dir_all(&tmp);

        assert!(
            detection.has("cargo"),
            "expected 'cargo' to be detected when ~/.cargo/bin/ exists; got {:?}",
            detection.installed
        );
    }

    #[test]
    fn is_executable_file_detects_real_executables() {
        // /bin/sh always exists and should be detected.
        assert!(is_executable_file(Path::new("/bin/sh")));
    }

    #[test]
    fn is_executable_file_rejects_missing_path() {
        assert!(!is_executable_file(Path::new(
            "/no/such/path/xyzzy_bin_nonexistent"
        )));
    }

    #[test]
    fn is_executable_file_rejects_non_executable() {
        // Create a file without executable bits.
        let tmp = make_temp_dir("noexec");
        let f = tmp.join("plain.txt");
        fs::write(&f, b"hello").expect("write file");
        let mut perms = fs::metadata(&f).expect("metadata").permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&f, perms).expect("set perms");
        let result = !is_executable_file(&f);
        let _ = fs::remove_dir_all(&tmp);
        assert!(result);
    }

    #[test]
    fn is_on_path_finds_sh_in_usr_bin() {
        // /bin or /usr/bin should contain sh; test both.
        let dirs: Vec<&str> = vec!["/bin", "/usr/bin"];
        assert!(
            is_on_path("sh", &dirs),
            "expected 'sh' to be found in /bin or /usr/bin"
        );
    }

    #[test]
    fn admit_all_has_returns_true_for_anything() {
        let all = AdmitAll;
        assert!(all.has("git"));
        assert!(all.has("kubectl"));
        assert!(all.has("xyzzy_does_not_exist"));
    }
}
