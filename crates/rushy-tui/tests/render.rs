use std::collections::BTreeMap;
use std::error::Error;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use rushy_bundles::Segment;
use rushy_pty::{PaneId, PaneView, Rect, SessionId, SessionInfo, View, WindowInfo};
use rushy_tui::mux::{self, Options, Screens};
use rushy_tui::palette::{self, Action};
use rushy_tui::{
    AiState, ConfigWatcher, Palette, PromptInput, RenderOpts, StatusInfo, Theme, Values, plain,
    render_prompt, render_status,
};

type TestResult = Result<(), Box<dyn Error>>;

fn text(buf: &Buffer) -> String {
    let w = usize::from(buf.area.width);
    buf.content()
        .chunks(w)
        .map(|row| {
            row.iter()
                .map(|c| c.symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn draw(w: u16, h: u16, f: impl FnOnce(&mut ratatui::Frame<'_>)) -> Result<Buffer, Box<dyn Error>> {
    let mut t = Terminal::new(TestBackend::new(w, h))?;
    t.draw(f)?;
    Ok(t.backend().buffer().clone())
}

fn segment(name: &str, format: &str, priority: u8, max: Option<u16>) -> Segment {
    Segment {
        segment: name.into(),
        color: "cyan".into(),
        format: format.into(),
        priority,
        max_width: max,
    }
}

fn vals(pairs: &[(&str, &[(&str, &str)])]) -> Values {
    pairs
        .iter()
        .map(|(k, v)| {
            (
                (*k).to_owned(),
                v.iter()
                    .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn themes_parse_and_report_errors() -> TestResult {
    let t = Theme::builtin("tokyo-night")?;
    assert_eq!(t.prompt_format, "{user} {path} {git}{node}{python}❯ ");
    assert!(Theme::mono()?.plain);
    assert!(Theme::builtin("nope").is_err());
    let bad = "name = \"x\"\n[colors]\nbackground = \"#zzzzzz\"\n";
    assert!(Theme::parse("x.toml".as_ref(), bad).is_err());
    let dir = tempfile::tempdir()?;
    std::fs::write(
        dir.path().join("mine.toml"),
        "name = \"mine\"\n[colors]\nbackground=\"black\"\nforeground=\"white\"\naccent=\"red\"\nerror=\"red\"\nsuccess=\"green\"\nprompt_user=\"blue\"\nprompt_path=\"cyan\"\n[prompt]\nformat=\"{path}> \"\n",
    )?;
    assert_eq!(Theme::load(dir.path(), "mine")?.prompt_format, "{path}> ");
    assert!(
        Theme::load(dir.path(), "../etc/passwd").is_err(),
        "names cannot escape the directory"
    );
    Ok(())
}

#[test]
fn prompt_segments_resolve_and_truncate() -> TestResult {
    let theme = Theme::builtin("tokyo-night")?;
    let input = PromptInput {
        user: "ash",
        host: "box",
        path: "~/code/rushy",
        status: 0,
    };
    let mut segs = BTreeMap::new();
    segs.insert(
        "git".to_owned(),
        segment("git", " {branch}{dirty} ", 80, Some(10)),
    );
    segs.insert(
        "node".to_owned(),
        segment("node", " node {version} ", 40, None),
    );
    let v = vals(&[
        (
            "git",
            &[("branch", "feature/very-long-branch"), ("dirty", "*")],
        ),
        ("node", &[("version", "22.1.0")]),
    ]);
    let wide = RenderOpts {
        width: 80,
        ascii: true,
    };
    assert_eq!(
        plain(&render_prompt(&theme, &input, &segs, &v, wide)),
        "ash ~/code/rushy  feature/~ node 22.1.0 ❯ "
    );
    // Too narrow: the low-priority node segment goes first.
    let narrow = RenderOpts {
        width: 38,
        ascii: true,
    };
    let p = plain(&render_prompt(&theme, &input, &segs, &v, narrow));
    assert!(!p.contains("node") && p.contains("feature"), "{p}");
    // Segments with no values are omitted; failed status is shown.
    let failed = PromptInput { status: 2, ..input };
    let theme_status = Theme::parse(
        "t.toml".as_ref(),
        "name=\"t\"\n[colors]\nbackground=\"reset\"\nforeground=\"reset\"\naccent=\"reset\"\nerror=\"red\"\nsuccess=\"reset\"\nprompt_user=\"reset\"\nprompt_path=\"reset\"\n[prompt]\nformat=\"{status}{git}$ \"\n",
    )?;
    assert_eq!(
        plain(&render_prompt(
            &theme_status,
            &failed,
            &segs,
            &Values::new(),
            wide
        )),
        "[2] $ "
    );
    // Very narrow: the path is cut from the left.
    let tiny = RenderOpts {
        width: 15,
        ascii: true,
    };
    let cut = plain(&render_prompt(
        &theme,
        &input,
        &BTreeMap::new(),
        &Values::new(),
        tiny,
    ));
    assert_eq!(cut.chars().count(), 15, "{cut}");
    assert!(cut.contains('~') && cut.ends_with("❯ "));
    Ok(())
}

#[test]
fn status_bar_screenshot() -> TestResult {
    let theme = Theme::mono()?;
    let info = StatusInfo {
        session: "work".into(),
        window: 1,
        windows: 2,
        pane: "%3".into(),
        bundles: 4,
        ai: AiState::Ready("anthropic".into()),
        clock: "14:05".into(),
    };
    let buf = draw(60, 1, |f| {
        f.render_widget(
            ratatui::widgets::Paragraph::new(render_status(&theme, &info, 60, true)),
            f.area(),
        );
    })?;
    assert_eq!(
        text(&buf),
        "[work] win 1/2 pane %3 | bundles 4 | ai: anthropic    14:05"
    );
    let narrow = draw(20, 1, |f| {
        f.render_widget(
            ratatui::widgets::Paragraph::new(render_status(&theme, &info, 20, true)),
            f.area(),
        );
    })?;
    assert_eq!(render_status(&theme, &info, 20, true).width(), 20);
    let line = text(&narrow);
    assert!(line.ends_with(" 14:05"), "{line}");
    Ok(())
}

fn view() -> View {
    View {
        session: SessionInfo {
            id: SessionId(1),
            name: "work".into(),
            windows: 1,
            panes: 2,
            attached: true,
        },
        windows: vec![WindowInfo {
            index: 0,
            active: true,
            panes: 2,
        }],
        panes: vec![
            PaneView {
                id: PaneId(1),
                rect: Rect::new(0, 0, 20, 5),
                focused: false,
            },
            PaneView {
                id: PaneId(2),
                rect: Rect::new(20, 0, 20, 5),
                focused: true,
            },
        ],
    }
}

#[test]
fn mux_screenshot_ascii_with_focus_and_content() -> TestResult {
    let theme = Theme::mono()?;
    let v = view();
    let mut screens = Screens::new();
    screens.sync(&v);
    screens.output(PaneId(1), b"left$ ls");
    screens.snapshot(PaneId(2), b"\x1b[1mright\x1b[0m> pwd");
    assert_eq!(screens.contents(PaneId(2)).as_deref(), Some("right> pwd"));
    let info = StatusInfo {
        session: "work".into(),
        window: 1,
        windows: 1,
        pane: "%2".into(),
        bundles: 0,
        ai: AiState::Off,
        clock: "09:00".into(),
    };
    let opts = Options {
        ascii: true,
        status_bar: true,
    };
    assert_eq!(mux::pane_area((40, 6), true), (40, 5));
    let buf = draw(40, 6, |f| {
        mux::draw(f, &v, &screens, &theme, Some(&info), opts)
    })?;
    let expected = "\
+ %1 --------------++ %2 [active] -----+
|left$ ls          ||right> pwd        |
|                  ||                  |
|                  ||                  |
+------------------++------------------+
[work] win 1/1 pane %2 | bundles 0 | ai: off 09:00";
    // The status line is padded to the width; compare it trimmed.
    let got = text(&buf);
    assert_eq!(
        got.lines().take(5).collect::<Vec<_>>().join("\n"),
        expected.lines().take(5).collect::<Vec<_>>().join("\n")
    );
    assert!(
        got.lines()
            .nth(5)
            .is_some_and(|l| l.starts_with("[work] win 1/1 pane %2") && l.ends_with("09:00"))
    );
    Ok(())
}

#[test]
fn mux_unicode_borders_without_ascii_flag() -> TestResult {
    let theme = Theme::mono()?;
    let v = view();
    let mut screens = Screens::new();
    screens.sync(&v);
    let buf = draw(40, 5, |f| {
        mux::draw(
            f,
            &v,
            &screens,
            &theme,
            None,
            Options {
                ascii: false,
                status_bar: false,
            },
        )
    })?;
    let first = text(&buf).lines().next().unwrap_or_default().to_owned();
    assert!(first.starts_with('┌') && first.contains('●'), "{first}");
    Ok(())
}

#[test]
fn palette_filters_navigates_and_chooses() -> TestResult {
    let mut aliases = BTreeMap::new();
    aliases.insert("gs".to_owned(), "git status --short".to_owned());
    let items = palette::items(
        &[("cd", "change directory"), ("history", "list history")],
        &["git".to_owned()],
        &aliases,
        &["tokyo-night".to_owned(), "mono".to_owned()],
        &["cargo test".to_owned(), "ls".to_owned()],
    );
    let mut p = Palette::new(items);
    assert_eq!(
        p.visible().len(),
        2 + 1 + 1 + 2 + palette::AI_ACTIONS.len() + 2
    );
    for c in "tokyo".chars() {
        p.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    assert_eq!(
        p.current().map(|i| i.insert.as_str()),
        Some("theme tokyo-night")
    );
    let theme = Theme::mono()?;
    let buf = draw(50, 10, |f| p.render(f, f.area(), &theme))?;
    let shot = text(&buf);
    assert!(
        shot.contains("Command palette") && shot.contains("> tokyo") && shot.contains("theme"),
        "{shot}"
    );

    let mut p = Palette::new(palette::items(
        &[],
        &[],
        &BTreeMap::new(),
        &[],
        &["cargo test".into()],
    ));
    for c in "cargo".chars() {
        p.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    p.on_key(KeyEvent::from(KeyCode::Backspace));
    assert_eq!(p.query(), "carg");
    let Action::Choose(item) = p.on_key(KeyEvent::from(KeyCode::Enter)) else {
        return Err("expected a choice".into());
    };
    assert_eq!(item.insert, "cargo test");
    assert_eq!(p.on_key(KeyEvent::from(KeyCode::Esc)), Action::Close);
    Ok(())
}

#[test]
fn only_config_files_count_as_config_changes() {
    use std::path::Path;
    for yes in [
        "config.toml",
        "themes/mine.toml",
        "bundles/git/init.rushy",
        "bundles/git/README.md",
        "bundles/git",
    ] {
        assert!(rushy_tui::is_config_path(Path::new(yes)), "{yes}");
    }
    for no in [
        "history.db",
        "history.db-wal",
        "mux/default.sock",
        "registry.json",
        "ai/17-agent.bin",
    ] {
        assert!(!rushy_tui::is_config_path(Path::new(no)), "{no}");
    }
}

#[tokio::test]
async fn watcher_ignores_database_writes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut w = ConfigWatcher::new(dir.path())?;
    let db = dir.path().join("history.db");
    tokio::task::spawn_blocking(move || {
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(db, "row")
    });
    let changed = tokio::time::timeout(Duration::from_millis(2500), w.changed()).await;
    assert!(
        changed.is_err(),
        "a database write must not look like a config change"
    );
    Ok(())
}

#[tokio::test]
async fn watcher_reports_theme_changes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let themes = dir.path().join("themes");
    let mut w = ConfigWatcher::new(&themes)?;
    let path = themes.join("new.toml");
    tokio::task::spawn_blocking({
        let path = path.clone();
        move || {
            std::thread::sleep(Duration::from_millis(100));
            std::fs::write(path, "name = \"new\"")
        }
    });
    let changed = tokio::time::timeout(Duration::from_secs(10), w.changed()).await;
    assert_eq!(changed.ok(), Some(true));
    Ok(())
}
