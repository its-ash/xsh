use std::error::Error;
use std::time::Duration;

use rushy_pty::proto::{ClientMsg, ServerMsg, read_frame, write_frame};
use rushy_pty::{MuxCommand, PaneCommand, PaneId, server};

type TestResult = Result<(), Box<dyn Error>>;

async fn next(conn: &mut rushy_pty::server::Connection) -> Result<ServerMsg, Box<dyn Error>> {
    let msg = tokio::time::timeout(Duration::from_secs(10), read_frame::<_, ServerMsg>(conn))
        .await??
        .ok_or("connection closed")?;
    Ok(msg)
}

async fn until_text(
    conn: &mut rushy_pty::server::Connection,
    pane: &mut Vec<u8>,
    needle: &str,
) -> TestResult {
    while !String::from_utf8_lossy(pane).contains(needle) {
        match next(conn).await? {
            ServerMsg::Output(PaneId(_), b) => pane.extend(b),
            ServerMsg::Snapshot(PaneId(_), b) => pane.extend(b),
            ServerMsg::Error(e) => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

/// Detach keeps the session alive; attaching again redraws its screen.
#[tokio::test]
async fn sessions_survive_detach_and_reattach() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mux = dir.path().join("mux");
    let server_dir = mux.clone();
    let task =
        tokio::spawn(async move { server::serve(&server_dir, PaneCommand::new("/bin/sh")).await });
    let mut attempts = 0;
    let mut conn = loop {
        match server::connect(&mux).await {
            Ok(c) => break c,
            Err(e) => {
                attempts += 1;
                if task.is_finished() || attempts > 200 {
                    // The server could not start; report why, or skip when the
                    // environment forbids sockets altogether.
                    let why = match task.await {
                        Ok(Err(se)) => se.to_string(),
                        Ok(Ok(())) => "the server stopped".to_owned(),
                        Err(je) => je.to_string(),
                    };
                    if why.contains("Operation not permitted") {
                        eprintln!("skipped: this environment does not allow sockets ({why})");
                        return Ok(());
                    }
                    return Err(
                        format!("server did not start: {why} (last connect error: {e})").into(),
                    );
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    };
    let io = &mut conn;
    write_frame(
        io,
        &ClientMsg::Attach {
            session: "work".into(),
            create: true,
            cols: 60,
            rows: 12,
            cwd: None,
        },
    )
    .await?;
    let mut screen = Vec::new();
    // A sandbox without PTY access reports it as an error; that is not a bug here.
    let first = next(io).await?;
    if let ServerMsg::Error(e) = &first {
        if e.contains("Operation not permitted") {
            eprintln!("skipped: this environment does not allow PTYs ({e})");
            task.abort();
            return Ok(());
        }
        return Err(e.clone().into());
    }
    assert!(matches!(first, ServerMsg::View(_)), "{first:?}");
    write_frame(io, &ClientMsg::Input(b"echo persisted-$((6*7))\r".to_vec())).await?;
    until_text(io, &mut screen, "persisted-42").await?;

    write_frame(io, &ClientMsg::Command(MuxCommand::SplitHorizontal)).await?;
    loop {
        if let ServerMsg::View(v) = next(io).await? {
            if v.panes.len() == 2 {
                break;
            }
        }
    }
    write_frame(io, &ClientMsg::Command(MuxCommand::Detach)).await?;
    loop {
        if matches!(next(io).await?, ServerMsg::Detached) {
            break;
        }
    }
    drop(conn);

    conn = server::connect(&mux).await?;
    let io = &mut conn;
    write_frame(io, &ClientMsg::List).await?;
    let ServerMsg::Sessions(list) = next(io).await? else {
        return Err("expected a session list".into());
    };
    assert_eq!(
        (
            list.len(),
            list[0].name.as_str(),
            list[0].panes,
            list[0].attached
        ),
        (1, "work", 2, false)
    );

    write_frame(
        io,
        &ClientMsg::Attach {
            session: "work".into(),
            create: false,
            cols: 60,
            rows: 12,
            cwd: None,
        },
    )
    .await?;
    let mut again = Vec::new();
    until_text(io, &mut again, "persisted-42").await?;

    write_frame(io, &ClientMsg::Kill("work".into())).await?;
    task.abort();
    Ok(())
}
