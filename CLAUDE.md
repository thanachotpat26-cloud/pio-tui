# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`pio-tui` is a Rust (edition 2024) terminal UI that wraps PlatformIO Core (`pio`). It has two jobs:

- a new-project wizard (platform → board → framework → registry libraries → options → review)
- generating `compile_commands.json` plus a `.clangd` file so clangd works with PlatformIO's GCC cross-toolchains

## Commands

```sh
cargo build                      # debug build
cargo build --release            # LTO + strip + panic=abort (see [profile.release])
cargo run                        # TUI home screen
cargo run -- new                 # straight into the wizard
cargo run -- compiledb <DIR> [-e ENV] [--no-clangd]   # headless, no TUI; good for quick backend checks
cargo run -- --refresh           # bypass the board-catalogue cache
cargo test                       # unit tests + tests/cli.rs (fake `pio` on PATH, no PlatformIO needed)
cargo test -- --ignored          # also tests that use the real PlatformIO install
cargo test --test cli ctrl_c     # a single test by name filter
```

CI (`.github/workflows/ci.yml`) builds and tests on Ubuntu and macOS; clippy there is advisory. `cargo clippy` and `rustfmt` aren't installed in this local environment, so the code is not rustfmt-formatted.

`tests/cli.rs` drives the real binary with an isolated `HOME`/`XDG_CACHE_HOME` and a fake `pio` shell script prepended to `PATH`, which `pio::pio_bin()` picks up. Use that pattern for any new job behaviour. Running the TUI needs a real TTY. To check rendering headlessly, launch the release binary in `tmux new-session -d -x 140 -y 40 ...`, then drive it with `tmux send-keys` and read the screen with `tmux capture-pane -p`.

Runtime needs: `pio` on `$PATH`, or at `$PLATFORMIO_CORE_DIR`/`~/.platformio/penv/bin/pio`. Library search also needs network access to `api.registry.platformio.org`.

## Architecture

There's no async runtime. The program is a single-threaded render/input loop, with background work done on `std::thread` workers that report back over `mpsc` channels.

- **`main.rs`**: clap CLI and the event loop: draw → poll keys (50 ms, draining every queued key before the next draw) → `app.tick()`. The `compiledb` subcommand skips the TUI and consumes the same `JobMsg` stream, printing it with ANSI colours.
- **`app.rs`** (`App`): all state and key handling, organised as a state machine. `Screen` is one of `Home | Wizard | Compiledb | Running`, and inside `Wizard` there's a `Step` enum.
  - `tick()` checks three kinds of channel without blocking: the board-catalogue load, library-search results, and the running job's `JobMsg`s.
  - `confirm()` holds the "continue" logic for each step. It's shared by Enter and `Ctrl+N`. (In the Libraries step, Enter adds/removes a library instead.)
  - Library search is debounced (`SEARCH_DEBOUNCE`). Each request carries a sequence number (`lib_seq`), and responses from older requests are dropped.
  - Fuzzy filtering uses `fuzzy-matcher` (Skim). An exact or prefix match on the board ID gets a score bonus.
- **`ui.rs`**: pure rendering from `&mut App`. It needs `&mut` only for ratatui `ListState`s; list items are built as owned strings first so the borrows don't conflict. Helpers: `panel`, `draw_input`, `hints` (footer key bar), `center`.
- **`pio.rs`**: everything that touches PlatformIO.
  - Board catalogue: `pio boards --json-output`, cached at `~/.cache/pio-tui/boards.json` and refreshed after 7 days. The `de_str`/`de_u64` helpers tolerate inconsistent field types in that JSON.
  - Registry search goes through `ureq` (blocking, run on a worker thread).
  - `ProjectSpec::ini()` / `template()` produce the generated `platformio.ini` and starter `src/main.*` file.
  - Jobs: `spawn_create` / `spawn_compiledb` take a `Cancel` token, return a `JoinHandle`, and send `JobMsg::{Steps, Step, Log, Done}`. `Done` carries `Result<Summary, Failure>`. The `Job` helper numbers steps automatically, and if a step fails it marks every remaining step `Skipped`.
  - `Job::run` streams child stdout/stderr line by line, splitting on `\r` and `\n` and stripping ANSI codes, through a `LogSink` that also writes to `~/.cache/pio-tui/logs/`. It sets `PLATFORMIO_DISABLE_PROGRESSBAR`.
  - Cancellation: on Unix every child runs in its own process group (`process_group(0)`). `Job::run` polls `try_wait` and the `Cancel` flag, and `terminate()` sends SIGTERM and then SIGKILL to the whole group, so scons and gcc children die too. Because of the process group, the terminal's SIGINT never reaches pio: the TUI maps Ctrl+C/Esc to `cancel_job()`, headless mode forwards it via `ctrlc` (exit code 130), and `App::shutdown()` runs before the terminal is restored.
  - `Failure::partial_dir` is set only when the target directory was missing or empty before the job started. That invariant is what makes the "delete partial project" key safe, so keep it.
- **`input.rs`**: a single-line text input with readline-style keys. `handle()` returns `true` only when the text changed; callers use that to re-filter or re-search.
- **`theme.rs`**: truecolor palette. It never sets a background, so the user's terminal theme shows through.

### PlatformIO details the code depends on

- Project creation runs `pio project init -d DIR` on an empty directory *first*, which creates the folder skeleton, and *then* overwrites `platformio.ini`. If the ini already exists, `init` won't create the folders.
- Then `pio pkg install -d DIR` (installs the platform, toolchain and libraries) and `pio run -t compiledb -d DIR`.
- Library specs are written as `owner/name@^version`, except snapshot versions (`0.0.0…` or anything containing `+`), which are written as plain `owner/name`.
- The registry's `platform:` filter is loose: libraries that declare all platforms still appear. That's expected registry behaviour.
- After `compiledb`, `absolutize_compilers` rewrites bare compiler names (espressif32/8266 emit e.g. `xtensa-esp32-elf-g++`) to the unique match under `~/.platformio/packages/*/bin/`. Ambiguous or unknown names are left alone.
- Existing projects (the `compiledb` path) must not be modified beyond `compile_commands.json` and an opt-in `.clangd`. `ensure_gitignore` is called only for projects pio-tui creates.
- `ProjectSpec::template()` returns starters only for frameworks verified against PlatformIO (arduino, espidf). Return `None` rather than guess.
- `write_clangd` never overwrites an existing `.clangd`. Users still need clangd's `--query-driver=<toolchain>/bin/*` for system headers, and the UI and CLI print that path after a run.

Adding a wizard step means touching four places: `Step::ALL`/`title`, `App::confirm`, `App::key_wizard`, and `ui::draw_wizard` plus the footer hints in `draw_footer`.
