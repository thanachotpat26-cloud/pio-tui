//! End-to-end tests of the `pio-tui` binary.
//!
//! Most tests use a fake `pio` script placed first on `PATH`, so they are
//! deterministic and don't need PlatformIO. Tests marked `#[ignore]` use the
//! real PlatformIO install: `cargo test -- --ignored`.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant},
};

const BIN: &str = env!("CARGO_BIN_EXE_pio-tui");

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("pio-tui-it-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        Self(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A command for the binary with an isolated cache/home (so tests never touch
/// the real board cache or log directory) and, optionally, a fake `pio`.
fn cmd(sandbox: &Path, fake_pio: Option<&str>) -> Command {
    let mut c = Command::new(BIN);
    let home = sandbox.join("home");
    fs::create_dir_all(&home).unwrap();
    c.env("HOME", &home).env("XDG_CACHE_HOME", sandbox.join("cache"));
    if let Some(script) = fake_pio {
        let bin = sandbox.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let p = bin.join("pio");
        fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
        )
        .unwrap();
        c.env("PATH", path);
    }
    c
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn project(dir: &Path) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("platformio.ini"), "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n").unwrap();
    fs::write(dir.join("src/main.cpp"), "#include <Arduino.h>\nvoid setup() {}\nvoid loop() {}\n").unwrap();
}

#[test]
fn help_and_version() {
    let t = TempDir::new("help");
    let o = cmd(t.path(), None).arg("--help").output().unwrap();
    assert!(o.status.success());
    assert!(text(&o).contains("compiledb"));
    let o = cmd(t.path(), None).arg("--version").output().unwrap();
    assert!(text(&o).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn compiledb_rejects_non_project_and_writes_nothing() {
    let t = TempDir::new("noproj");
    let dir = t.path().join("empty");
    fs::create_dir_all(&dir).unwrap();
    let o = cmd(t.path(), Some("exit 0")).args(["compiledb"]).arg(&dir).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("not a PlatformIO project"), "{}", text(&o));
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0, "nothing may be written into the directory");
}

#[test]
fn compiledb_reports_pio_failure_with_log_file() {
    let t = TempDir::new("fail");
    let dir = t.path().join("proj");
    project(&dir);
    let o = cmd(t.path(), Some("echo 'boom from pio' >&2; exit 2"))
        .args(["compiledb", "--no-clangd"])
        .arg(&dir)
        .output()
        .unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("exited with code 2"), "{out}");
    assert!(out.contains("boom from pio"), "pio output is streamed: {out}");
    let log = out.lines().find_map(|l| l.split("full log: ").nth(1)).expect("log path printed");
    let log = log.trim_end_matches("\x1b[0m");
    let content = fs::read_to_string(log).unwrap();
    assert!(content.contains("boom from pio"), "log file has the full output");
}

#[test]
fn compiledb_success_does_not_modify_existing_project() {
    let t = TempDir::new("ok");
    let dir = t.path().join("proj");
    project(&dir);
    fs::write(dir.join(".gitignore"), "mine\n").unwrap();
    // Fake pio writes a compilation database like the real one does.
    let script = r#"d=""; while [ $# -gt 0 ]; do [ "$1" = "-d" ] && d="$2"; shift; done
printf '[{"command":"/tc/bin/avr-g++ -c src/main.cpp","file":"src/main.cpp","directory":"%s"}]' "$d" > "$d/compile_commands.json""#;
    let o = cmd(t.path(), Some(script)).args(["compiledb"]).arg(&dir).output().unwrap();
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("ready (1 entry)"), "{out}");
    assert!(out.contains("--query-driver=/tc/bin/*"), "{out}");
    assert_eq!(fs::read_to_string(dir.join(".gitignore")).unwrap(), "mine\n", ".gitignore must be untouched");
    assert!(dir.join(".clangd").is_file(), ".clangd written when requested");

    // An existing .clangd is never overwritten.
    fs::write(dir.join(".clangd"), "custom").unwrap();
    let o = cmd(t.path(), Some(script)).args(["compiledb"]).arg(&dir).output().unwrap();
    assert!(o.status.success());
    assert_eq!(fs::read_to_string(dir.join(".clangd")).unwrap(), "custom");
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_pio_and_its_children() {
    let t = TempDir::new("cancel");
    let dir = t.path().join("proj");
    project(&dir);
    let pids = t.path().join("pids");
    // Like real pio: a parent that spawns a long-running child (scons/gcc).
    let script = format!("sleep 60 &\necho \"$$ $!\" > '{}'\nwait", pids.display());
    let mut child = cmd(t.path(), Some(&script)).args(["compiledb"]).arg(&dir).spawn().unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    while !pids.exists() || fs::read_to_string(&pids).unwrap().split_whitespace().count() < 2 {
        assert!(Instant::now() < deadline, "fake pio never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let ids: Vec<i32> = fs::read_to_string(&pids).unwrap().split_whitespace().map(|s| s.parse().unwrap()).collect();

    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < deadline, "pio-tui did not exit after Ctrl+C");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(130));
    for pid in ids {
        // kill(pid, 0) fails with ESRCH once the process is gone (and reaped by init).
        let gone = (0..100).any(|_| {
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if alive {
                std::thread::sleep(Duration::from_millis(20));
            }
            !alive
        });
        assert!(gone, "process {pid} from pio's tree survived cancellation");
    }
}

/// Uses the real PlatformIO install and the atmelavr platform.
#[test]
#[ignore]
fn real_pio_compiledb_on_avr_project() {
    let t = TempDir::new("real");
    let dir = t.path().join("proj");
    project(&dir);
    // Real pio must keep using the user's real PlatformIO home.
    let mut c = Command::new(BIN);
    let o = c.args(["compiledb"]).arg(&dir).output().unwrap();
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let db: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("compile_commands.json")).unwrap()).unwrap();
    assert!(db.as_array().is_some_and(|a| !a.is_empty()));
    assert!(dir.join(".clangd").is_file());
    assert!(!dir.join(".gitignore").exists(), "existing projects get no .gitignore");
}
