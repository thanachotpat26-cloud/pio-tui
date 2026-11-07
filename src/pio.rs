//! Everything that talks to PlatformIO: the board catalogue, the package
//! registry, and the background jobs that scaffold projects and generate the
//! compilation database.

use std::{
    collections::HashSet,
    fmt::Write as _,
    fs::{self, File},
    io::{BufWriter, Read, Write as _},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

const REGISTRY_SEARCH: &str = "https://api.registry.platformio.org/v3/search";
const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
const KEEP_LOGS: usize = 20;

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
        Value::String(s) => s.trim().trim_end_matches(['L', 'l']).parse().unwrap_or(0),
        _ => 0,
    })
}

fn app_cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("pio-tui"))
}

/// Write via a temp file + rename so a crash or a second instance can never
/// leave a half-written file behind.
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.{}.tmp", path.file_name().unwrap_or_default().to_string_lossy(), std::process::id()));
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// Load the board catalogue, from the local cache when fresh, otherwise via
/// `pio boards --json-output` (and refresh the cache).
pub fn load_boards(refresh: bool) -> Result<Vec<Board>> {
    let cache = app_cache_dir().map(|d| d.join("boards.json"));
    if !refresh {
        if let Some(path) = &cache {
            let fresh = fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age < CACHE_MAX_AGE);
            if fresh {
                if let Some(b) = fs::read(path).ok().and_then(|d| serde_json::from_slice::<Vec<Board>>(&d).ok()) {
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
    let boards = parse_boards(&out.stdout)?;
    if let Some(path) = &cache {
        // A cache write failure only costs speed next time; never fail the load.
        let _ = serde_json::to_vec(&boards).map_err(anyhow::Error::from).and_then(|d| write_atomic(path, &d));
    }
    Ok(boards)
}

pub fn parse_boards(json: &[u8]) -> Result<Vec<Board>> {
    let mut boards: Vec<Board> = serde_json::from_slice(json).context("unexpected `pio boards` output")?;
    boards.retain(|b| !b.id.is_empty());
    boards.sort_by(|a, b| {
        a.platform.cmp(&b.platform).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
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

    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(true) // honour HTTPS_PROXY / ALL_PROXY / NO_PROXY
        .user_agent(concat!("pio-tui/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(15))
        .build();
    let resp: Value = agent
        .get(REGISTRY_SEARCH)
        .query("query", &q)
        .query("page", "1")
        .query("limit", "50")
        .query("sort", sort)
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => anyhow!("registry returned HTTP {code}"),
            ureq::Error::Transport(t) => anyhow!("cannot reach the PlatformIO registry ({t}) — offline or behind a proxy?"),
        })?
        .into_json()
        .context("invalid registry response")?;
    Ok(parse_search(&resp))
}

pub fn parse_search(resp: &Value) -> (Vec<Library>, u64) {
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
                .filter(|l| !l.name.is_empty() && !l.owner.is_empty())
                .collect()
        })
        .unwrap_or_default();
    (items, total)
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

    /// Starter source for frameworks whose entry point we have verified
    /// against PlatformIO. Everything else gets no template rather than a
    /// guess that might not build.
    pub fn template(&self) -> Option<(&'static str, String)> {
        let baud = self.monitor_speed.trim();
        let baud = if baud.is_empty() { "115200" } else { baud };
        match self.framework.as_deref()? {
            "arduino" => Some((
                "main.cpp",
                format!(
                    "#include <Arduino.h>\n\nvoid setup() {{\n  Serial.begin({baud});\n  Serial.println(\"Hello from {name}!\");\n}}\n\nvoid loop() {{\n  delay(1000);\n}}\n",
                    name = self.name
                ),
            )),
            // PlatformIO's espidf builder generates the CMakeLists.txt files
            // itself and expects `app_main` in src/ (espidf.py:create_default_project_files).
            "espidf" => Some((
                "main.c",
                format!(
                    "#include <stdio.h>\n#include \"freertos/FreeRTOS.h\"\n#include \"freertos/task.h\"\n\nvoid app_main(void)\n{{\n    printf(\"Hello from {}!\\n\");\n    while (1) {{\n        vTaskDelay(pdMS_TO_TICKS(1000));\n    }}\n}}\n",
                    self.name
                ),
            )),
            _ => None,
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
    pub log_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Failure {
    pub message: String,
    pub cancelled: bool,
    pub log_path: Option<PathBuf>,
    /// A directory this job created from scratch and left incomplete. Only
    /// ever set when the target did not exist or was empty before the job
    /// started, so deleting it can never remove user data.
    pub partial_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub enum JobMsg {
    Steps(Vec<String>),
    Step(usize, StepState),
    Log(String),
    Done(Result<Summary, Failure>),
}

/// Shared cancellation flag; flip it to stop a running job.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled by user")
    }
}
impl std::error::Error for Cancelled {}

/// Destination for log lines: the UI channel and a log file on disk.
#[derive(Clone)]
struct LogSink {
    tx: Sender<JobMsg>,
    file: Option<Arc<Mutex<BufWriter<File>>>>,
}

impl LogSink {
    fn line(&self, s: String) {
        if let Some(f) = &self.file {
            if let Ok(mut f) = f.lock() {
                let _ = writeln!(f, "{s}");
            }
        }
        let _ = self.tx.send(JobMsg::Log(s));
    }
    fn flush(&self) {
        if let Some(f) = &self.file {
            if let Ok(mut f) = f.lock() {
                let _ = f.flush();
            }
        }
    }
}

fn open_log(label: &str) -> Option<(PathBuf, File)> {
    let dir = app_cache_dir()?.join("logs");
    fs::create_dir_all(&dir).ok()?;
    // Prune old logs, keeping the newest KEEP_LOGS - 1 plus the one we create.
    if let Ok(rd) = fs::read_dir(&dir) {
        let mut logs: Vec<PathBuf> =
            rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "log")).collect();
        logs.sort();
        let excess = logs.len().saturating_sub(KEEP_LOGS - 1);
        for p in logs.into_iter().take(excess) {
            let _ = fs::remove_file(p);
        }
    }
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let safe: String = label.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let path = dir.join(format!("{ts}-{safe}.log"));
    let file = File::create(&path).ok()?;
    Some((path, file))
}

struct Job {
    log: LogSink,
    tx: Sender<JobMsg>,
    cancel: Cancel,
    next: usize,
    total: usize,
    log_path: Option<PathBuf>,
}

impl Job {
    fn new(tx: Sender<JobMsg>, cancel: Cancel, label: &str, steps: &[&str]) -> Self {
        let _ = tx.send(JobMsg::Steps(steps.iter().map(|s| s.to_string()).collect()));
        let (log_path, file) = match open_log(label) {
            Some((p, f)) => (Some(p), Some(Arc::new(Mutex::new(BufWriter::new(f))))),
            None => (None, None),
        };
        let job = Self { log: LogSink { tx: tx.clone(), file }, tx, cancel, next: 0, total: steps.len(), log_path };
        job.log(format!("pio-tui {} · steps: {}", env!("CARGO_PKG_VERSION"), steps.join(" → ")));
        job
    }

    fn log(&self, s: impl Into<String>) {
        self.log.line(s.into());
    }

    fn step(&mut self, f: impl FnOnce(&Job) -> Result<()>) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let i = self.next;
        self.next += 1;
        let _ = self.tx.send(JobMsg::Step(i, StepState::Running));
        let r = f(self);
        let state = if r.is_ok() { StepState::Ok } else { StepState::Failed };
        let _ = self.tx.send(JobMsg::Step(i, state));
        r
    }

    fn finish(self, r: Result<Summary>, partial_dir: Option<PathBuf>) {
        let msg = match r {
            Ok(mut s) => {
                self.log("✓ finished successfully");
                s.log_path = self.log_path.clone();
                Ok(s)
            }
            Err(e) => {
                for i in self.next..self.total {
                    let _ = self.tx.send(JobMsg::Step(i, StepState::Skipped));
                }
                let cancelled = e.is::<Cancelled>() || self.cancel.is_cancelled();
                let message = if cancelled { "Cancelled — PlatformIO was stopped.".to_owned() } else { format!("{e:#}") };
                self.log(format!("✗ {message}"));
                Err(Failure { message, cancelled, log_path: self.log_path.clone(), partial_dir })
            }
        };
        self.log.flush();
        let _ = self.tx.send(JobMsg::Done(msg));
    }

    /// Run a command, streaming stdout/stderr into the log. Polls for
    /// cancellation and, if requested, stops the whole process tree.
    fn run(&self, cmd: &mut Command) -> Result<()> {
        let shown = std::iter::once(
            Path::new(cmd.get_program()).file_name().unwrap_or_default().to_string_lossy().into_owned(),
        )
        .chain(cmd.get_args().map(|a| shell_quote(&a.to_string_lossy())))
        .collect::<Vec<_>>()
        .join(" ");
        self.log(format!("$ {shown}"));

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PLATFORMIO_DISABLE_PROGRESSBAR", "true")
            .env("PYTHONUNBUFFERED", "1");
        #[cfg(unix)]
        {
            // Own process group, so cancelling can stop pio *and* the scons /
            // compiler processes it spawns.
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().with_context(|| format!("failed to start `{shown}`"))?;
        let out = child.stdout.take().map(|r| pump(r, self.log.clone()));
        let err = child.stderr.take().map(|r| pump(r, self.log.clone()));

        let status = loop {
            if let Some(st) = child.try_wait()? {
                break Some(st);
            }
            if self.cancel.is_cancelled() {
                terminate(&mut child);
                break None;
            }
            thread::sleep(Duration::from_millis(40));
        };
        for h in [out, err].into_iter().flatten() {
            let _ = h.join();
        }
        match status {
            None => Err(Cancelled.into()),
            Some(st) if st.success() => Ok(()),
            Some(st) => bail!("`{shown}` exited with {}", st.code().map_or("a signal".into(), |c| format!("code {c}"))),
        }
    }
}

/// Stop a child and its process group: SIGTERM, then SIGKILL after a grace period.
fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        let pgid = child.id() as libc::pid_t;
        // SAFETY: plain syscall; a negative pid addresses the process group we created.
        unsafe { libc::kill(-pgid, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                // Leader gone; make sure no stragglers survive in the group.
                unsafe { libc::kill(-pgid, libc::SIGKILL) };
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn shell_quote(s: &str) -> String {
    if s.chars().any(|c| c.is_whitespace() || c == '\'' || c == '"') {
        format!("'{}'", s.replace('\'', r"'\''"))
    } else {
        s.to_owned()
    }
}

fn pump<R: Read + Send + 'static>(mut r: R, sink: LogSink) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut line = Vec::new();
        let flush = |line: &mut Vec<u8>| {
            let s = strip_ansi(&String::from_utf8_lossy(line));
            if !s.trim().is_empty() {
                sink.line(s.trim_end().to_owned());
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

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            match it.peek() {
                // CSI: ESC [ … final byte in @..~
                Some('[') => {
                    it.next();
                    for c in it.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] … BEL or ESC \
                Some(']') => {
                    it.next();
                    while let Some(c) = it.next() {
                        if c == '\x07' || (c == '\x1b' && it.peek() == Some(&'\\')) {
                            if c == '\x1b' {
                                it.next();
                            }
                            break;
                        }
                    }
                }
                // Charset designation: ESC ( B and friends (three bytes)
                Some('(' | ')' | '*' | '+') => {
                    it.next();
                    it.next();
                }
                // Two-byte escape (ESC + one char)
                Some(_) => {
                    it.next();
                }
                None => {}
            }
        } else if c == '\t' {
            out.push_str("    ");
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

fn dir_is_missing_or_empty(dir: &Path) -> bool {
    match fs::read_dir(dir) {
        Ok(mut rd) => rd.next().is_none(),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Scaffold a brand-new project in a background thread.
pub fn spawn_create(spec: ProjectSpec, cancel: Cancel, tx: Sender<JobMsg>) -> JoinHandle<()> {
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
        let dir = spec.path();
        let mut job = Job::new(tx, cancel, &spec.name, &names);
        let pio = pio_bin();
        // Only a directory we create from nothing may be offered for deletion.
        let owned = dir_is_missing_or_empty(&dir);

        let r = (|| -> Result<Summary> {
            if !owned {
                bail!("{} already exists and is not empty", dir.display());
            }
            job.step(|j| {
                fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
                // Without -b nothing is installed; this only lays out the
                // standard folders. It must run *before* we write our ini.
                j.run(Command::new(&pio).args(["project", "init", "-d"]).arg(&dir))
            })?;
            job.step(|j| {
                fs::write(dir.join("platformio.ini"), spec.ini())?;
                ensure_gitignore(&dir)?;
                j.log("wrote platformio.ini and .gitignore");
                Ok(())
            })?;
            if spec.template {
                job.step(|j| {
                    let src = dir.join("src");
                    fs::create_dir_all(&src)?;
                    let empty = fs::read_dir(&src)?.flatten().all(|e| e.file_name().to_string_lossy().starts_with('.'));
                    match spec.template() {
                        _ if !empty => j.log("src/ is not empty — leaving it untouched"),
                        Some((file, body)) => {
                            fs::write(src.join(file), body)?;
                            j.log(format!("wrote src/{file}"));
                        }
                        None => j.log(format!(
                            "no verified starter for framework `{}` — src/ left empty",
                            spec.framework.as_deref().unwrap_or("none")
                        )),
                    }
                    Ok(())
                })?;
            }
            job.step(|j| j.run(Command::new(&pio).args(["pkg", "install", "-d"]).arg(&dir)))?;
            if spec.compiledb {
                job.step(|j| {
                    j.run(Command::new(&pio).args(["run", "-t", "compiledb", "-d"]).arg(&dir))?;
                    resolve_compilers_step(&dir, j);
                    Ok(())
                })?;
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
            Ok(Summary { cdb: cdb_info(&dir), project: dir.clone(), log_path: None })
        })();
        let partial = (r.is_err() && owned && dir.exists()).then(|| dir.clone());
        job.finish(r, partial);
    })
}

/// Generate `compile_commands.json` for an existing project. Never touches
/// the project's other files (`.clangd` is only written when asked and absent).
pub fn spawn_compiledb(dir: PathBuf, env: Option<String>, clangd: bool, cancel: Cancel, tx: Sender<JobMsg>) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut names = vec!["Generate compile_commands.json"];
        if clangd {
            names.push("Write .clangd config");
        }
        let label = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "compiledb".into());
        let mut job = Job::new(tx, cancel, &label, &names);
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
                j.run(&mut cmd)?;
                resolve_compilers_step(&dir, j);
                Ok(())
            })?;
            if clangd {
                job.step(|j| write_clangd(&dir, j))?;
            }
            Ok(Summary { cdb: cdb_info(&dir), project: dir.clone(), log_path: None })
        })();
        job.finish(r, None);
    })
}

pub const CLANGD: &str = r#"# Generated by pio-tui — makes clangd happy with PlatformIO's GCC cross toolchains.
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
    Ok(())
}

/// Only used for projects pio-tui creates; existing projects are never modified.
pub fn ensure_gitignore(dir: &Path) -> Result<()> {
    let p = dir.join(".gitignore");
    let mut cur = match fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).context("cannot read .gitignore"),
    };
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

/// Some platforms (e.g. espressif32) write the compiler into
/// compile_commands.json as a bare name like `xtensa-esp32-elf-g++`, relying
/// on PlatformIO's private PATH. Tools reading the database (clangd's
/// `--query-driver`) can't find it, so rewrite bare names to the absolute
/// path of the matching toolchain binary under `<packages>/*/bin/`.
/// Returns how many entries were rewritten. Names that can't be resolved
/// unambiguously are left untouched.
pub fn absolutize_compilers(dir: &Path, packages: &Path) -> Result<usize> {
    use std::collections::HashMap;
    let path = dir.join("compile_commands.json");
    let mut db: Vec<Value> = serde_json::from_slice(&fs::read(&path)?).context("invalid compile_commands.json")?;
    let mut cache: HashMap<String, Option<String>> = HashMap::new();
    let mut resolve = |name: &str| -> Option<String> {
        cache
            .entry(name.to_owned())
            .or_insert_with(|| {
                let hits: Vec<PathBuf> = fs::read_dir(packages)
                    .ok()?
                    .flatten()
                    .map(|e| e.path().join("bin").join(name))
                    .filter(|p| p.is_file())
                    .collect();
                // Several toolchain versions installed side by side: ambiguous, skip.
                (hits.len() == 1).then(|| hits[0].to_string_lossy().into_owned())
            })
            .clone()
    };
    let is_bare = |s: &str| !s.is_empty() && !s.contains('/') && !s.contains('\\');
    let mut changed = 0;
    for e in &mut db {
        if let Some(args) = e.get_mut("arguments").and_then(Value::as_array_mut) {
            if let Some(first) = args.first_mut() {
                if let Some(abs) = first.as_str().filter(|s| is_bare(s)).and_then(&mut resolve) {
                    *first = Value::String(abs);
                    changed += 1;
                }
            }
        } else if let Some(cmd) = e.get("command").and_then(Value::as_str) {
            let trimmed = cmd.trim_start();
            let token = trimmed.split_whitespace().next().unwrap_or("");
            if is_bare(token) {
                if let Some(abs) = resolve(token) {
                    let quoted = if abs.contains(char::is_whitespace) { format!("\"{abs}\"") } else { abs };
                    let new = format!("{quoted}{}", &trimmed[token.len()..]);
                    e["command"] = Value::String(new);
                    changed += 1;
                }
            }
        }
    }
    if changed > 0 {
        write_atomic(&path, &serde_json::to_vec_pretty(&db)?)?;
    }
    Ok(changed)
}

fn resolve_compilers_step(dir: &Path, j: &Job) {
    let Some(packages) = platformio_home().map(|h| h.join("packages")) else { return };
    match absolutize_compilers(dir, &packages) {
        Ok(0) => {}
        Ok(n) => j.log(format!("resolved toolchain path in {n} compile command(s)")),
        // Not fatal: the database is still usable, just without absolute paths.
        Err(e) => j.log(format!("note: could not post-process compile_commands.json: {e:#}")),
    }
}

/// `[env:NAME]` sections in a project's platformio.ini.
pub fn parse_envs(dir: &Path) -> Option<Vec<String>> {
    Some(parse_envs_str(&fs::read_to_string(dir.join("platformio.ini")).ok()?))
}

pub fn parse_envs_str(ini: &str) -> Vec<String> {
    ini.lines()
        .filter_map(|l| {
            let l = l.split([';', '#']).next()?.trim();
            l.strip_prefix("[env:")?.strip_suffix(']').map(|s| s.trim().to_owned())
        })
        .filter(|s| !s.is_empty())
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pio-tui-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn board() -> Board {
        parse_boards(br#"[{"id":"esp32dev","name":"Espressif ESP32 Dev Module","platform":"espressif32",
            "mcu":"ESP32","fcpu":240000000,"ram":327680,"rom":4194304,"frameworks":["arduino","espidf"],
            "vendor":"Espressif","url":"https://x"}]"#)
        .unwrap()
        .remove(0)
    }

    fn spec(framework: Option<&str>) -> ProjectSpec {
        ProjectSpec {
            name: "demo".into(),
            parent: "/tmp".into(),
            board: board(),
            framework: framework.map(Into::into),
            libs: vec!["bblanchon/ArduinoJson@^7.4.3".into()],
            monitor_speed: "115200".into(),
            upload_port: String::new(),
            compiledb: true,
            clangd: true,
            template: true,
            git: false,
        }
    }

    #[test]
    fn board_json_tolerates_odd_types() {
        let b = parse_boards(
            br#"[{"id":"b","fcpu":"16000000L","ram":2048.0,"rom":null,"name":null,"platform":"p"},
                 {"id":"","platform":"p"},
                 {"id":"a","platform":"a"}]"#,
        )
        .unwrap();
        assert_eq!(b.len(), 2, "entries without an id are dropped");
        assert_eq!(b[0].id, "a", "sorted by platform");
        assert_eq!(b[1].fcpu, 16_000_000);
        assert_eq!(b[1].ram, 2048);
        assert_eq!(b[1].rom, 0);
        assert_eq!(b[1].name, "");
    }

    #[test]
    fn ini_contains_expected_options() {
        let ini = spec(Some("arduino")).ini();
        assert!(ini.contains("[platformio]\ndefault_envs = esp32dev\n"));
        assert!(ini.contains("[env:esp32dev]\nplatform = espressif32\nboard = esp32dev\nframework = arduino\n"));
        assert!(ini.contains("monitor_speed = 115200\n"));
        assert!(ini.contains("monitor_filters = esp32_exception_decoder\n"));
        assert!(ini.contains("lib_deps =\n    bblanchon/ArduinoJson@^7.4.3\n"));
        assert!(!ini.contains("upload_port"), "empty upload port is omitted");
        assert_eq!(parse_envs_str(&ini), vec!["esp32dev"]);
    }

    #[test]
    fn ini_without_framework_or_libs() {
        let mut s = spec(None);
        s.libs.clear();
        s.upload_port = " /dev/ttyUSB0 ".into();
        let ini = s.ini();
        assert!(!ini.contains("framework"));
        assert!(!ini.contains("lib_deps"));
        assert!(ini.contains("upload_port = /dev/ttyUSB0\n"));
    }

    #[test]
    fn templates_only_for_verified_frameworks() {
        let (f, body) = spec(Some("arduino")).template().unwrap();
        assert_eq!(f, "main.cpp");
        assert!(body.contains("Serial.begin(115200)"));
        let (f, body) = spec(Some("espidf")).template().unwrap();
        assert_eq!(f, "main.c");
        assert!(body.contains("void app_main(void)"));
        for fw in ["zephyr", "stm32cube", "cmsis", "mbed", "libopencm3"] {
            assert!(spec(Some(fw)).template().is_none(), "{fw} should not get a guessed template");
        }
        assert!(spec(None).template().is_none());
    }

    #[test]
    fn library_specs() {
        let lib = |v: &str| Library {
            owner: "o".into(),
            name: "n".into(),
            description: String::new(),
            version: v.into(),
            keywords: vec![],
            stars: 0,
            license: String::new(),
        };
        assert_eq!(lib("1.2.3").spec(), "o/n@^1.2.3");
        assert_eq!(lib("").spec(), "o/n");
        assert_eq!(lib("0.0.0+sha.36b69a7").spec(), "o/n");
        assert_eq!(lib("0.0.0-alpha").spec(), "o/n");
        assert_eq!(lib("1.0.0").key(), "o/n");
    }

    #[test]
    fn registry_response_parsing() {
        let v: Value = serde_json::from_str(
            r#"{"total":440,"items":[
                {"owner":{"username":"bblanchon"},"name":"ArduinoJson","description":"A  JSON\nlibrary",
                 "version":{"name":"7.4.3"},"keywords":["json"],"stars_count":7124,"license":"MIT"},
                {"owner":{"username":"x"},"name":""},
                {"name":"no-owner"}]}"#,
        )
        .unwrap();
        let (items, total) = parse_search(&v);
        assert_eq!(total, 440);
        assert_eq!(items.len(), 1, "malformed items are skipped");
        assert_eq!(items[0].spec(), "bblanchon/ArduinoJson@^7.4.3");
        assert_eq!(items[0].description, "A JSON library", "whitespace is normalised");
        let (none, zero) = parse_search(&Value::Null);
        assert!(none.is_empty() && zero == 0);
    }

    #[test]
    fn env_parsing_ignores_comments_and_other_sections() {
        let ini = "[platformio]\n[env]\n[env:a] ; comment\n  [env: b ]\n;[env:c]\n[env:]\n";
        assert_eq!(parse_envs_str(ini), vec!["a", "b"]);
    }

    #[test]
    fn strip_ansi_sequences() {
        assert_eq!(strip_ansi("\x1b[1;32mOK\x1b[0m done"), "OK done");
        assert_eq!(strip_ansi("\x1b]0;title\x07text"), "text");
        assert_eq!(strip_ansi("a\tb\x08"), "a    b");
        assert_eq!(strip_ansi("\x1b(Bplain"), "plain");
        assert_eq!(strip_ansi("\x1b7saved\x1b8"), "saved");
    }

    #[test]
    fn gitignore_is_idempotent_and_preserves_content() {
        let d = tmpdir("gitignore");
        fs::write(d.join(".gitignore"), "build/").unwrap();
        ensure_gitignore(&d).unwrap();
        ensure_gitignore(&d).unwrap();
        let s = fs::read_to_string(d.join(".gitignore")).unwrap();
        assert_eq!(s, "build/\n.pio\n.cache/\ncompile_commands.json\n");
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn cdb_info_reads_both_formats() {
        let d = tmpdir("cdb");
        fs::write(d.join("compile_commands.json"), r#"[{"command":"/tc/bin/avr-g++ -c a.cpp"},{"arguments":["x"]}]"#).unwrap();
        let i = cdb_info(&d).unwrap();
        assert_eq!(i.entries, 2);
        assert_eq!(i.compiler.as_deref(), Some("/tc/bin/avr-g++"));
        fs::write(d.join("compile_commands.json"), r#"[{"arguments":["/tc/gcc","-c"]}]"#).unwrap();
        assert_eq!(cdb_info(&d).unwrap().compiler.as_deref(), Some("/tc/gcc"));
        fs::write(d.join("compile_commands.json"), "not json").unwrap();
        assert!(cdb_info(&d).is_none());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn bare_compiler_names_are_resolved() {
        let d = tmpdir("abs");
        let pk = d.join("packages");
        fs::create_dir_all(pk.join("toolchain-xtensa-esp32/bin")).unwrap();
        fs::write(pk.join("toolchain-xtensa-esp32/bin/xtensa-esp32-elf-g++"), "").unwrap();
        // Ambiguous: two toolchains provide the same name.
        for t in ["toolchain-a", "toolchain-b"] {
            fs::create_dir_all(pk.join(t).join("bin")).unwrap();
            fs::write(pk.join(t).join("bin/dup-gcc"), "").unwrap();
        }
        let proj = d.join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("compile_commands.json"),
            r#"[{"command":"xtensa-esp32-elf-g++ -o a.o -c a.cpp","file":"a.cpp"},
                {"arguments":["xtensa-esp32-elf-g++","-c","b.cpp"],"file":"b.cpp"},
                {"command":"/abs/avr-g++ -c c.cpp"},
                {"command":"dup-gcc -c d.c"},
                {"command":"unknown-gcc -c e.c"}]"#,
        )
        .unwrap();
        assert_eq!(absolutize_compilers(&proj, &pk).unwrap(), 2);
        let db: Vec<Value> = serde_json::from_slice(&fs::read(proj.join("compile_commands.json")).unwrap()).unwrap();
        let tc = pk.join("toolchain-xtensa-esp32/bin/xtensa-esp32-elf-g++").to_string_lossy().into_owned();
        assert_eq!(db[0]["command"], format!("{tc} -o a.o -c a.cpp"));
        assert_eq!(db[0]["file"], "a.cpp", "other fields preserved");
        assert_eq!(db[1]["arguments"][0], tc.as_str());
        assert_eq!(db[2]["command"], "/abs/avr-g++ -c c.cpp", "absolute paths untouched");
        assert_eq!(db[3]["command"], "dup-gcc -c d.c", "ambiguous names untouched");
        assert_eq!(db[4]["command"], "unknown-gcc -c e.c", "unknown names untouched");
        // Second run: nothing left to do, file not rewritten.
        assert_eq!(absolutize_compilers(&proj, &pk).unwrap(), 0);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn atomic_write_replaces_file() {
        let d = tmpdir("atomic");
        let p = d.join("f.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1, "no temp files left behind");
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn missing_or_empty_dir_detection() {
        let d = tmpdir("owned");
        assert!(dir_is_missing_or_empty(&d));
        assert!(dir_is_missing_or_empty(&d.join("nope")));
        fs::write(d.join("x"), "").unwrap();
        assert!(!dir_is_missing_or_empty(&d));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_hz(240_000_000), "240 MHz");
        assert_eq!(fmt_hz(16_500_000), "16.5 MHz");
        assert_eq!(fmt_hz(32_768), "32.8 kHz");
        assert_eq!(fmt_hz(0), "—");
        assert_eq!(fmt_bytes(327_680), "320 KB");
        assert_eq!(fmt_bytes(4_194_304), "4 MB");
        assert_eq!(fmt_bytes(32_256), "31.5 KB");
        assert_eq!(fmt_bytes(512), "512 B");
    }
}
