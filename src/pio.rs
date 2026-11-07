//! Everything that talks to PlatformIO: the board catalogue, the package
//! registry, and the background jobs that scaffold projects and generate the
//! compilation database.

use std::{
    collections::HashSet,
    fmt::Write as _,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::Sender,
    thread,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

const REGISTRY_SEARCH: &str = "https://api.registry.platformio.org/v3/search";
const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

// ───────────────────────────── Boards ─────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Board {
    pub id: String,
    #[serde(default, deserialize_with = "de_str")]
    pub name: String,
    #[serde(default, deserialize_with = "de_str")]
    pub platform: String,
    #[serde(default, deserialize_with = "de_str")]
    pub mcu: String,
    #[serde(default, deserialize_with = "de_u64")]
    pub fcpu: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub ram: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub rom: u64,
    #[serde(default)]
    pub frameworks: Vec<String>,
    #[serde(default, deserialize_with = "de_str")]
    pub vendor: String,
    #[serde(default, deserialize_with = "de_str")]
    pub url: String,
}

impl Board {
    pub fn haystack(&self) -> String {
        format!("{} {} {} {}", self.id, self.name, self.vendor, self.mcu)
    }
}

fn de_str<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Null => String::new(),
        v => v.to_string(),
    })
}

fn de_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f as u64)).unwrap_or(0),
        Value::String(s) => s.trim_end_matches(['L', 'l']).parse().unwrap_or(0),
        _ => 0,
    })
}

fn cache_file() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("pio-tui").join("boards.json"))
}

/// Load the board catalogue, from the local cache when fresh, otherwise via
/// `pio boards --json-output` (and refresh the cache).
pub fn load_boards(refresh: bool) -> Result<Vec<Board>> {
    if !refresh {
        if let Some(path) = cache_file() {
            let fresh = fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age < CACHE_MAX_AGE);
            if fresh {
                if let Ok(b) = fs::read(&path).map_err(anyhow::Error::from).and_then(|d| {
                    serde_json::from_slice::<Vec<Board>>(&d).map_err(Into::into)
                }) {
                    if !b.is_empty() {
                        return Ok(b);
                    }
                }
            }
        }
    }

    let out = Command::new(pio_bin())
        .args(["boards", "--json-output"])
        .stdin(Stdio::null())
        .output()
        .context("could not run `pio` — is PlatformIO Core installed?")?;
    if !out.status.success() {
        bail!("`pio boards` failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let mut boards: Vec<Board> =
        serde_json::from_slice(&out.stdout).context("unexpected `pio boards` output")?;
    boards.sort_by(|a, b| {
        a.platform
            .cmp(&b.platform)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    if let Some(path) = cache_file() {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(&path, serde_json::to_vec(&boards)?);
    }
    Ok(boards)
}

pub fn platformio_home() -> Option<PathBuf> {
    std::env::var_os("PLATFORMIO_CORE_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".platformio")))
}

pub fn installed_platforms() -> HashSet<String> {
    let Some(dir) = platformio_home().map(|h| h.join("platforms")) else {
        return HashSet::new();
    };
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.split('@').next().map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Locate the `pio` executable: `$PATH` first, then PlatformIO's own penv.
pub fn pio_bin() -> PathBuf {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in ["pio", "platformio"] {
                let p = dir.join(name);
                if p.is_file() {
                    return p;
                }
            }
        }
    }
    if let Some(home) = platformio_home() {
        let p = home.join("penv").join("bin").join("pio");
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from("pio")
}

// ───────────────────────────── Libraries ─────────────────────────────

#[derive(Debug, Clone)]
pub struct Library {
    pub owner: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub keywords: Vec<String>,
    pub stars: u64,
    pub license: String,
}

impl Library {
    /// The `lib_deps` spec, e.g. `bblanchon/ArduinoJson@^7.2.0`.
    pub fn spec(&self) -> String {
        // Snapshot builds (e.g. mbed imports: `0.0.0+sha.abc`) don't take a semver range.
        if self.version.is_empty() || self.version.contains('+') || self.version.starts_with("0.0.0") {
            format!("{}/{}", self.owner, self.name)
        } else {
            format!("{}/{}@^{}", self.owner, self.name, self.version)
        }
    }

    pub fn key(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// Search the PlatformIO registry. An empty query lists the most popular
/// libraries (optionally for one platform).
pub fn search_libraries(query: &str, platform: Option<&str>) -> Result<(Vec<Library>, u64)> {
    let mut q = String::from("type:library");
    if let Some(p) = platform {
        let _ = write!(q, " platform:{p}");
    }
    let query = query.trim();
    if !query.is_empty() {
        q.push(' ');
        q.push_str(query);
    }
    let sort = if query.is_empty() { "popularity" } else { "relevance" };

    let resp: Value = ureq::get(REGISTRY_SEARCH)
        .query("query", &q)
        .query("page", "1")
        .query("limit", "50")
        .query("sort", sort)
        .timeout(Duration::from_secs(15))
        .call()
        .context("registry request failed")?
        .into_json()
        .context("invalid registry response")?;

    let s = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let total = resp["total"].as_u64().unwrap_or(0);
    let items = resp["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|it| Library {
                    owner: s(&it["owner"]["username"]),
                    name: s(&it["name"]),
                    description: s(&it["description"]).split_whitespace().collect::<Vec<_>>().join(" "),
                    version: s(&it["version"]["name"]),
                    keywords: it["keywords"]
                        .as_array()
                        .map(|k| k.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                        .unwrap_or_default(),
                    stars: it["stars_count"].as_u64().unwrap_or(0),
                    license: s(&it["license"]),
                })
                .filter(|l| !l.name.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Ok((items, total))
}

// ───────────────────────────── Project spec ─────────────────────────────

#[derive(Debug, Clone)]
pub struct ProjectSpec {
    pub name: String,
    pub parent: PathBuf,
    pub board: Board,
    pub framework: Option<String>,
    pub libs: Vec<String>,
    pub monitor_speed: String,
    pub upload_port: String,
    pub compiledb: bool,
    pub clangd: bool,
    pub template: bool,
    pub git: bool,
}

impl ProjectSpec {
    pub fn path(&self) -> PathBuf {
        self.parent.join(&self.name)
    }

    pub fn ini(&self) -> String {
        let b = &self.board;
        let mut s = String::new();
        s.push_str("; PlatformIO Project Configuration File — generated by pio-tui\n");
        s.push_str("; https://docs.platformio.org/page/projectconf.html\n\n");
        let _ = writeln!(s, "[platformio]\ndefault_envs = {}\n", b.id);
        let _ = writeln!(s, "[env:{}]", b.id);
        let _ = writeln!(s, "platform = {}", b.platform);
        let _ = writeln!(s, "board = {}", b.id);
        if let Some(fw) = &self.framework {
            let _ = writeln!(s, "framework = {fw}");
        }
        if !self.monitor_speed.trim().is_empty() {
            let _ = writeln!(s, "monitor_speed = {}", self.monitor_speed.trim());
        }
        if b.platform == "espressif32" {
            s.push_str("monitor_filters = esp32_exception_decoder\n");
        }
        if !self.upload_port.trim().is_empty() {
            let _ = writeln!(s, "upload_port = {}", self.upload_port.trim());
        }
        if !self.libs.is_empty() {
            s.push_str("lib_deps =\n");
            for l in &self.libs {
                let _ = writeln!(s, "    {l}");
            }
        }
        s
    }

    fn template(&self) -> (&'static str, String) {
        let baud = self.monitor_speed.trim();
        let baud = if baud.is_empty() { "115200" } else { baud };
        match self.framework.as_deref() {
            Some("arduino") => (
                "main.cpp",
                format!(
                    "#include <Arduino.h>\n\nvoid setup() {{\n  Serial.begin({baud});\n  Serial.println(\"Hello from {name}!\");\n}}\n\nvoid loop() {{\n  delay(1000);\n}}\n",
                    name = self.name
                ),
            ),
            Some("espidf") => (
                "main.c",
                format!(
                    "#include <stdio.h>\n#include \"freertos/FreeRTOS.h\"\n#include \"freertos/task.h\"\n\nvoid app_main(void)\n{{\n    printf(\"Hello from {}!\\n\");\n    while (1) {{\n        vTaskDelay(pdMS_TO_TICKS(1000));\n    }}\n}}\n",
                    self.name
                ),
            ),
            Some("zephyr") => (
                "main.c",
                "#include <zephyr/kernel.h>\n\nint main(void)\n{\n    while (1) {\n        printk(\"Hello, Zephyr!\\n\");\n        k_msleep(1000);\n    }\n    return 0;\n}\n".into(),
            ),
            Some("mbed") => (
                "main.cpp",
                "#include \"mbed.h\"\n\nint main()\n{\n    while (true) {\n        ThisThread::sleep_for(1000ms);\n    }\n}\n".into(),
            ),
            _ => ("main.c", "int main(void)\n{\n    for (;;) {\n    }\n    return 0;\n}\n".into()),
        }
    }
}

// ───────────────────────────── Jobs ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Pending,
    Running,
    Ok,
    Failed,
    Skipped,
}

#[derive(Debug, Clone)]
pub struct CdbInfo {
    pub entries: usize,
    pub compiler: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub project: PathBuf,
    pub cdb: Option<CdbInfo>,
}

#[derive(Debug)]
pub enum JobMsg {
    Steps(Vec<String>),
    Step(usize, StepState),
    Log(String),
    Done(Result<Summary, String>),
}

struct Job {
    tx: Sender<JobMsg>,
    next: usize,
}

impl Job {
    fn new(tx: Sender<JobMsg>, steps: &[&str]) -> Self {
        let _ = tx.send(JobMsg::Steps(steps.iter().map(|s| s.to_string()).collect()));
        Self { tx, next: 0 }
    }

    fn log(&self, s: impl Into<String>) {
        let _ = self.tx.send(JobMsg::Log(s.into()));
    }

    fn step(&mut self, f: impl FnOnce(&Job) -> Result<()>) -> Result<()> {
        let i = self.next;
        self.next += 1;
        let _ = self.tx.send(JobMsg::Step(i, StepState::Running));
        let r = f(self);
        let state = if r.is_ok() { StepState::Ok } else { StepState::Failed };
        let _ = self.tx.send(JobMsg::Step(i, state));
        r
    }

    fn finish(self, total: usize, r: Result<Summary>) {
        if r.is_err() {
            for i in self.next..total {
                let _ = self.tx.send(JobMsg::Step(i, StepState::Skipped));
            }
        }
        let _ = self.tx.send(JobMsg::Done(r.map_err(|e| format!("{e:#}"))));
    }

    /// Run a command, streaming stdout/stderr into the log.
    fn run(&self, cmd: &mut Command) -> Result<()> {
        let shown = std::iter::once(
            Path::new(cmd.get_program()).file_name().unwrap_or_default().to_string_lossy().into_owned(),
        )
        .chain(cmd.get_args().map(|a| shell_quote(&a.to_string_lossy())))
        .collect::<Vec<_>>()
        .join(" ");
        self.log(format!("$ {shown}"));

        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PLATFORMIO_DISABLE_PROGRESSBAR", "true")
            .env("PYTHONUNBUFFERED", "1")
            .spawn()
            .with_context(|| format!("failed to start `{shown}`"))?;
        let out = child.stdout.take().map(|r| pump(r, self.tx.clone()));
        let err = child.stderr.take().map(|r| pump(r, self.tx.clone()));
        let status = child.wait()?;
        for h in [out, err].into_iter().flatten() {
            let _ = h.join();
        }
        if !status.success() {
            bail!("`{shown}` exited with {}", status.code().map_or("a signal".into(), |c| format!("code {c}")));
        }
        Ok(())
    }
}

fn shell_quote(s: &str) -> String {
    if s.chars().any(|c| c.is_whitespace() || c == '\'' || c == '"') {
        format!("'{}'", s.replace('\'', r"'\''"))
    } else {
        s.to_owned()
    }
}

fn pump<R: Read + Send + 'static>(mut r: R, tx: Sender<JobMsg>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut line = Vec::new();
        let flush = |line: &mut Vec<u8>| {
            let s = strip_ansi(&String::from_utf8_lossy(line));
            if !s.trim().is_empty() {
                let _ = tx.send(JobMsg::Log(s.trim_end().to_owned()));
            }
            line.clear();
        };
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    for &b in &buf[..n] {
                        if b == b'\n' || b == b'\r' {
                            flush(&mut line);
                        } else {
                            line.push(b);
                        }
                    }
                }
            }
        }
        flush(&mut line);
    })
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            if it.peek() == Some(&'[') {
                it.next();
                for c in it.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else if c == '\t' {
            out.push_str("    ");
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// Scaffold a brand-new project in a background thread.
pub fn spawn_create(spec: ProjectSpec, tx: Sender<JobMsg>) {
    thread::spawn(move || {
        let mut names = vec!["Create project skeleton", "Write platformio.ini"];
        if spec.template {
            names.push("Write source template");
        }
        names.push("Install platform, toolchain & libraries");
        if spec.compiledb {
            names.push("Generate compile_commands.json");
        }
        if spec.clangd {
            names.push("Write .clangd config");
        }
        if spec.git {
            names.push("Initialize git repository");
        }
        let total = names.len();
        let mut job = Job::new(tx, &names);
        let dir = spec.path();
        let pio = pio_bin();

        let r = (|| -> Result<Summary> {
            job.step(|j| {
                fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
                j.run(Command::new(&pio).args(["project", "init", "--no-install-dependencies", "-d"]).arg(&dir))
                    .or_else(|_| j.run(Command::new(&pio).args(["project", "init", "-d"]).arg(&dir)))
            })?;
            job.step(|j| {
                fs::write(dir.join("platformio.ini"), spec.ini())?;
                ensure_gitignore(&dir)?;
                j.log("wrote platformio.ini");
                Ok(())
            })?;
            if spec.template {
                job.step(|j| {
                    let src = dir.join("src");
                    fs::create_dir_all(&src)?;
                    let empty = fs::read_dir(&src)?.flatten().all(|e| e.file_name().to_string_lossy().starts_with('.'));
                    if empty {
                        let (file, body) = spec.template();
                        fs::write(src.join(file), body)?;
                        j.log(format!("wrote src/{file}"));
                    } else {
                        j.log("src/ is not empty — leaving it untouched");
                    }
                    Ok(())
                })?;
            }
            job.step(|j| j.run(Command::new(&pio).args(["pkg", "install", "-d"]).arg(&dir)))?;
            if spec.compiledb {
                job.step(|j| j.run(Command::new(&pio).args(["run", "-t", "compiledb", "-d"]).arg(&dir)))?;
            }
            if spec.clangd {
                job.step(|j| write_clangd(&dir, j))?;
            }
            if spec.git {
                job.step(|j| {
                    if dir.join(".git").exists() {
                        j.log("already a git repository");
                        Ok(())
                    } else {
                        j.run(Command::new("git").args(["init", "-q"]).arg(&dir))
                    }
                })?;
            }
            Ok(Summary { cdb: cdb_info(&dir), project: dir.clone() })
        })();
        job.finish(total, r);
    });
}

/// Generate `compile_commands.json` for an existing project.
pub fn spawn_compiledb(dir: PathBuf, env: Option<String>, clangd: bool, tx: Sender<JobMsg>) {
    thread::spawn(move || {
        let mut names = vec!["Generate compile_commands.json"];
        if clangd {
            names.push("Write .clangd config");
        }
        let total = names.len();
        let mut job = Job::new(tx, &names);
        let pio = pio_bin();
        let r = (|| -> Result<Summary> {
            job.step(|j| {
                if !dir.join("platformio.ini").is_file() {
                    bail!("{} is not a PlatformIO project (no platformio.ini)", dir.display());
                }
                let mut cmd = Command::new(&pio);
                cmd.args(["run", "-t", "compiledb", "-d"]).arg(&dir);
                if let Some(e) = &env {
                    cmd.args(["-e", e]);
                }
                j.run(&mut cmd)
            })?;
            if clangd {
                job.step(|j| write_clangd(&dir, j))?;
            }
            Ok(Summary { cdb: cdb_info(&dir), project: dir.clone() })
        })();
        job.finish(total, r);
    });
}

const CLANGD: &str = r#"# Generated by pio-tui — makes clangd happy with PlatformIO's GCC cross toolchains.
# For full system-header resolution start clangd with:
#   --query-driver=<toolchain>/bin/*
CompileFlags:
  CompilationDatabase: .
  Remove:
    - -mlongcalls
    - -mtext-section-literals
    - -mfix-esp32-psram-cache-issue
    - -mfix-esp32-psram-cache-strategy=*
    - -mdisable-hardware-atomics
    - -mtarget-align
    - -mno-target-align
    - -fstrict-volatile-bitfields
    - -fno-tree-switch-conversion
    - -fno-shrink-wrap
    - -fipa-pta
    - -free
    - -fno-split-wide-types
    - -fno-jump-tables
    - -flto*
    - -fuse-linker-plugin
    - -fno-fat-lto-objects
  Add:
    - -Wno-unknown-warning-option
    - -Wno-unknown-attributes
Diagnostics:
  Suppress:
    - pp_including_mainfile_in_preamble
    - drv_unknown_argument
    - drv_unknown_argument_with_suggestion
"#;

fn write_clangd(dir: &Path, j: &Job) -> Result<()> {
    let p = dir.join(".clangd");
    if p.exists() {
        j.log(".clangd already exists — kept as-is");
    } else {
        fs::write(&p, CLANGD)?;
        j.log("wrote .clangd");
    }
    ensure_gitignore(dir)?;
    Ok(())
}

fn ensure_gitignore(dir: &Path) -> Result<()> {
    let p = dir.join(".gitignore");
    let mut cur = fs::read_to_string(&p).unwrap_or_default();
    let mut changed = false;
    for line in [".pio", ".cache/", "compile_commands.json"] {
        if !cur.lines().any(|l| l.trim() == line) {
            if !cur.is_empty() && !cur.ends_with('\n') {
                cur.push('\n');
            }
            cur.push_str(line);
            cur.push('\n');
            changed = true;
        }
    }
    if changed {
        fs::write(p, cur)?;
    }
    Ok(())
}

pub fn cdb_info(dir: &Path) -> Option<CdbInfo> {
    let data = fs::read(dir.join("compile_commands.json")).ok()?;
    let v: Vec<Value> = serde_json::from_slice(&data).ok()?;
    let compiler = v.first().and_then(|e| {
        e["arguments"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| e["command"].as_str().and_then(|c| c.split_whitespace().next()).map(str::to_owned))
    });
    Some(CdbInfo { entries: v.len(), compiler })
}

/// `[env:NAME]` sections in a project's platformio.ini.
pub fn parse_envs(dir: &Path) -> Option<Vec<String>> {
    let ini = fs::read_to_string(dir.join("platformio.ini")).ok()?;
    Some(
        ini.lines()
            .filter_map(|l| l.trim().strip_prefix("[env:")?.strip_suffix(']').map(|s| s.trim().to_owned()))
            .collect(),
    )
}

pub fn expand_tilde(s: &str) -> PathBuf {
    let s = s.trim();
    if s == "~" {
        return dirs::home_dir().unwrap_or_default();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest);
        }
    }
    PathBuf::from(s)
}

pub fn contract_tilde(p: &Path) -> String {
    if let Some(h) = dirs::home_dir() {
        if let Ok(rest) = p.strip_prefix(&h) {
            return format!("~/{}", rest.display());
        }
    }
    p.display().to_string()
}

pub fn fmt_hz(hz: u64) -> String {
    match hz {
        0 => "—".into(),
        h if h >= 1_000_000 => format!("{} MHz", trim_float(h as f64 / 1e6)),
        h if h >= 1_000 => format!("{} kHz", trim_float(h as f64 / 1e3)),
        h => format!("{h} Hz"),
    }
}

pub fn fmt_bytes(b: u64) -> String {
    match b {
        0 => "—".into(),
        b if b >= 1 << 20 => format!("{} MB", trim_float(b as f64 / (1u64 << 20) as f64)),
        b if b >= 1 << 10 => format!("{} KB", trim_float(b as f64 / 1024.0)),
        b => format!("{b} B"),
    }
}

fn trim_float(f: f64) -> String {
    let s = format!("{f:.1}");
    s.strip_suffix(".0").map(str::to_owned).unwrap_or(s)
}
