#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use shac::config::AppPaths;

pub struct TestEnv {
    pub root: PathBuf,
    pub home: PathBuf,
    pub config_home: PathBuf,
    pub data_home: PathBuf,
    pub state_home: PathBuf,
    pub shac: PathBuf,
    pub shacd: PathBuf,
    pub bin_dir: PathBuf,
}

impl TestEnv {
    pub fn new(name: &str) -> Self {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root =
            PathBuf::from("/tmp").join(format!("shac-{name}-{}-{suffix}", std::process::id()));
        let home = root.join("home");
        let config_home = root.join("config");
        let data_home = root.join("data");
        let state_home = root.join("state");
        for dir in [&home, &config_home, &data_home, &state_home] {
            fs::create_dir_all(dir).expect("create test dir");
        }

        let shac = PathBuf::from(env!("CARGO_BIN_EXE_shac"));
        let shacd = PathBuf::from(env!("CARGO_BIN_EXE_shacd"));
        let bin_dir = shac.parent().expect("shac binary dir").to_path_buf();

        // Write a test config that raises daemon_timeout_ms. Default 150ms is too
        // tight under concurrent spawning; 1000ms still flakes on Ubuntu CI runners
        // under v0.5.0 (extra DB query per /complete from tip selection).
        let shac_config_dir = config_home.join("shac");
        fs::create_dir_all(&shac_config_dir).expect("create shac config dir");
        fs::write(
            shac_config_dir.join("config.toml"),
            "daemon_timeout_ms = 5000\n",
        )
        .expect("write test config");

        Self {
            root,
            home,
            config_home,
            data_home,
            state_home,
            shac,
            shacd,
            bin_dir,
        }
    }

    pub fn shac_cmd(&self) -> Command {
        let mut command = Command::new(&self.shac);
        self.apply_env(&mut command);
        command
    }

    pub fn apply_env(&self, command: &mut Command) {
        command
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("XDG_DATA_HOME", &self.data_home)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("SHAC_DAEMON_BIN", &self.shacd)
            .env("TTY", "test-tty")
            .env("PATH", self.test_path());
    }

    pub fn path_with_prefix(&self, prefix: &Path) -> String {
        let existing = self.test_path();
        std::env::join_paths(
            std::iter::once(prefix.to_path_buf()).chain(std::env::split_paths(&existing)),
        )
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
    }

    pub fn zsh_script_path(&self) -> PathBuf {
        self.config_home.join("shac").join("shell").join("shac.zsh")
    }

    pub fn app_paths(&self) -> AppPaths {
        let config_dir = self.config_home.join("shac");
        let data_dir = self.data_home.join("shac");
        let state_dir = self.state_home.join("shac");
        AppPaths {
            config_file: config_dir.join("config.toml"),
            db_file: data_dir.join("shac.db"),
            socket_file: state_dir.join("shacd.sock"),
            pid_file: state_dir.join("shacd.pid"),
            shell_dir: config_dir.join("shell"),
            config_dir,
            data_dir,
            state_dir,
        }
    }

    pub fn stop_daemon(&self) {
        let mut command = self.shac_cmd();
        command.args(["daemon", "stop"]);
        let _ = command.output();
    }

    pub fn spawn_daemon(&self) -> TestDaemon {
        self.spawn_daemon_with_extra_env(&[] as &[(&str, &str)])
    }

    /// Spawn the daemon with additional environment variables layered on top of the
    /// standard test environment.  Used by tests that need to override runtime
    /// knobs such as SHAC_BG_REINDEX_INTERVAL_SECS.
    ///
    /// The background indexer is disabled by default (`SHAC_BG_DISABLED=1`) to
    /// prevent SQLite write contention in tests that don't need it.  Pass
    /// `("SHAC_BG_DISABLED", "0")` in `extra` to opt back in.
    pub fn spawn_daemon_with_extra_env<K, V>(&self, extra: &[(K, V)]) -> TestDaemon
    where
        K: AsRef<std::ffi::OsStr>,
        V: AsRef<std::ffi::OsStr>,
    {
        let mut command = Command::new(&self.shacd);
        self.apply_env(&mut command);
        // Disable the background indexer by default to avoid SQLite write
        // contention in tests.  Individual tests can override this by passing
        // ("SHAC_BG_DISABLED", "0") in their extra_env.
        command.env("SHAC_BG_DISABLED", "1");
        for (k, v) in extra {
            command.env(k, v);
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn shacd");
        let socket = self.state_home.join("shac").join("shacd.sock");
        for _ in 0..50 {
            if socket.exists() {
                return TestDaemon { child: Some(child) };
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("shacd did not create socket: {}", socket.display());
    }

    fn test_path(&self) -> String {
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![self.bin_dir.clone()];
        paths.extend(std::env::split_paths(&existing));
        std::env::join_paths(paths)
            .unwrap_or(existing)
            .to_string_lossy()
            .to_string()
    }
}

pub struct TestDaemon {
    child: Option<Child>,
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        self.stop_daemon();
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn run_ok<I, S>(env: &TestEnv, args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = env.shac_cmd();
    command.args(args);
    let output = command.output().expect("run shac command");
    assert!(
        output.status.success(),
        "command failed: {:?}\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        command,
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

pub fn command_available(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub fn current_repo() -> String {
    std::env::current_dir()
        .expect("current dir")
        .to_string_lossy()
        .to_string()
}

pub fn assert_path_exists(path: &Path) {
    assert!(path.exists(), "expected path to exist: {}", path.display());
}
