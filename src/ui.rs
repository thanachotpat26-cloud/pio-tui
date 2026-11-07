//! All rendering.

use ratatui::{
    layout::{Constraint, Flex, Layout, Position, Rect},
    prelude::*,
    widgets::{Block, BorderType, Clear, List, ListItem, Padding, Paragraph, Wrap},
};

use crate::{
    app::{App, HOME_ITEMS, OPTION_ROWS, Screen, Step},
    input::Input,
    pio::{self, StepState},
    theme::*,
};

const LOGO: [&str; 6] = [
    "██████╗ ██╗ ██████╗     ████████╗██╗   ██╗██╗",
    "██╔══██╗██║██╔═══██╗    ╚══██╔══╝██║   ██║██║",
    "██████╔╝██║██║   ██║       ██║   ██║   ██║██║",
    "██╔═══╝ ██║██║   ██║       ██║   ██║   ██║██║",
    "██║     ██║╚██████╔╝       ██║   ╚██████╔╝██║",
    "╚═╝     ╚═╝ ╚═════╝        ╚═╝    ╚═════╝ ╚═╝",
];

pub fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)]).areas(f.area());
    draw_header(f, header, app);
    match app.screen {
        Screen::Home => draw_home(f, body, app),
        Screen::Wizard => draw_wizard(f, body, app),
        Screen::Compiledb => draw_cdb(f, body, app),
        Screen::Running => draw_running(f, body, app),
    }
    draw_footer(f, footer, app);
    draw_toast(f, body, app);
}

// ───────────────────────────── chrome ─────────────────────────────

fn panel(title: &str, focused: bool) -> Block<'static> {
    let color = if focused { ACCENT } else { BORDER };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title(Line::from(format!(" {title} ")).style(Style::new().fg(if focused { ACCENT } else { FG }).bold()))
        .padding(Padding::horizontal(1))
}

fn center(area: Rect, w: u16, h: u16) -> Rect {
    let [r] = Layout::horizontal([Constraint::Length(w.min(area.width))]).flex(Flex::Center).areas(area);
    let [r] = Layout::vertical([Constraint::Length(h.min(area.height))]).flex(Flex::Center).areas(r);
    r
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(BORDER));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let title = match app.screen {
        Screen::Home => "Home".to_string(),
        Screen::Wizard => "New project".to_string(),
        Screen::Compiledb => "Compilation database".to_string(),
        Screen::Running => app.run.as_ref().map(|r| r.title.clone()).unwrap_or_default(),
    };
    let left = Line::from(vec![
        " ◆ ".fg(ACCENT).bold(),
        "pio".fg(ACCENT).bold(),
        "·tui".fg(FG).bold(),
        "  │  ".fg(BORDER),
        title.fg(FG),
    ]);
    f.render_widget(Paragraph::new(left), inner);

    let right = if app.boards_loading {
        Line::from(vec![app.spinner().fg(ACCENT), " loading board catalogue ".fg(MUTED)])
    } else if let Some(e) = &app.boards_err {
        Line::from(vec!["⚠ ".fg(ERR), truncate(e, 60).fg(ERR), " ".into()])
    } else {
        Line::from(vec![
            app.boards.len().to_string().fg(ACCENT2).bold(),
            " boards · ".fg(MUTED),
            app.platforms.len().to_string().fg(ACCENT2).bold(),
            " platforms · ".fg(MUTED),
            app.installed.len().to_string().fg(OK).bold(),
            " installed ".fg(MUTED),
        ])
    };
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), inner);
}

fn hints(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![" ".into()];
    for (k, label) in pairs {
        spans.push(Span::styled(format!(" {k} "), Style::new().fg(Color::Black).bg(ACCENT).bold()));
        spans.push(Span::styled(format!(" {label}   "), Style::new().fg(MUTED)));
    }
    Line::from(spans)
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let line = match app.screen {
        Screen::Home => hints(&[("↑↓", "move"), ("⏎", "select"), ("n", "new"), ("c", "compiledb"), ("q", "quit")]),
        Screen::Compiledb => hints(&[("type", "directory"), ("↑↓", "environment"), ("Tab", ".clangd"), ("⏎", "generate"), ("Esc", "back")]),
        Screen::Running => match &app.run {
            Some(r) if r.confirm_delete => hints(&[("y", "delete folder"), ("any key", "keep it")]),
            Some(r) if r.is_running() => hints(&[("↑↓/PgUp", "scroll log"), ("End", "follow"), ("Esc/^C", "cancel")]),
            Some(r) if matches!(&r.finished, Some((Err(f), _)) if f.partial_dir.is_some()) => {
                hints(&[("d", "delete partial project"), ("h", "back"), ("⏎", "exit"), ("↑↓", "scroll log")])
            }
            _ => hints(&[("⏎", "exit"), ("h", "home"), ("↑↓", "scroll log")]),
        },
        Screen::Wizard => match app.step {
            Step::Project => hints(&[("Tab", "switch field"), ("⏎", "continue"), ("Esc", "home")]),
            Step::Platform | Step::Board => hints(&[("type", "fuzzy filter"), ("↑↓", "move"), ("⏎", "select"), ("Esc", "back")]),
            Step::Framework => hints(&[("↑↓", "move"), ("⏎", "select"), ("Esc", "back")]),
            Step::Libraries => {
                if app.lib_focus_selected {
                    hints(&[("↑↓", "move"), ("⏎/Del", "remove"), ("Tab", "results"), ("^N", "continue"), ("Esc", "back")])
                } else {
                    hints(&[("type", "search"), ("⏎", "add/remove"), ("Tab", "selected"), ("^F", "platform filter"), ("^N", "continue"), ("Esc", "back")])
                }
            }
            Step::Options => hints(&[("↑↓", "move"), ("Space", "toggle"), ("⏎", "review"), ("Esc", "back")]),
            Step::Review => hints(&[("⏎", "create project"), ("Esc", "back")]),
        },
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_toast(f: &mut Frame, area: Rect, app: &App) {
    let Some((msg, _)) = &app.toast else { return };
    let w = (unicode_width::UnicodeWidthStr::width(msg.as_str()) as u16 + 6).min(area.width);
    let r = Rect { x: area.right().saturating_sub(w + 1), y: area.bottom().saturating_sub(3), width: w, height: 3 };
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(Line::from(vec!["● ".fg(ACCENT), msg.clone().fg(FG)]))
            .block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(ACCENT))),
        r,
    );
}

fn draw_input(f: &mut Frame, area: Rect, block: Block, input: &Input, focused: bool, placeholder: &str) {
    let inner = block.inner(area);
    f.render_widget(block, area);
    let prompt = "› ";
    let (text, col) = input.view(inner.width.saturating_sub(2) as usize);
    let line = if input.is_empty() {
        Line::from(vec![prompt.fg(ACCENT), placeholder.fg(MUTED).italic()])
    } else {
        Line::from(vec![prompt.fg(ACCENT), text.fg(FG)])
    };
    f.render_widget(Paragraph::new(line), inner);
    if focused {
        f.set_cursor_position(Position::new(inner.x + 2 + col as u16, inner.y));
    }
}

/// Truncate to `n` terminal columns (not chars), appending `…` when cut.
fn truncate(s: &str, n: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    if s.width() <= n {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for g in s.graphemes(true) {
        let w = g.width();
        if used + w + 1 > n {
            break;
        }
        out.push_str(g);
        used += w;
    }
    out.push('…');
    out
}

fn kv(label: &str, value: impl Into<String>, color: Color) -> Line<'static> {
    Line::from(vec![format!("{label:<11}").fg(MUTED), Span::styled(value.into(), Style::new().fg(color))])
}

fn chips(items: &[String], color: Color) -> Vec<Span<'static>> {
    items
        .iter()
        .flat_map(|s| [Span::styled(format!(" {s} "), Style::new().fg(Color::Black).bg(color)), " ".into()])
        .collect()
}

// ───────────────────────────── home ─────────────────────────────

fn draw_home(f: &mut Frame, area: Rect, app: &mut App) {
    let menu_h = HOME_ITEMS.len() as u16 * 2 + 1;
    let r = center(area, 64, 6 + 2 + 1 + menu_h + 3);
    let [logo, tag, _, menu, desc] = Layout::vertical([
        Constraint::Length(6),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(menu_h),
        Constraint::Length(3),
    ])
    .areas(r);

    let lines: Vec<Line> =
        LOGO.iter().zip(LOGO_GRADIENT).map(|(l, c)| Line::from(l.fg(c).bold())).collect();
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), logo);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            "PlatformIO project studio".fg(FG),
            "  ·  ".fg(BORDER),
            concat!("v", env!("CARGO_PKG_VERSION")).fg(MUTED),
        ]))
        .alignment(Alignment::Center),
        tag,
    );

    let menu = center(menu, 42, menu_h);
    let items: Vec<ListItem> = HOME_ITEMS
        .iter()
        .map(|(icon, label, _)| {
            ListItem::new(vec![Line::from(vec![format!(" {icon}  ").fg(ACCENT), label.to_string().fg(FG)]), Line::raw("")])
        })
        .collect();
    let list = List::new(items).highlight_style(selected()).highlight_symbol("▌");
    f.render_stateful_widget(list, menu, &mut app.home);

    let d = HOME_ITEMS[app.home.selected().unwrap_or(0)].2;
    f.render_widget(Paragraph::new(d.fg(MUTED).italic()).alignment(Alignment::Center).wrap(Wrap { trim: true }), desc);
}

// ───────────────────────────── wizard ─────────────────────────────

fn draw_wizard(f: &mut Frame, area: Rect, app: &mut App) {
    let [stepper, _, content] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let mut spans = vec![];
    for (i, s) in Step::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(" ── ".fg(BORDER));
        }
        let n = i + 1;
        if *s < app.step {
            spans.push(format!("✓ {}", s.title()).fg(OK));
        } else if *s == app.step {
            spans.push(Span::styled(format!(" {n} {} ", s.title()), Style::new().fg(Color::Black).bg(ACCENT).bold()));
        } else {
            spans.push(format!("{n} {}", s.title()).fg(MUTED));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)).alignment(Alignment::Center), stepper);

    let content = content.inner(Margin::new(1, 0));
    match app.step {
        Step::Project => draw_project(f, content, app),
        Step::Platform => draw_platform(f, content, app),
        Step::Board => draw_board(f, content, app),
        Step::Framework => draw_framework(f, content, app),
        Step::Libraries => draw_libraries(f, content, app),
        Step::Options => draw_options(f, content, app),
        Step::Review => draw_review(f, content, app),
    }
}

fn draw_project(f: &mut Frame, area: Rect, app: &mut App) {
    let r = center(area, 76, 13);
    let [title, name, dir, _, status] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(3),
    ])
    .areas(r);
    f.render_widget(
        Paragraph::new(vec![Line::from("Let's start a new project".fg(FG).bold())]).alignment(Alignment::Center),
        title,
    );
    draw_input(f, name, panel("Project name", app.proj_focus == 0), &app.name, app.proj_focus == 0, "my-firmware");
    draw_input(f, dir, panel("Location", app.proj_focus == 1), &app.dir, app.proj_focus == 1, "~/projects");
    let path = pio::contract_tilde(&app.project_path());
    let lines = match app.validate_project() {
        Ok(()) => vec![
            Line::from(vec!["  ✓ ".fg(OK), "will be created at ".fg(MUTED), path.fg(ACCENT2)]),
        ],
        Err(e) => vec![Line::from(vec!["  ○ ".fg(WARN), e.fg(MUTED)])],
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), status);
}

fn list_with_filter<'a>(
    f: &mut Frame,
    area: Rect,
    filter: &Input,
    filter_title: &str,
    placeholder: &str,
    list_title: String,
    items: Vec<ListItem<'a>>,
    state: &mut ratatui::widgets::ListState,
    loading: Option<&str>,
) {
    let [fa, la] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);
    draw_input(f, fa, panel(filter_title, true), filter, true, placeholder);
    let block = panel(&list_title, false);
    if let Some(msg) = loading {
        f.render_widget(Paragraph::new(msg.to_string().fg(MUTED)).block(block), la);
        return;
    }
    if items.is_empty() {
        f.render_widget(Paragraph::new("No matches".fg(MUTED).italic()).block(block), la);
        return;
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(selected())
        .highlight_symbol("▌ ")
        .scroll_padding(2);
    f.render_stateful_widget(list, la, state);
}

fn draw_platform(f: &mut Frame, area: Rect, app: &mut App) {
    let [left, right] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).spacing(1).areas(area);
    let total = app.boards.len();
    let items: Vec<ListItem> = app
        .plat_view
        .iter()
        .map(|p| match p {
            None => ListItem::new(Line::from(vec![
                "★ ".fg(STAR),
                format!("{:<26}", "All platforms").fg(FG).bold(),
                format!("{total:>5} boards").fg(MUTED),
            ])),
            Some(i) => {
                let p = &app.platforms[*i];
                ListItem::new(Line::from(vec![
                    if p.installed { "● ".fg(OK) } else { "○ ".fg(MUTED) },
                    format!("{:<26}", p.name).fg(FG),
                    format!("{:>5} boards", p.boards).fg(MUTED),
                ]))
            }
        })
        .collect();
    let loading = if app.boards_loading {
        Some(format!("{} Loading board catalogue…", app.spinner()))
    } else {
        app.boards_err.as_ref().map(|e| format!("⚠ {e}"))
    };
    let title = format!("Platforms ({})", app.platforms.len());
    list_with_filter(f, left, &app.plat_filter, "Filter platforms", "espressif, ststm32, atmelavr…", title, items, &mut app.plat_state, loading.as_deref());

    let block = panel("Details", false);
    let sel = app.plat_state.selected().and_then(|i| app.plat_view.get(i)).copied();
    let lines = match sel {
        Some(Some(i)) => {
            let p = &app.platforms[i];
            let mut v = vec![
                Line::from(p.name.clone().fg(ACCENT).bold()),
                Line::raw(""),
                kv("Boards", p.boards.to_string(), FG),
                kv("Status", if p.installed { "installed" } else { "will be installed on first build" }, if p.installed { OK } else { WARN }),
                Line::raw(""),
                Line::from("Frameworks".fg(MUTED)),
            ];
            for (fw, n) in p.frameworks.iter().take(10) {
                v.push(Line::from(vec![format!("  {fw:<18}").fg(ACCENT2), format!("{n} boards").fg(MUTED)]));
            }
            if p.frameworks.is_empty() {
                v.push(Line::from("  bare metal only".fg(MUTED)));
            }
            v
        }
        Some(None) => vec![
            Line::from("All platforms".fg(ACCENT).bold()),
            Line::raw(""),
            Line::from("Search every board PlatformIO knows about.".fg(MUTED)),
            Line::from("Tip: type a board name on the next step — the filter".fg(MUTED)),
            Line::from("matches id, name, vendor and MCU.".fg(MUTED)),
        ],
        None => vec![],
    };
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), right);
}

fn draw_board(f: &mut Frame, area: Rect, app: &mut App) {
    let [left, right] = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).spacing(1).areas(area);
    let id_w = 30usize;
    let items: Vec<ListItem> = app
        .board_view
        .iter()
        .map(|&i| {
            let b = &app.boards[i];
            ListItem::new(Line::from(vec![
                format!("{:<id_w$} ", truncate(&b.id, id_w)).fg(ACCENT2),
                b.name.clone().fg(FG),
            ]))
        })
        .collect();
    let scope = app.platform.clone().unwrap_or_else(|| "all platforms".into());
    let title = format!("Boards · {scope} ({}/{})", app.board_view.len(), app.boards.len());
    list_with_filter(f, left, &app.board_filter, "Find a board", "esp32 devkit, uno, nucleo f401…", title, items, &mut app.board_state, None);

    let block = panel("Board", false);
    let Some(b) = app.highlighted_board() else {
        f.render_widget(block, right);
        return;
    };
    let installed = app.installed.contains(&b.platform);
    let mut lines = vec![
        Line::from(b.name.clone().fg(ACCENT).bold()),
        Line::from(b.id.clone().fg(MUTED)),
        Line::raw(""),
        kv("Vendor", if b.vendor.is_empty() { "—".into() } else { b.vendor.clone() }, FG),
        Line::from(vec![
            format!("{:<11}", "Platform").fg(MUTED),
            b.platform.clone().fg(FG),
            if installed { "  ● installed".fg(OK) } else { "  ○ not installed".fg(MUTED) },
        ]),
        kv("MCU", b.mcu.clone(), ACCENT2),
        kv("Clock", pio::fmt_hz(b.fcpu), FG),
        kv("RAM", pio::fmt_bytes(b.ram), FG),
        kv("Flash", pio::fmt_bytes(b.rom), FG),
        Line::raw(""),
        Line::from("Frameworks".fg(MUTED)),
    ];
    if b.frameworks.is_empty() {
        lines.push(Line::from("none (bare metal)".fg(MUTED)));
    } else {
        lines.push(Line::from(chips(&b.frameworks, ACCENT2)));
    }
    if !b.url.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(b.url.clone().fg(MUTED).underlined()));
    }
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), right);
}

fn framework_desc(fw: &str) -> &'static str {
    match fw {
        "arduino" => "Arduino core — setup()/loop(), huge library ecosystem",
        "espidf" => "Espressif IoT Development Framework (FreeRTOS based)",
        "zephyr" => "Zephyr RTOS — scalable, security-focused real-time OS",
        "stm32cube" => "STM32Cube HAL & LL drivers by ST",
        "cmsis" => "Arm CMSIS core — minimal, close to the metal",
        "spl" => "Standard Peripheral Library (legacy ST drivers)",
        "libopencm3" => "Open-source firmware library for Cortex-M",
        "mbed" => "Arm Mbed OS",
        "freertos" => "FreeRTOS kernel",
        "esp8266-rtos-sdk" => "Espressif ESP8266 RTOS SDK",
        "esp8266-nonos-sdk" => "Espressif ESP8266 Non-OS SDK",
        "picosdk" => "Raspberry Pi Pico C/C++ SDK",
        "energia" => "Energia — Arduino-style framework for TI MCUs",
        "wiringpi" => "WiringPi — GPIO access library for Raspberry Pi",
        "fsp" => "Renesas Flexible Software Package",
        "simba" | "pumbaa" => "Simba embedded programming platform",
        _ => "",
    }
}

fn draw_framework(f: &mut Frame, area: Rect, app: &mut App) {
    let choices = app.framework_choices();
    let board = app.current_board().map(|b| b.name.clone()).unwrap_or_default();
    let h = (choices.len() as u16 * 2 + 4).max(8);
    let r = center(area, 74, h + 2);
    let [title, list_a] = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(r);
    f.render_widget(
        Paragraph::new(Line::from(vec!["Choose a framework for ".fg(MUTED), board.fg(ACCENT).bold()])).alignment(Alignment::Center),
        title,
    );
    let items: Vec<ListItem> = choices
        .iter()
        .map(|c| match c {
            Some(fw) => ListItem::new(vec![
                Line::from(fw.clone().fg(FG).bold()),
                Line::from(framework_desc(fw).fg(MUTED)),
            ]),
            None => ListItem::new(vec![
                Line::from("No framework".fg(FG).bold()),
                Line::from("This board has no framework support — bare metal C/C++".fg(MUTED)),
            ]),
        })
        .collect();
    let list = List::new(items)
        .block(panel("Frameworks", true))
        .highlight_style(selected())
        .highlight_symbol("▌ ");
    f.render_stateful_widget(list, list_a, &mut app.fw_state);
}

fn draw_libraries(f: &mut Frame, area: Rect, app: &mut App) {
    let [left, right] = Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).spacing(1).areas(area);
    let [search, results, details] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(5), Constraint::Length(7)]).areas(left);

    let focus_results = !app.lib_focus_selected;
    let plat = app.effective_platform().unwrap_or_default();
    let filter_label = if app.lib_platform_only {
        Line::from(vec![" platform: ".fg(MUTED), plat.fg(ACCENT2), " ".into()])
    } else {
        Line::from(" all platforms ".fg(MUTED))
    };
    let block = panel("Search PlatformIO Registry", focus_results).title(filter_label.right_aligned());
    draw_input(f, search, block, &app.lib_query, focus_results, "json, mqtt, neopixel, display…");

    let title = if app.lib_loading {
        format!("{} Searching…", app.spinner())
    } else if app.lib_query.is_empty() {
        format!("Popular ({} total)", app.lib_total)
    } else {
        format!("Results ({} of {})", app.lib_results.len(), app.lib_total)
    };
    let block = panel(&title, false);
    if let Some(e) = &app.lib_err {
        f.render_widget(Paragraph::new(format!("⚠ {e}").fg(ERR)).wrap(Wrap { trim: true }).block(block), results);
    } else if app.lib_results.is_empty() {
        let msg = if app.lib_loading { "" } else { "No libraries found" };
        f.render_widget(Paragraph::new(msg.fg(MUTED).italic()).block(block), results);
    } else {
        let chosen: std::collections::HashSet<String> = app.libs.iter().map(|l| l.key()).collect();
        let items: Vec<ListItem> = app
            .lib_results
            .iter()
            .map(|l| {
                let picked = chosen.contains(&l.key());
                ListItem::new(Line::from(vec![
                    if picked { "✓ ".fg(OK).bold() } else { "  ".into() },
                    l.name.clone().fg(if picked { OK } else { FG }).bold(),
                    format!("  {}", l.owner).fg(MUTED),
                    format!("  v{}", l.version).fg(ACCENT2),
                    if l.stars > 0 { format!("  ★ {}", l.stars).fg(STAR) } else { "".into() },
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(if focus_results { selected() } else { Style::new() })
            .highlight_symbol(if focus_results { "▌ " } else { "  " })
            .scroll_padding(2);
        f.render_stateful_widget(list, results, &mut app.lib_state);
    }

    let mut lines = vec![];
    if let Some(l) = app.lib_state.selected().and_then(|i| app.lib_results.get(i)) {
        lines.push(Line::from(vec![l.spec().fg(ACCENT2), format!("   {}", l.license).fg(MUTED)]));
        lines.push(Line::from(l.description.clone().fg(FG)));
        if !l.keywords.is_empty() {
            lines.push(Line::from(format!("# {}", l.keywords.iter().take(6).cloned().collect::<Vec<_>>().join(" · ")).fg(MUTED)));
        }
    }
    f.render_widget(Paragraph::new(lines).block(panel("Library", false)).wrap(Wrap { trim: true }), details);

    let block = panel(&format!("lib_deps ({})", app.libs.len()), app.lib_focus_selected);
    if app.libs.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from("No libraries yet.".fg(MUTED)),
                Line::raw(""),
                Line::from(vec!["Press ".fg(MUTED), "⏎".fg(ACCENT), " on a result to add it,".fg(MUTED)]),
                Line::from(vec!["or ".fg(MUTED), "^N".fg(ACCENT), " to skip this step.".fg(MUTED)]),
            ])
            .block(block)
            .wrap(Wrap { trim: true }),
            right,
        );
    } else {
        let items: Vec<ListItem> = app
            .libs
            .iter()
            .map(|l| {
                ListItem::new(vec![
                    Line::from(vec!["◆ ".fg(ACCENT), l.name.clone().fg(FG).bold()]),
                    Line::from(format!("  {}", l.spec()).fg(MUTED)),
                ])
            })
            .collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(if app.lib_focus_selected { selected() } else { Style::new() })
            .highlight_symbol(if app.lib_focus_selected { "▌ " } else { "  " });
        f.render_stateful_widget(list, right, &mut app.lib_sel_state);
    }
}

fn draw_options(f: &mut Frame, area: Rect, app: &mut App) {
    let r = center(area, 78, OPTION_ROWS as u16 * 2 + 3);
    let block = panel("Options", true);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let rows = Layout::vertical(vec![Constraint::Length(2); OPTION_ROWS]).split(inner.inner(Margin::new(0, 1)));

    let label_w = 34usize;
    let mut row = |i: usize, label: &str, value: Line<'static>| {
        let sel = app.opt_sel == i;
        let mut spans = vec![
            if sel { "▌ ".fg(ACCENT) } else { "  ".into() },
            format!("{label:<label_w$}").fg(if sel { FG } else { MUTED }).bold(),
        ];
        spans.extend(value.spans);
        let mut line = Line::from(spans);
        if sel {
            line = line.style(Style::new().bg(SEL_BG));
        }
        f.render_widget(Paragraph::new(line), rows[i]);
    };
    let input_line = |inp: &Input, ph: &str| -> Line<'static> {
        if inp.is_empty() {
            Line::from(ph.to_owned().fg(MUTED).italic())
        } else {
            Line::from(inp.value().fg(ACCENT2))
        }
    };
    let toggle = |on: bool, desc: &str| -> Line<'static> {
        Line::from(vec![
            if on { "[✓] ".fg(OK).bold() } else { "[ ] ".fg(MUTED) },
            desc.to_owned().fg(MUTED),
        ])
    };
    row(0, "Monitor speed (baud)", input_line(&app.monitor_speed, "default"));
    row(1, "Upload port", input_line(&app.upload_port, "auto-detect"));
    row(2, "Generate compile_commands.json", toggle(app.opt_compiledb, "code completion for clangd / IDEs"));
    row(3, "Write .clangd", toggle(app.opt_clangd, "drop GCC-only flags clang rejects"));
    row(4, "Source template", toggle(app.opt_template, "starter src/main.*"));
    row(5, "git init", toggle(app.opt_git, "initialize a repository"));

    let cursor_input = match app.opt_sel {
        0 => Some(&app.monitor_speed),
        1 => Some(&app.upload_port),
        _ => None,
    };
    if let Some(inp) = cursor_input {
        let (_, col) = inp.view(40);
        let r = rows[app.opt_sel];
        f.set_cursor_position(Position::new(r.x + 2 + label_w as u16 + col as u16, r.y));
    }
}

fn ini_highlight(ini: &str) -> Vec<Line<'static>> {
    ini.lines()
        .map(|l| {
            let t = l.trim_start();
            if t.starts_with(';') {
                Line::from(l.to_owned().fg(MUTED).italic())
            } else if t.starts_with('[') {
                Line::from(l.to_owned().fg(ACCENT).bold())
            } else if let Some((k, v)) = l.split_once('=') {
                Line::from(vec![k.to_owned().fg(ACCENT2), "=".fg(MUTED), v.to_owned().fg(FG)])
            } else if l.starts_with(' ') {
                Line::from(l.to_owned().fg(STAR))
            } else {
                Line::from(l.to_owned())
            }
        })
        .collect()
}

fn draw_review(f: &mut Frame, area: Rect, app: &mut App) {
    let [left, right] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).spacing(1).areas(area);
    let Some(spec) = app.spec() else { return };
    let yes = |b: bool| if b { "✓".fg(OK) } else { "·".fg(MUTED) };
    let mut lines = vec![
        Line::from(spec.name.clone().fg(ACCENT).bold()),
        Line::from(pio::contract_tilde(&spec.path()).fg(MUTED)),
        Line::raw(""),
        kv("Board", spec.board.name.clone(), FG),
        kv("", spec.board.id.clone(), MUTED),
        kv("Platform", spec.board.platform.clone(), ACCENT2),
        kv("Framework", spec.framework.clone().unwrap_or_else(|| "none".into()), ACCENT2),
        kv("MCU", format!("{} @ {}", spec.board.mcu, pio::fmt_hz(spec.board.fcpu)), FG),
        kv("Libraries", if spec.libs.is_empty() { "none".into() } else { spec.libs.len().to_string() }, FG),
        Line::raw(""),
        Line::from("After scaffolding".fg(MUTED)),
        Line::from(vec!["  ".into(), yes(true), " install platform, toolchain & libs".fg(FG)]),
        Line::from(vec!["  ".into(), yes(spec.compiledb), " compile_commands.json".fg(FG)]),
        Line::from(vec!["  ".into(), yes(spec.clangd), " .clangd".fg(FG)]),
        Line::from(vec!["  ".into(), yes(spec.template), " source template".fg(FG)]),
        Line::from(vec!["  ".into(), yes(spec.git), " git init".fg(FG)]),
        Line::raw(""),
        Line::from(vec!["Press ".fg(MUTED), " ⏎ Enter ".fg(Color::Black).bg(ACCENT).bold(), " to create".fg(MUTED)]),
    ];
    if !app.installed.contains(&spec.board.platform) {
        lines.insert(
            10,
            Line::from(vec!["⚠ ".fg(WARN), format!("{} will be downloaded (may take a while)", spec.board.platform).fg(WARN)]),
        );
    }
    f.render_widget(Paragraph::new(lines).block(panel("Summary", false)).wrap(Wrap { trim: false }), left);
    f.render_widget(Paragraph::new(ini_highlight(&spec.ini())).block(panel("platformio.ini", false)), right);
}

// ───────────────────────────── compiledb ─────────────────────────────

fn draw_cdb(f: &mut Frame, area: Rect, app: &mut App) {
    let r = center(area, 84, 22);
    let [title, input, status, list_a, toggle] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(2),
    ])
    .areas(r);
    f.render_widget(
        Paragraph::new("Generate compile_commands.json for an existing project".fg(FG).bold()).alignment(Alignment::Center),
        title,
    );
    draw_input(f, input, panel("Project directory", true), &app.cdb_dir, true, ".");
    let st = match &app.cdb_envs {
        Some(e) => Line::from(vec!["  ✓ ".fg(OK), "platformio.ini found · ".fg(MUTED), format!("{} environment(s)", e.len()).fg(ACCENT2)]),
        None => Line::from(vec!["  ○ ".fg(WARN), "no platformio.ini here".fg(MUTED)]),
    };
    f.render_widget(Paragraph::new(st), status);

    let mut items = vec![ListItem::new(Line::from(vec!["★ ".fg(STAR), "default environments".fg(FG)]))];
    if let Some(envs) = &app.cdb_envs {
        items.extend(envs.iter().map(|e| ListItem::new(Line::from(vec!["◆ ".fg(ACCENT), e.clone().fg(FG)]))));
    }
    let list = List::new(items)
        .block(panel("Environment", false))
        .highlight_style(selected())
        .highlight_symbol("▌ ");
    f.render_stateful_widget(list, list_a, &mut app.cdb_state);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            "  ".into(),
            if app.cdb_clangd { "[✓] ".fg(OK).bold() } else { "[ ] ".fg(MUTED) },
            "also write .clangd (Tab)".fg(MUTED),
        ])),
        toggle,
    );
}

// ───────────────────────────── running ─────────────────────────────

fn draw_running(f: &mut Frame, area: Rect, app: &mut App) {
    let spinner = app.spinner();
    let Some(run) = &app.run else { return };
    let [left, right] = Layout::horizontal([Constraint::Length(48), Constraint::Min(20)]).spacing(1).areas(area);

    let result_h = if run.finished.is_some() { 13 } else { 0 };
    let [steps_a, result_a] = Layout::vertical([Constraint::Min(5), Constraint::Length(result_h)]).areas(left);

    let mut lines: Vec<Line> = run
        .steps
        .iter()
        .map(|(name, st)| {
            let (icon, color) = match st {
                StepState::Pending => ("○".to_string(), MUTED),
                StepState::Running => (spinner.to_string(), ACCENT),
                StepState::Ok => ("✓".to_string(), OK),
                StepState::Failed => ("✗".to_string(), ERR),
                StepState::Skipped => ("–".to_string(), MUTED),
            };
            let style = match st {
                StepState::Running => Style::new().fg(FG).bold(),
                StepState::Ok => Style::new().fg(FG),
                _ => Style::new().fg(MUTED),
            };
            Line::from(vec![Span::styled(format!("{icon} "), Style::new().fg(color).bold()), Span::styled(name.clone(), style)])
        })
        .collect();
    let elapsed = run.finished.as_ref().map(|(_, d)| *d).unwrap_or_else(|| run.started.elapsed());
    let done = run.steps.iter().filter(|s| s.1 == StepState::Ok).count();
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        format!("{done}/{} done", run.steps.len()).fg(MUTED),
        "  ·  ".fg(BORDER),
        format!("{:.1}s", elapsed.as_secs_f64()).fg(MUTED),
    ]));
    if run.cancelling {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![spinner.fg(WARN), " stopping PlatformIO…".fg(WARN)]));
    }
    f.render_widget(Paragraph::new(lines).block(panel("Tasks", run.finished.is_none())), steps_a);

    if let Some((res, _)) = &run.finished {
        let (block, lines) = match res {
            Ok(s) => {
                let mut l = vec![
                    Line::from("✓ All done!".fg(OK).bold()),
                    Line::from(pio::contract_tilde(&s.project).fg(ACCENT2)),
                    Line::raw(""),
                ];
                match &s.cdb {
                    Some(c) => {
                        l.push(Line::from(vec!["compile_commands.json ".fg(FG), format!("{} {}", c.entries, if c.entries == 1 { "entry" } else { "entries" }).fg(MUTED)]));
                        // Only absolute compiler paths give a usable --query-driver glob.
                        if let Some(dir) = c.compiler.as_deref().map(std::path::Path::new).filter(|p| p.is_absolute()).and_then(|p| p.parent()) {
                            let dir = dir.display().to_string();
                            l.push(Line::from("clangd flag for full header support:".fg(MUTED)));
                            l.push(Line::from(format!("--query-driver={dir}/*").fg(STAR)));
                        }
                    }
                    None => l.push(Line::from("no compile_commands.json generated".fg(MUTED))),
                }
                (panel("Result", true).border_style(Style::new().fg(OK)), l)
            }
            Err(fail) => {
                let (title, color, head) = if fail.cancelled {
                    ("Cancelled", WARN, "■ Stopped")
                } else {
                    ("Failed", ERR, "✗ Something went wrong")
                };
                let mut l = vec![Line::from(head.fg(color).bold()), Line::from(fail.message.clone().fg(FG))];
                if let Some(dir) = &fail.partial_dir {
                    l.push(Line::raw(""));
                    if run.confirm_delete {
                        l.push(Line::from("Delete this folder and everything in it?".fg(WARN).bold()));
                        l.push(Line::from(pio::contract_tilde(dir).fg(ACCENT2)));
                        l.push(Line::from(vec!["y".fg(ACCENT).bold(), " yes · any other key: keep".fg(MUTED)]));
                    } else {
                        l.push(Line::from("Partial project left at".fg(MUTED)));
                        l.push(Line::from(pio::contract_tilde(dir).fg(ACCENT2)));
                        l.push(Line::from(vec!["d".fg(ACCENT).bold(), " delete it".fg(MUTED)]));
                    }
                }
                (panel(title, true).border_style(Style::new().fg(color)), l)
            }
        };
        let log_path = match res {
            Ok(s) => s.log_path.as_ref(),
            Err(f) => f.log_path.as_ref(),
        };
        let mut lines = lines;
        if let Some(p) = log_path {
            lines.push(Line::raw(""));
            lines.push(Line::from(vec!["log ".fg(MUTED), pio::contract_tilde(p).fg(MUTED).italic()]));
        }
        f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), result_a);
    }

    // log
    let mut title = if run.scroll > 0 { format!("Output · ↑{}", run.scroll) } else { "Output".into() };
    if run.dropped > 0 {
        title.push_str(&format!(" · {} earlier lines in log file", run.dropped));
    }
    let block = panel(&title, false);
    let inner = block.inner(right);
    let h = inner.height as usize;
    let end = run.log.len().saturating_sub(run.scroll);
    let start = end.saturating_sub(h);
    let width = inner.width as usize;
    let lines: Vec<Line> = run.log[start..end]
        .iter()
        .map(|l| {
            let lower = l.to_lowercase();
            let color = if l.starts_with("$ ") {
                ACCENT
            } else if lower.contains("error") || lower.contains("failed") {
                ERR
            } else if lower.contains("warning") {
                WARN
            } else if lower.contains("success") || lower.contains("installed") {
                OK
            } else {
                MUTED
            };
            let style = if l.starts_with("$ ") { Style::new().fg(color).bold() } else { Style::new().fg(color) };
            Line::from(Span::styled(truncate(l, width), style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), right);
}
