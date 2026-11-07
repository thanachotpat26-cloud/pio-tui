# pio-tui

A fast, friendly terminal UI for [PlatformIO](https://platformio.org) written in Rust.

- **New project wizard**: Project → Platform → Board → Framework → Libraries → Options → Review
  - fuzzy search across every board PlatformIO knows about (~1,600), with MCU, clock, RAM and flash details
  - live PlatformIO Registry search for `lib_deps`, filtered to your platform (`Ctrl+F` toggles)
  - writes a clean `platformio.ini`, starter `src/main.*`, `.gitignore`, and optionally runs `git init`
  - installs the platform, toolchain and libraries, with streamed output
- **compile_commands.json** via `pio run -t compiledb`, plus a `.clangd` that strips GCC-only
  cross-compiler flags (`-mlongcalls`, `-fstrict-volatile-bitfields`, …) that clang rejects
- **Fast**: the board catalogue is cached (`~/.cache/pio-tui/boards.json`, refreshed weekly), and all
  PlatformIO and network work runs on background threads, so the UI never blocks

## Install

```sh
cargo install --path .
```

## Usage

```sh
pio-tui                      # home screen
pio-tui new                  # straight into the wizard
pio-tui compiledb [DIR]      # headless: regenerate compile_commands.json (+ .clangd)
pio-tui compiledb -e esp32dev --no-clangd
pio-tui --refresh            # re-query `pio boards`
```

### clangd

For full system-header resolution with cross toolchains, start clangd with the `--query-driver`
flag that pio-tui prints after generating the database, e.g.

```
--query-driver=/home/you/.platformio/packages/toolchain-xtensa-esp32/bin/*
```

## Keys

| Where | Keys |
|---|---|
| Everywhere | `Ctrl+C` quit |
| Wizard | `Enter` select / continue · `Esc` back · `Ctrl+N` continue · type to filter |
| Libraries | `Enter` add/remove · `Tab` switch to selected list · `Del` remove · `Ctrl+F` platform filter |
| Options | `↑↓` move · `Space` toggle |
| Output | `↑↓` / `PgUp` / `PgDn` scroll · `End` follow |
