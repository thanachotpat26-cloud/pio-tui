mod app;
mod input;
mod pio;
mod theme;
mod ui;

use std::{
    path::PathBuf,
    process::ExitCode,
    sync::mpsc,
    time::Duration,
};

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::event::{self, Event, KeyEventKind};

/// A fast, friendly TUI for PlatformIO: scaffold projects and keep
/// compile_commands.json fresh for clangd.
#[derive(Parser)]
#[command(name = "pio-tui", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,

    /// Re-query `pio boards` instead of using the cached catalogue
    #[arg(long, global = true)]
    refresh: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Jump straight into the new-project wizard
    New,
    /// Generate compile_commands.json (+ .clangd) for a project, no TUI
    #[command(alias = "cdb")]
    Compiledb {
        /// Project directory (defaults to the current directory)
        dir: Option<PathBuf>,
        /// Only this environment
        #[arg(short, long)]
        env: Option<String>,
        /// Don't write a .clangd file
        #[arg(long)]
        no_clangd: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Some(Cmd::Compiledb { dir, env, no_clangd }) = cli.command {
        return headless_compiledb(dir, env, !no_clangd);
    }

    let mut app = app::App::new(matches!(cli.command, Some(Cmd::New)), cli.refresh);
    let mut terminal = ratatui::init();
    let res = run(&mut terminal, &mut app);
    ratatui::restore();

    match res {
        Ok(()) => {
            if let Some(note) = app.exit_note {
                println!("\x1b[38;2;255;138;51m◆\x1b[0m Project ready. Jump in with:\n\n    {note}\n");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut app::App) -> Result<()> {
    while !app.quit {
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(50))? {
            // Drain everything queued so fast typing never lags behind rendering.
            loop {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press {
                        app.on_key(k);
                    }
                }
                if app.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        app.tick();
    }
    Ok(())
}

fn headless_compiledb(dir: Option<PathBuf>, env: Option<String>, clangd: bool) -> ExitCode {
    const O: &str = "\x1b[38;2;255;138;51m";
    const G: &str = "\x1b[38;2;126;214;126m";
    const R: &str = "\x1b[38;2;240;108;108m";
    const D: &str = "\x1b[2m";
    const X: &str = "\x1b[0m";

    let dir = dir.unwrap_or_else(|| PathBuf::from("."));
    let dir = dir.canonicalize().unwrap_or(dir);
    let (tx, rx) = mpsc::channel();
    pio::spawn_compiledb(dir, env, clangd, tx);
    let mut steps = Vec::new();
    for msg in rx {
        match msg {
            pio::JobMsg::Steps(s) => steps = s,
            pio::JobMsg::Step(i, st) => {
                let name = steps.get(i).map(String::as_str).unwrap_or("");
                match st {
                    pio::StepState::Running => println!("{O}▸{X} {name}"),
                    pio::StepState::Ok => println!("{G}✓{X} {name}"),
                    pio::StepState::Failed => println!("{R}✗{X} {name}"),
                    _ => {}
                }
            }
            pio::JobMsg::Log(l) => println!("{D}  │ {l}{X}"),
            pio::JobMsg::Done(Ok(s)) => {
                if let Some(c) = s.cdb {
                    println!("\n{G}◆{X} compile_commands.json ready ({} entries)", c.entries);
                    if let Some(cc) = c.compiler {
                        if let Some(d) = std::path::Path::new(&cc).parent() {
                            println!("  clangd: --query-driver={}/*", d.display());
                        }
                    }
                }
                return ExitCode::SUCCESS;
            }
            pio::JobMsg::Done(Err(e)) => {
                eprintln!("\n{R}error:{X} {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::FAILURE
}
