#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

use rusqlite::Connection;
use serde_json::Value;

// Use the real client binary and the real remote stdio command, with an SSH
// shim to inject failures deterministically without needing a login account.
struct Pair {
    root: tempfile::TempDir,
    local: PathBuf,
    remote: PathBuf,
}

impl Pair {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("client.sqlite");
        let remote = root.path().join("master.sqlite");
        let shim = root.path().join("ssh");
        fs::write(
            &shim,
            r#"#!/bin/sh
printf 'ssh\n' >> "$TEST_ROOT/ssh.log"
case "$*" in
    *'-- mini todo-tui sync --stdio') ;;
    *) echo 'unexpected SSH command' >&2; exit 99 ;;
esac
if test "$TEST_FAULT" = offline; then echo 'simulated offline' >&2; exit 255; fi
export TODO_TUI_DB="$TEST_REMOTE"
if test -z "$TEST_FAULT"; then exec "$TEST_BIN" sync --stdio; fi
payload=$(mktemp "$TEST_ROOT/request.XXXXXXXX") || exit 1
trap 'rm -f "$payload"' EXIT
cat > "$payload"
if grep -q '"action":"apply"' "$payload"; then
    if test "$TEST_FAULT" = race; then
        "$TEST_BIN" add 'Master raced with client' >/dev/null || exit 1
    elif test "$TEST_FAULT" = lost_ack; then
        "$TEST_BIN" sync --stdio < "$payload" > "$TEST_ROOT/lost-reply" || exit 1
        grep -q '"status":"applied"' "$TEST_ROOT/lost-reply" || exit 1
        echo 'simulated dropped response' >&2
        exit 255
    fi
fi
"$TEST_BIN" sync --stdio < "$payload"
"#,
        )
        .unwrap();
        fs::set_permissions(shim, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root,
            local,
            remote,
        }
    }

    fn command(&self, remote: bool, args: &[&str], fault: &str) -> Output {
        let mut path = vec![self.root.path().to_path_buf()];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        Command::new(env!("CARGO_BIN_EXE_todo-tui"))
            .args(args)
            .arg("--json")
            .env(
                "TODO_TUI_DB",
                if remote { &self.remote } else { &self.local },
            )
            .env("PATH", std::env::join_paths(path).unwrap())
            .env("TEST_ROOT", self.root.path())
            .env("TEST_REMOTE", &self.remote)
            .env("TEST_BIN", env!("CARGO_BIN_EXE_todo-tui"))
            .env("TEST_FAULT", fault)
            .output()
            .unwrap()
    }

    fn json(&self, remote: bool, args: &[&str]) -> Value {
        parse(self.command(remote, args, ""))
    }
    fn local(&self, args: &[&str]) -> Value {
        self.json(false, args)
    }
    fn master(&self, args: &[&str]) -> Value {
        self.json(true, args)
    }
    fn count_ssh(&self) -> usize {
        fs::read_to_string(self.root.path().join("ssh.log"))
            .unwrap_or_default()
            .lines()
            .count()
    }
    fn first_sync(&self) {
        self.local(&["sync", "configure", "mini"]);
        assert_eq!(self.local(&["sync", "preview"])["outcome"], "preview");
        assert_eq!(
            self.local(&["sync", "--accept-import"])["outcome"],
            "synced"
        );
    }
}

fn parse(out: Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn expect_error(out: Output, message: &str) {
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(message),
        "{out:?}"
    );
}
fn id(value: &Value) -> &str {
    value["id"].as_str().unwrap()
}
fn rows(value: &Value) -> usize {
    value.as_array().unwrap().len()
}
fn key(prefix: &str, id: &str) -> String {
    format!("{prefix}:{id}")
}

#[test]
fn first_import_backs_up_both_legacy_databases_and_requires_current_preview() {
    let p = Pair::new();
    for (path, title) in [(&p.local, "Client task"), (&p.remote, "Master task")] {
        let old = Connection::open(path).unwrap();
        old.execute_batch("CREATE TABLE lists(id INTEGER PRIMARY KEY,title TEXT NOT NULL);
            CREATE TABLE todos(id INTEGER PRIMARY KEY,list_id INTEGER,title TEXT NOT NULL,description TEXT,
                               due_date TEXT,completed BOOLEAN NOT NULL,completed_date TEXT);
            INSERT INTO lists VALUES (1, 'Inbox');").unwrap();
        old.execute(
            "INSERT INTO todos VALUES(1,1,?,NULL,'2000-01-01',0,NULL)",
            [title],
        )
        .unwrap();
    }
    p.local(&["sync", "configure", "mini"]);
    assert_eq!(p.count_ssh(), 0);
    let preview = p.local(&["sync", "preview"]);
    assert_eq!(rows(&preview["upload"]), 1);
    assert_eq!(rows(&preview["download"]), 1);
    assert_eq!(preview["download"][0]["after"]["title"], "Master task");
    assert_eq!(preview["upload"][0]["after"]["title"], "Client task");
    for path in [&p.local, &p.remote] {
        let backup = Connection::open_with_flags(
            format!("{}.pre-ssh-sync.sqlite", path.display()),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            backup
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            backup
                .query_row("SELECT count(*) FROM todos", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    let client_inbox = p.local(&["inbox"]);
    let master_inbox = p.master(&["inbox"]);
    assert_ne!(id(&client_inbox), id(&master_inbox));
    let extra = p.master(&["add", "Added after preview"]);
    let refreshed = p.local(&["sync", "--accept-import"]);
    assert_eq!(refreshed["outcome"], "preview");
    assert_eq!(refreshed["changed_since_preview"], true);
    assert_eq!(rows(&p.local(&["list"])), 1);
    assert_eq!(rows(&p.master(&["list"])), 2);
    assert_eq!(p.local(&["sync", "--accept-import"])["outcome"], "synced");
    assert_eq!(rows(&p.local(&["list"])), 3);
    assert_eq!(rows(&p.master(&["list"])), 3);
    assert_eq!(p.local(&["show", id(&extra)])["list_id"], id(&client_inbox));
    assert_eq!(p.master(&["inbox"]), master_inbox);
    assert_eq!(p.local(&["sync", "status"])["pending_changes"], 0);
}

#[test]
fn offline_work_conflicts_deletions_and_retries() {
    let p = Pair::new();
    assert_eq!(p.local(&["sync"])["outcome"], "off");
    assert_eq!(p.count_ssh(), 0);
    let both = p.local(&["add", "Both sides edit"]);
    let both = id(&both);
    let delete = p.master(&["add", "Delete me"]);
    let delete = id(&delete);
    let moved = p.local(&[
        "add",
        "Move me",
        "--date",
        "2000-01-01",
        "-D",
        "Description",
    ]);
    let moved = id(&moved);
    p.first_sync();
    expect_error(p.command(false, &["sync"], "offline"), "simulated offline");
    assert_eq!(p.local(&["sync", "status"])["reachable"], false);

    let work = p.local(&["lists", "create", "Work"]);
    let work = id(&work);
    p.local(&["move", moved, work]);
    p.local(&["next", moved]);
    p.local(&["pin", moved, "--agent", "Codex", "--avatar", "🛠"]);
    p.local(&["note", moved, "A separate note"]);
    p.local(&["edit", both, "--title", "Local version"]);
    p.master(&["edit", both, "--title", "Master version"]);
    p.master(&["delete", delete, "--yes"]);
    let remote_add = p.master(&["add", "Master added this offline"]);
    assert_eq!(p.local(&["sync", "status"])["pending_changes"], 3);
    assert_eq!(p.local(&["sync"])["outcome"], "conflicts");
    let conflict = &p.local(&["sync", "conflicts"])[0];
    let conflict_key = key("task", both);
    assert_eq!(conflict["key"], conflict_key);
    assert_eq!(conflict["base"]["title"], "Both sides edit");
    assert_eq!(conflict["local"]["title"], "Local version");
    assert_eq!(conflict["master"]["title"], "Master version");
    assert_eq!(p.local(&["show", both])["title"], "Local version");
    assert_eq!(p.master(&["show", both])["title"], "Master version");
    p.local(&["sync", "resolve", &conflict_key, "master"]);
    assert_eq!(p.local(&["sync"])["outcome"], "synced");
    assert_eq!(p.local(&["show", both])["title"], "Master version");
    assert_eq!(
        p.local(&["show", id(&remote_add)])["title"],
        "Master added this offline"
    );
    let synced_move = p.master(&["show", moved]);
    assert_eq!(synced_move["list_id"], work);
    assert_eq!(synced_move["next"], true);
    assert_eq!(synced_move["pins"][0]["agent"], "Codex");
    assert_eq!(synced_move["completion_note"], "A separate note");
    expect_error(p.command(false, &["show", delete], ""), "no todo");
    assert_eq!(
        p.local(&["sync"]),
        serde_json::json!({"outcome":"synced","uploaded":0,"downloaded":0})
    );
    p.local(&["sync", "off"]);
    let ssh_calls = p.count_ssh();
    assert_eq!(p.local(&["sync"])["outcome"], "off");
    p.local(&["list"]);
    assert_eq!(p.count_ssh(), ssh_calls);
}

#[test]
fn compare_and_swap_and_lost_ack_do_not_duplicate_or_resurrect() {
    let p = Pair::new();
    p.first_sync();
    let task = p.local(&["add", "Outgoing task"]);
    let task = id(&task);
    assert_eq!(
        parse(p.command(false, &["sync"], "race"))["outcome"],
        "retry"
    );
    expect_error(p.command(true, &["show", task], ""), "no todo");
    assert_eq!(rows(&p.master(&["list"])), 1);
    expect_error(
        p.command(false, &["sync"], "lost_ack"),
        "simulated dropped response",
    );
    assert_eq!(p.local(&["sync", "status"])["pending_attempt"], true);
    assert_eq!(p.master(&["show", task])["title"], "Outgoing task");
    assert_eq!(rows(&p.master(&["list"])), 2);
    p.master(&["delete", task, "--yes"]);
    let another = p.local(&["add", "Kept editing after lost response"]);
    assert_eq!(p.local(&["sync"])["outcome"], "synced");
    assert_eq!(p.local(&["sync"])["outcome"], "synced");
    expect_error(p.command(false, &["show", task], ""), "no todo");
    expect_error(p.command(true, &["show", task], ""), "no todo");
    assert_eq!(
        p.master(&["show", id(&another)])["title"],
        "Kept editing after lost response"
    );
    assert_eq!(rows(&p.master(&["list"])), 2);
    assert_eq!(rows(&p.local(&["list"])), 2);
}

#[test]
fn deletion_conflicts_with_new_task_in_the_list() {
    let p = Pair::new();
    let work = p.local(&["lists", "create", "Work"]);
    let work = id(&work);
    p.first_sync();
    p.local(&["lists", "delete", work, "--yes"]);
    let task = p.master(&["add", "New task", "--list", work]);
    assert_eq!(p.local(&["sync"])["outcome"], "conflicts");
    let conflict_key = key("list", work);
    assert_eq!(p.local(&["sync", "conflicts"])[0]["key"], conflict_key);
    expect_error(
        p.command(false, &["sync", "resolve", &conflict_key, "local"], ""),
        "deletes a list with tasks",
    );
    p.local(&["sync", "resolve", &conflict_key, "master"]);
    assert_eq!(p.local(&["sync"])["outcome"], "synced");
    assert_eq!(p.local(&["show", id(&task)])["list_id"], work);
    assert_eq!(p.master(&["show", id(&task)])["list_id"], work);
}
