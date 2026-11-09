use std::error::Error;
use std::path::Path;
use std::time::Duration;

use rushy_core::{Flow, JobState, Shell, ShellState};

type TestResult = Result<(), Box<dyn Error>>;

fn shell(dir: &Path) -> Shell {
    Shell::new(ShellState::new(dir.to_path_buf(), std::env::vars()))
}

async fn read(dir: &Path, name: &str) -> std::io::Result<String> {
    tokio::fs::read_to_string(dir.join(name)).await
}

#[tokio::test]
async fn redirection_writes_appends_and_reads() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.run_line("echo hello > out.txt").await?;
    sh.run_line("echo world >> out.txt").await?;
    assert_eq!(read(dir.path(), "out.txt").await?, "hello\nworld\n");
    sh.run_line("wc -l < out.txt > count.txt").await?;
    assert_eq!(read(dir.path(), "count.txt").await?.trim(), "2");
    Ok(())
}

#[tokio::test]
async fn multi_stage_pipeline() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.run_line("printf 'a\\nb\\nc\\n' | grep -v b | wc -l > n.txt")
        .await?;
    assert_eq!(read(dir.path(), "n.txt").await?.trim(), "2");
    Ok(())
}

#[tokio::test]
async fn exit_status_propagates() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.run_line("false").await?;
    assert_eq!(sh.last_status(), 1);
    sh.run_line("true").await?;
    assert_eq!(sh.last_status(), 0);
    sh.run_line("rushy-definitely-missing-command").await?;
    assert_eq!(sh.last_status(), 127);
    sh.run_line("false; echo $? > s.txt").await?;
    assert_eq!(read(dir.path(), "s.txt").await?, "1\n");
    assert_eq!(sh.run_line("exit 7").await?, Flow::Exit(7));
    Ok(())
}

#[tokio::test]
async fn ctrl_z_stops_the_foreground_command_and_fg_resumes_it() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    let fg = sh.state().foreground();
    // Stand in for the terminal: stop the process, then report Ctrl-Z.
    let stopper = tokio::spawn(async move {
        for _ in 0..300 {
            if let Some(pid) = fg.pids().first().copied() {
                let _ = std::process::Command::new("kill")
                    .args(["-STOP", &pid.to_string()])
                    .status();
                fg.request_stop();
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    });
    sh.run_line("sleep 1 2> /dev/null").await?;
    assert!(stopper.await?, "the command never started");
    assert_eq!(sh.last_status(), 148);
    assert_eq!(sh.job_count(), 1);
    sh.run_line("jobs > j.txt").await?;
    assert!(read(dir.path(), "j.txt").await?.contains("Stopped"));
    sh.run_line("fg > /dev/null").await?;
    assert_eq!(sh.last_status(), 0);
    assert_eq!(sh.job_count(), 0);
    Ok(())
}

#[tokio::test]
async fn only_exported_variables_reach_children() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    let script = "secret=s1; sh -c 'echo [$secret]'; \
        export secret; sh -c 'echo [$secret]'; \
        export -n secret; sh -c 'echo [$secret]'; echo $secret; \
        export NEW=n; sh -c 'echo [$NEW]'; \
        unset NEW; NEW=again; sh -c 'echo [$NEW]'";
    sh.run_line(&format!("{{ {script}; }} > e.txt")).await?;
    assert_eq!(
        read(dir.path(), "e.txt").await?,
        "[]\n[s1]\n[]\ns1\n[n]\n[]\n"
    );
    assert!(sh.state().is_exported("PWD"));
    Ok(())
}

#[tokio::test]
async fn variables_expand_with_quoting_rules() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.run_line("export GREETING='hi there'").await?;
    sh.run_line(r#"echo "$GREETING" ${GREETING} '$GREETING' > v.txt"#)
        .await?;
    assert_eq!(
        read(dir.path(), "v.txt").await?,
        "hi there hi there $GREETING\n"
    );
    sh.run_line("FOO=bar sh -c 'echo $FOO' > f.txt").await?;
    assert_eq!(read(dir.path(), "f.txt").await?, "bar\n");
    assert_eq!(sh.state().var("FOO"), None);
    sh.run_line("unset GREETING").await?;
    assert_eq!(sh.state().var("GREETING"), None);
    Ok(())
}

#[tokio::test]
async fn aliases_and_cd() -> TestResult {
    let dir = tempfile::tempdir()?;
    let root = tokio::fs::canonicalize(dir.path()).await?;
    tokio::fs::create_dir(root.join("sub")).await?;
    let mut sh = shell(&root);
    sh.run_line("alias greet='echo hi'").await?;
    sh.run_line("greet > a.txt").await?;
    assert_eq!(read(&root, "a.txt").await?, "hi\n");
    sh.run_line("cd sub").await?;
    assert_eq!(sh.state().cwd(), root.join("sub"));
    sh.run_line("pwd > p.txt").await?;
    assert_eq!(
        read(&root.join("sub"), "p.txt").await?.trim(),
        root.join("sub").display().to_string()
    );
    sh.run_line("cd ..").await?;
    assert_eq!(sh.state().cwd(), root);
    sh.run_line("cd missing-dir").await?;
    assert_eq!(sh.last_status(), 1);
    Ok(())
}

#[tokio::test]
async fn builtin_output_feeds_pipeline() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.run_line("alias ll='ls -l'; alias gs='git status'")
        .await?;
    sh.run_line("alias | wc -l > n.txt").await?;
    assert_eq!(read(dir.path(), "n.txt").await?.trim(), "2");
    Ok(())
}

#[tokio::test]
async fn history_dedupes_consecutive_and_skips_leading_space() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.run_line("true").await?;
    sh.run_line("true").await?;
    sh.run_line(" false").await?;
    sh.run_line("pwd").await?;
    assert_eq!(sh.state().history(), ["true", "pwd"]);
    Ok(())
}

#[tokio::test]
async fn failed_command_stderr_is_captured_for_ai_fix() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.state_mut().set_capture_stderr(true);
    sh.run_line("sh -c 'echo boom >&2; exit 3'").await?;
    let f = sh.last_failure().ok_or("no failure recorded")?;
    assert_eq!((f.exit_code, f.stderr.trim()), (3, "boom"));
    assert_eq!(f.command, "sh -c 'echo boom >&2; exit 3'");
    sh.record_external("ai explain x", 0, "");
    assert!(
        sh.last_failure().is_some(),
        "a successful ai command keeps the failure"
    );
    sh.run_line("true").await?;
    assert!(sh.last_failure().is_none());
    sh.run_line("rushy-definitely-missing-command").await?;
    let f = sh.last_failure().ok_or("no failure recorded")?;
    assert_eq!(f.exit_code, 127);
    assert!(f.stderr.contains("command not found"));
    Ok(())
}

#[tokio::test]
async fn captured_stderr_is_still_a_terminal() -> TestResult {
    use std::io::IsTerminal as _;
    if !std::io::stderr().is_terminal() {
        println!("skipped: standard error is not a terminal");
        return Ok(());
    }
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.state_mut().set_capture_stderr(true);
    sh.run_line("sh -c '[ -t 2 ] && echo tty > t.txt; echo boom >&2; exit 3'")
        .await?;
    assert_eq!(read(dir.path(), "t.txt").await?, "tty\n");
    let f = sh.last_failure().ok_or("no failure recorded")?;
    assert_eq!((f.exit_code, f.stderr.as_str()), (3, "boom\n"));
    Ok(())
}

#[tokio::test]
async fn a_helper_left_running_does_not_hold_the_prompt() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.state_mut().set_capture_stderr(true);
    let start = std::time::Instant::now();
    // The background `sleep` keeps the captured standard error open.
    sh.run_line("sh -c 'sleep 5 > /dev/null &'").await?;
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(sh.last_status(), 0);
    Ok(())
}

#[tokio::test]
async fn log_collects_stdout_and_stderr_of_every_command() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.state_mut().set_log(true);
    sh.run_line("echo first; sh -c 'echo oops >&2; exit 4'; pwd; echo a | wc -c")
        .await?;
    let (out, err) = sh.state_mut().take_log();
    assert!(out.contains("first\n"), "{out}");
    assert!(
        out.contains(&dir.path().display().to_string()),
        "builtin output is logged: {out}"
    );
    assert!(
        out.trim_end().ends_with('2'),
        "last stage output is logged: {out}"
    );
    assert_eq!(err.trim(), "oops");
    sh.state_mut().set_log(false);
    sh.run_line("echo quiet").await?;
    assert_eq!(sh.state_mut().take_log(), (String::new(), String::new()));
    Ok(())
}

#[tokio::test]
async fn sigterm_is_forwarded_to_the_foreground_command() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    let fg = sh.state().foreground();
    let task = tokio::spawn(async move {
        let ok = sh.run_line("sleep 30").await.is_ok();
        (ok, sh.last_status())
    });
    for _ in 0..300 {
        if !fg.pids().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(fg.signal("TERM"), 1);
    let (ok, status) = tokio::time::timeout(Duration::from_secs(10), task).await??;
    assert!(ok);
    assert_eq!(status, 143, "128 + SIGTERM");
    assert!(fg.pids().is_empty());
    Ok(())
}

/// Runs `script` with stdout to `out.txt` and returns that file.
async fn out(sh: &mut Shell, dir: &Path, script: &str) -> Result<String, Box<dyn Error>> {
    sh.run_line(&format!("{{ {script}\n}} > out.txt")).await?;
    Ok(read(dir, "out.txt").await?)
}

#[tokio::test]
async fn and_or_negation_and_errexit() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    let o = out(
        &mut sh,
        dir.path(),
        "true && echo a; false && echo b; false || echo c; ! false && echo d",
    )
    .await?;
    assert_eq!(o, "a\nc\nd\n");
    sh.run_line("! true").await?;
    assert_eq!(sh.last_status(), 1);
    sh.run_line("set -e").await?;
    assert_eq!(
        sh.run_line("false || true; echo still").await?,
        Flow::Continue
    );
    assert_eq!(sh.run_line("false; echo never").await?, Flow::Exit(1));
    Ok(())
}

#[tokio::test]
async fn compound_commands_and_loops() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    let script = r#"
if [ 1 -eq 2 ]; then echo no; elif test a = a; then echo elif; else echo else; fi
for i in 1 2 3; do [ $i = 2 ] && continue; echo f$i; done
i=0; while [ $i -lt 5 ]; do i=$((i+1)); [ $i = 4 ] && break; done; echo w$i
until true; do echo never; done
for ((j=0; j<3; j++)); do printf %s $j; done; echo
case foo.rs in *.txt) echo txt;; *.rs|*.go) echo code;& x) echo fell;; *) echo other;; esac
(( 3 > 2 )) && echo arith
[[ abc == a* && -n abc ]] && echo cond
[[ "a*" == "a*" ]] && echo literal
[[ abc =~ ^a.c$ ]] && echo regex
"#;
    let o = out(&mut sh, dir.path(), script).await?;
    assert_eq!(
        o,
        "elif\nf1\nf3\nw4\n012\ncode\nfell\narith\ncond\nliteral\nregex\n"
    );
    Ok(())
}

#[tokio::test]
async fn functions_locals_return_and_positionals() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    let script = r#"
greet() { local who=${1:-world}; echo "hi $who ($#)"; return 3; }
greet; echo $?; greet bob x
who=outer; greet z >/dev/null; echo $who
set -- a "b c" d
for a in "$@"; do echo "<$a>"; done
echo $# $1
shift; echo "$*"
fact() { if [ $1 -le 1 ]; then echo 1; else echo $(( $1 * $(fact $(( $1 - 1 ))) )); fi; }
fact 5
"#;
    let o = out(&mut sh, dir.path(), script).await?;
    assert_eq!(
        o,
        "hi world (0)\n3\nhi bob (2)\nouter\n<a>\n<b c>\n<d>\n3 a\nb c d\n120\n"
    );
    assert_eq!(sh.function_names(), ["fact", "greet"]);
    Ok(())
}

#[tokio::test]
async fn expansions() -> TestResult {
    let dir = tempfile::tempdir()?;
    let root = tokio::fs::canonicalize(dir.path()).await?;
    for f in ["a.rs", "b.rs", "c.txt"] {
        tokio::fs::write(root.join(f), "").await?;
    }
    let mut sh = shell(&root);
    let script = r#"
echo $(echo inner) `echo back` $((2 ** 10))
v=path/to/file.tar.gz
echo ${v##*/} ${v%%.*} ${v#*/} ${v%.*} ${#v}
echo ${unset:-dflt} ${v:+set} ${v/file/FILE} ${v//t/T} ${v:5:2} ${v^^}
echo ${new:=assigned} $new
echo {a,b}{1,2} x{1..3} {01..03} {c..a}
echo *.rs; echo '*.rs' "*.rs" nomatch*
w="one   two"; printf '[%s]' $w "$w"; echo
x=; printf '[%s]' "$x" $x; echo
IFS=: ; y=a:b; printf '[%s]' $y; echo; unset IFS
echo $'tab\there'
echo ~ | grep -q / && echo tilde
"#;
    let o = out(&mut sh, &root, script).await?;
    let want = "inner back 1024\n\
file.tar.gz path/to/file to/file.tar.gz path/to/file.tar 19\n\
dflt set path/to/FILE.tar.gz paTh/To/file.Tar.gz to PATH/TO/FILE.TAR.GZ\n\
assigned assigned\n\
a1 a2 b1 b2 x1 x2 x3 01 02 03 c b a\n\
a.rs b.rs\n*.rs *.rs nomatch*\n\
[one][two][one   two]\n\
[]\n\
[a][b]\n\
tab\there\n\
tilde\n";
    assert_eq!(o, want);
    sh.run_line("echo ${missing:?is required}").await?;
    assert_eq!(sh.last_status(), 1);
    sh.run_line("set -u; echo $missing; echo never > never.txt")
        .await?;
    assert!(read(&root, "never.txt").await.is_err());
    Ok(())
}

#[tokio::test]
async fn redirections_heredocs_and_process_substitution() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.run_line("sh -c 'echo out; echo err >&2' > both.txt 2>&1")
        .await?;
    assert_eq!(read(dir.path(), "both.txt").await?, "out\nerr\n");
    sh.run_line("sh -c 'echo e >&2' 2> err.txt").await?;
    assert_eq!(read(dir.path(), "err.txt").await?, "e\n");
    sh.run_line("sh -c 'echo o; echo e >&2' &> all.txt").await?;
    assert_eq!(read(dir.path(), "all.txt").await?, "o\ne\n");
    sh.run_line("echo x >&2 2>/dev/null").await?;
    sh.run_line("name=World\ncat <<EOF > h.txt\nHello $name\n$(echo sub)\nEOF")
        .await?;
    assert_eq!(read(dir.path(), "h.txt").await?, "Hello World\nsub\n");
    sh.run_line("cat <<'EOF' > q.txt\n$name\nEOF").await?;
    assert_eq!(read(dir.path(), "q.txt").await?, "$name\n");
    sh.run_line("tr a-z A-Z <<< hello > s.txt").await?;
    assert_eq!(read(dir.path(), "s.txt").await?, "HELLO\n");
    sh.run_line("cat <(echo one) <(echo two) > p.txt").await?;
    assert_eq!(read(dir.path(), "p.txt").await?, "one\ntwo\n");
    sh.run_line("set -C; echo new > s.txt").await?;
    assert_eq!(sh.last_status(), 1);
    sh.run_line("echo forced >| s.txt; set +C").await?;
    assert_eq!(read(dir.path(), "s.txt").await?, "forced\n");
    sh.run_line("sh -c 'echo piped >&2' 2>&1 | tr a-z A-Z > up.txt")
        .await?;
    assert_eq!(read(dir.path(), "up.txt").await?, "PIPED\n");
    Ok(())
}

#[tokio::test]
async fn pipelines_with_compound_stages_and_read() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    let script = r#"
printf 'a b\nc d\n' | while read x y; do echo "$y-$x"; done
for i in 3 1 2; do echo $i; done | sort | tr '\n' ' '; echo
echo "k=v" | { IFS== read k v; echo "$k:$v"; }
read -r line <<< 'back\slash'; echo "$line"
false | true; echo $?
set -o pipefail; false | true; echo $?; set +o pipefail
"#;
    let o = out(&mut sh, dir.path(), script).await?;
    assert_eq!(o, "b-a\nd-c\n1 2 3 \nk:v\nback\\slash\n0\n1\n");
    Ok(())
}

#[tokio::test]
async fn builtins_work() -> TestResult {
    let dir = tempfile::tempdir()?;
    let root = tokio::fs::canonicalize(dir.path()).await?;
    tokio::fs::create_dir_all(root.join("sub/deep")).await?;
    tokio::fs::write(
        root.join("lib.sh"),
        "alias hey='echo hey'\nhey from-source\nlibvar=$1\nreturn 4\necho never\n",
    )
    .await?;
    let mut sh = shell(&root);
    let script = r#"
. ./lib.sh arg; echo "$? $libvar"
eval 'echo ev; echo $((1+1))'
type cd | head -c 5; echo
command -v true
builtin echo bi
printf '%05.1f|%-3s|%x\n' 3.14159 ab 255
echo -e 'a\tb'
test -d sub && [ -f lib.sh ] && echo tests
f() { echo func; }; command f 2>/dev/null || echo nocmd
unset -f f; type f >/dev/null 2>&1 || echo gone
trap 'echo trapped' EXIT; trap | grep -c EXIT
"#;
    let o = out(&mut sh, &root, script).await?;
    assert_eq!(
        o,
        "hey from-source\n4 arg\nev\n2\ncd is\ntrue\nbi\n003.1|ab |ff\na\tb\ntests\nnocmd\ngone\n1\n"
    );
    sh.run_line("pushd sub > /dev/null; pushd deep > /dev/null")
        .await?;
    assert_eq!(sh.state().cwd(), root.join("sub/deep"));
    sh.run_line("popd > /dev/null").await?;
    assert_eq!(sh.state().cwd(), root.join("sub"));
    sh.run_line("cd ..").await?;
    sh.state_mut()
        .set_cdpath(vec![root.join("sub").display().to_string()]);
    sh.run_line("cd deep > /dev/null").await?;
    assert_eq!(sh.state().cwd(), root.join("sub/deep"));
    sh.run_line(&format!("cd {}", root.display())).await?;
    sh.state_mut().set_auto_cd(true);
    sh.run_line("sub").await?;
    assert_eq!(sh.state().cwd(), root.join("sub"));
    assert_eq!(sh.run_line("exit 5").await?, Flow::Exit(5));
    Ok(())
}

#[tokio::test]
async fn exit_trap_runs_on_exit() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.run_line("trap 'echo bye > bye.txt' EXIT").await?;
    assert_eq!(sh.run_line("exit 2").await?, Flow::Exit(2));
    assert_eq!(read(dir.path(), "bye.txt").await?, "bye\n");
    Ok(())
}

#[tokio::test]
async fn options_and_positional_api() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    assert!(sh.state_mut().set_option("xtrace", true));
    assert!(!sh.state_mut().set_option("bogus", true));
    sh.state_mut().set_option("xtrace", false);
    sh.state_mut()
        .set_positional("script.xsh".into(), vec!["a".into(), "b".into()]);
    let o = out(&mut sh, dir.path(), "echo $0 $# $2").await?;
    assert_eq!(o, "script.xsh 2 b\n");
    sh.state_mut().set_option("noexec", true);
    sh.run_line("echo hi > noexec.txt").await?;
    assert!(read(dir.path(), "noexec.txt").await.is_err());
    Ok(())
}

#[tokio::test]
async fn background_compound_job_runs_as_task() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.run_line("{ echo bg > bg.txt; exit 6; } &").await?;
    sh.run_line("wait").await?;
    assert_eq!(read(dir.path(), "bg.txt").await?, "bg\n");
    sh.run_line("sleep 5 & kill %1").await?;
    assert_eq!(sh.job_count(), 1);
    sh.run_line("wait").await?;
    assert_eq!(sh.job_count(), 0);
    Ok(())
}

#[tokio::test]
async fn background_jobs_are_reaped() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sh = shell(dir.path());
    sh.run_line("sh -c 'exit 3' &").await?;
    for _ in 0..200 {
        let done = sh.reap_jobs();
        if let Some(job) = done.first() {
            assert_eq!(job.state, JobState::Done(3));
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err("background job never finished".into())
}
