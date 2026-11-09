use std::error::Error;
use std::time::Duration;

use rushy_pty::{Dir, MuxCommand, PaneCommand, PaneEvent, SessionManager};
use tokio::sync::mpsc::UnboundedReceiver;

type TestResult = Result<(), Box<dyn Error>>;

async fn wait_for(
    rx: &mut UnboundedReceiver<PaneEvent>,
    mgr: &SessionManager,
    needle: &str,
) -> TestResult {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(id) = mgr.focused() {
            if mgr.pane(id).is_some_and(|p| p.contents().contains(needle)) {
                return Ok(());
            }
        }
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return Err(format!("never saw {needle:?}").into()),
        }
    }
}

#[tokio::test]
async fn sessions_split_navigate_and_kill() -> TestResult {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut mgr = SessionManager::new(PaneCommand::new("/bin/sh"), tx, (80, 24));
    let id = match mgr.create_session("work") {
        Err(rushy_pty::Error::Pty(e)) if e.contains("Operation not permitted") => {
            eprintln!("skipped: this environment does not allow PTYs ({e})");
            return Ok(());
        }
        other => other?,
    };
    mgr.attach(id)?;
    mgr.write_input(b"echo first-$((40+2))\r")?;
    wait_for(&mut rx, &mgr, "first-42").await?;

    let first = mgr.focused();
    mgr.apply(MuxCommand::SplitHorizontal)?;
    let second = mgr.focused();
    assert_ne!(first, second);
    let view = mgr.view().ok_or("no view")?;
    assert_eq!(view.panes.len(), 2);
    assert_eq!(view.panes[1].rect.x, 40);
    if let Some(p) = second.and_then(|id| mgr.pane(id)) {
        assert_eq!(p.size(), (38, 22));
    }
    mgr.write_input(b"echo second-$((1+1))\r")?;
    wait_for(&mut rx, &mgr, "second-2").await?;

    mgr.apply(MuxCommand::Focus(Dir::Left))?;
    assert_eq!(mgr.focused(), first);
    mgr.apply(MuxCommand::NewWindow)?;
    assert_eq!(mgr.view().ok_or("no view")?.windows.len(), 2);
    mgr.apply(MuxCommand::PrevWindow)?;
    assert_eq!(mgr.focused(), first);

    mgr.apply(MuxCommand::KillPane)?;
    assert_eq!(mgr.focused(), second);
    mgr.detach()?;
    assert!(!mgr.list_sessions()[0].attached);
    mgr.kill_session(id)?;
    assert!(mgr.is_empty());
    Ok(())
}
