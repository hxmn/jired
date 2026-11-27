//! The interactive paths, driven through a pseudo-terminal.
//!
//! Every other suite runs jired with `--no-input` and no terminal, so the
//! glue between a command and its screen — seed the form, run it, apply what
//! came back — and the prompts were reached by no test, the token prompt with
//! its raw mode least of all. Here jired runs on a pty, as it does for a
//! person, and the tests type at it.
#![cfg(unix)]

mod common;

use rexpect::session::{spawn_command, PtySession};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// A workspace on the recorded Cloud fixtures, like the end-to-end suite's.
struct Ws {
    dir: TempDir,
    home: TempDir,
    fixtures: PathBuf,
}

impl Ws {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let fixtures = dir.path().join(".fixtures");
        std::fs::create_dir_all(&fixtures).unwrap();
        let recorded = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cloud");
        for entry in std::fs::read_dir(recorded).unwrap().flatten() {
            std::fs::copy(entry.path(), fixtures.join(entry.file_name())).unwrap();
        }
        Self {
            dir,
            home: TempDir::new().unwrap(),
            fixtures,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// jired with the fixture server and a terminal it can draw on.
    fn command(&self, args: &[&str], token: bool) -> std::process::Command {
        let mut cmd = common::jired_std(self.home.path());
        cmd.current_dir(self.path())
            .env("TERM", "xterm-256color")
            .env("JIRED_FIXTURES", &self.fixtures)
            .env("JIRED_NO_KEYRING", "1")
            .env("JIRED_SECRET_KEY", "test-passphrase")
            .env("JIRED_URL", "https://fixture.invalid")
            .env("JIRED_EMAIL", "someone@example.invalid")
            .env("JIRED_PROJECT", "PROJ")
            .args(args);
        if token {
            cmd.env("JIRED_TOKEN", "fixture-token");
        }
        cmd
    }

    /// Run to completion with no terminal, for setting up.
    fn run(&self, args: &[&str]) {
        let out = self.command(args, true).arg("--no-input").output().unwrap();
        assert!(
            out.status.success(),
            "`jired {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn spawn(&self, args: &[&str], token: bool) -> PtySession {
        let p = spawn_command(self.command(args, token), Some(15_000)).unwrap();
        // A new pty is 0×0, and a screen of no size draws nothing: give it
        // the size of an ordinary terminal window.
        let size = nix::libc::winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        use std::os::fd::AsRawFd;
        #[allow(unsafe_code)]
        // SAFETY: a valid fd for the pty master, and a winsize that lives for
        // the call.
        unsafe {
            nix::libc::ioctl(p.process.pty.as_raw_fd(), nix::libc::TIOCSWINSZ, &size);
        }
        p
    }

    fn ready() -> Self {
        let ws = Self::new();
        ws.run(&["init", "--no-pull"]);
        ws.run(&["pull"]);
        ws
    }

    fn files_starting(&self, prefix: &str) -> Vec<String> {
        std::fs::read_dir(self.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(prefix))
            .collect()
    }
}

/// Send keys and flush them: rexpect holds what is sent until then.
fn type_in(p: &mut PtySession, text: &str) {
    p.send(text).unwrap();
    p.flush().unwrap();
}

/// Wait for the process to end, and say how, reading what it still writes
/// so it is not left blocked on a full pty.
fn finished(mut p: PtySession) -> rexpect::process::wait::WaitStatus {
    let rest = p.exp_eof().unwrap_or_else(|e| panic!("did not end: {e}"));
    let _ = rest;
    p.process.wait().unwrap()
}

fn exited_ok(status: &rexpect::process::wait::WaitStatus) -> bool {
    matches!(status, rexpect::process::wait::WaitStatus::Exited(_, 0))
}

/// `create` opens its form, takes a summary, and writes the issue.
#[test]
fn create_takes_a_summary_on_its_screen_and_writes_the_issue() {
    let ws = Ws::ready();
    let mut p = ws.spawn(&["create"], true);
    p.exp_string("New issue").unwrap();
    type_in(&mut p, "Made on a terminal");
    type_in(&mut p, "\r");
    let status = finished(p);
    assert!(exited_ok(&status), "{status:?}");
    let made = ws.files_starting("NEW-");
    assert!(
        made.iter().any(|n| n.contains("Made on a terminal")),
        "{made:?}"
    );
}

/// `logwork` on its screen books the time it is given against the issue
/// named on the command line.
#[test]
fn logwork_takes_a_duration_on_its_screen_and_stages_it() {
    let ws = Ws::ready();
    let mut p = ws.spawn(&["logwork", "PROJ-142"], true);
    p.exp_string("Log work").unwrap();
    type_in(&mut p, "1h 15m");
    type_in(&mut p, "\r");
    let status = finished(p);
    assert!(exited_ok(&status), "{status:?}");
    let file = std::fs::read_to_string(ws.path().join("PROJ-0142. Fix login flow.md")).unwrap();
    assert!(file.contains("### [new] 1h 15m"), "{file}");
}

/// `edit` opens on the issue as the file has it, and Enter saves the change
/// to the file.
#[test]
fn edit_changes_the_file_from_its_screen() {
    let ws = Ws::ready();
    let mut p = ws.spawn(&["edit", "PROJ-142"], true);
    p.exp_string("Edit issue").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    type_in(&mut p, " today");
    type_in(&mut p, "\r");
    let status = finished(p);
    assert!(exited_ok(&status), "{status:?}");
    // The summary names the file, so the file is renamed with it.
    let renamed = ws.files_starting("PROJ-0142.");
    assert_eq!(renamed, ["PROJ-0142. Fix login flow today.md"]);
}

/// Where no screen can be drawn, `create` asks line by line instead.
#[test]
fn create_asks_line_by_line_where_no_screen_can_be_drawn() {
    let ws = Ws::ready();
    let mut cmd = ws.command(&["create"], true);
    cmd.env("TERM", "dumb");
    let mut p = spawn_command(cmd, Some(15_000)).unwrap();
    p.exp_regex("(?i)summary").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    type_in(&mut p, "Asked on a dumb terminal\r");
    // Every question after the summary has an answer already: Enter takes it.
    let status = loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        match p.process.status() {
            Some(rexpect::process::wait::WaitStatus::StillAlive) => type_in(&mut p, "\r"),
            Some(done) => break done,
            None => panic!("lost the process"),
        }
    };
    assert!(exited_ok(&status), "{status:?}");
    assert!(ws
        .files_starting("NEW-")
        .iter()
        .any(|n| n.contains("Asked on a dumb terminal")));
}

/// Esc on an untouched form leaves without writing anything.
#[test]
fn escape_on_an_untouched_form_writes_nothing() {
    let ws = Ws::ready();
    let mut p = ws.spawn(&["create"], true);
    p.exp_string("New issue").unwrap();
    type_in(&mut p, "\x1b");
    let _ = finished(p);
    assert!(ws.files_starting("NEW-").is_empty());
}

/// The token prompt hides what is typed, and takes it.
#[test]
fn the_token_prompt_takes_a_token_without_showing_it() {
    let ws = Ws::new();
    let mut p = ws.spawn(&["init", "--no-pull"], false);
    p.exp_regex("(?i)token").unwrap();
    // Typed once the prompt is reading in raw mode, as a person would be.
    std::thread::sleep(std::time::Duration::from_millis(300));
    type_in(&mut p, "typed-secret-token");
    type_in(&mut p, "\r");
    let rest = p.exp_eof().unwrap();
    assert!(
        !rest.contains("typed-secret-token"),
        "the token was echoed: {rest}"
    );
    assert!(ws.path().join(".jired/session.db").is_file());
}

/// Ctrl-C at the token prompt ends jired, and gives the terminal back as it
/// found it: echo was left off before, so the next thing typed was invisible.
#[test]
fn ctrl_c_at_the_token_prompt_leaves_the_terminal_as_it_was() {
    use nix::sys::termios::{tcgetattr, tcsetattr, LocalFlags, SetArg};
    use std::os::unix::process::CommandExt;
    let ws = Ws::new();
    let mut cmd = ws.command(&["init", "--no-pull"], false);
    // rexpect turns echo off in the child just before it runs the program;
    // this runs after that, so jired starts on a terminal that echoes, as a
    // person's does, and has that to give back.
    #[allow(unsafe_code)]
    // SAFETY: only async-signal-safe termios calls on the child's own stdin.
    unsafe {
        cmd.pre_exec(|| {
            let stdin = std::io::stdin();
            if let Ok(mut t) = tcgetattr(&stdin) {
                t.local_flags.insert(LocalFlags::ECHO);
                let _ = tcsetattr(&stdin, SetArg::TCSANOW, &t);
            }
            Ok(())
        });
    }
    let mut p = spawn_command(cmd, Some(15_000)).unwrap();
    p.exp_regex("(?i)token").unwrap();
    // The prompt is reading in raw mode now.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let raw = tcgetattr(&p.process.pty).unwrap();
    assert!(
        !raw.local_flags.contains(LocalFlags::ECHO),
        "the token would be shown"
    );
    p.send_control('c').unwrap();
    p.flush().unwrap();
    let status = finished_after(&mut p);
    assert!(!exited_ok(&status), "{status:?}");
    let after = tcgetattr(&p.process.pty).unwrap();
    assert!(
        after
            .local_flags
            .contains(LocalFlags::ECHO | LocalFlags::ICANON),
        "the terminal was not given back: {:?}",
        after.local_flags
    );
}

fn finished_after(p: &mut PtySession) -> rexpect::process::wait::WaitStatus {
    p.process.wait().unwrap()
}
