use rusqlite::Connection;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "todo-tui-{}-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed),
            chrono::Utc::now().timestamp_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn db(&self) -> PathBuf {
        self.0.join("todos.db")
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_todo-tui"))
            .env("HOME", &self.0)
            .env("TODO_TUI_DB", self.db())
            .args(args)
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stderr.is_empty());
        String::from_utf8(out.stdout).unwrap()
    }
    fn json(&self, args: &[&str]) -> Value {
        let mut args = args.to_vec();
        args.push("--json");
        serde_json::from_str(&self.ok(&args)).unwrap()
    }
    fn error(&self, args: &[&str], code: i32, message: &str) {
        let out = self.run(args);
        assert_eq!(out.status.code(), Some(code));
        assert!(out.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(message),
            "{out:?}"
        );
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id(value: &Value) -> &str {
    value["id"].as_str().unwrap()
}
fn ids(value: &Value) -> Vec<&str> {
    value.as_array().unwrap().iter().map(id).collect()
}

#[test]
fn local_workflow() {
    let s = Sandbox::new();
    let inbox = s.json(&["inbox"]);
    let list = s.json(&["lists", "create", "Work"]);
    let list = id(&list);
    assert_eq!(
        s.json(&["lists", "rename", list, "Project"])["title"],
        "Project"
    );
    assert_eq!(s.json(&["lists"]).as_array().unwrap().len(), 2);
    let due = s.json(&["add", "Due", "--due", "0", "-D", "Original"]);
    let next = s.json(&["add", "Soon"]);
    let focus = s.json(&["add", "Focus", "--list", list, "--date", "2000-01-01"]);
    assert_eq!(due["list_id"], id(&inbox));
    let (due, next, focus) = (id(&due), id(&next), id(&focus));
    assert_eq!(s.ok(&["list", "--count"]).trim(), "2");
    assert!(s.ok(&["list"]).contains("\t\"Due\""));
    assert_eq!(
        ids(&s.json(&[
            "list",
            "--date",
            "2000-01-01",
            "--list",
            list,
            "--status",
            "open"
        ])),
        [focus]
    );
    s.json(&["next", next]);
    s.json(&["pin", focus, "--agent", "Ada", "--avatar", "🐱"]);
    s.json(&["pin", focus, "--agent", "Bo"]);
    s.json(&["pin", due, "--agent", "Bo"]);
    s.json(&["pin", focus, "--agent", "Ada", "--avatar", "⚡"]);
    assert_eq!(ids(&s.json(&["today"])), [focus, due, next]);
    let pinned = s.json(&["pinned", "--agent", "Ada"]);
    assert_eq!(ids(&pinned), [focus]);
    assert_eq!(
        pinned[0]["pins"],
        serde_json::json!([{"agent":"Ada","avatar":"⚡"}])
    );
    assert_eq!(s.json(&["pinned"]).as_array().unwrap().len(), 2);
    s.json(&["unpin", focus, "--agent", "Ada"]);
    assert_eq!(s.json(&["show", focus])["completed"], false);
    assert_eq!(s.json(&["show", focus])["pins"][0]["agent"], "Bo");
    let before = s.json(&["show", due]);
    s.error(&["move", due, "missing", "--json"], 1, "no list");
    assert_eq!(s.json(&["show", due]), before);
    let moved = s.json(&["move", due, list]);
    assert_eq!(id(&moved), due);
    assert_eq!(moved["due_date"], before["due_date"]);
    assert_eq!(moved["pins"], before["pins"]);
    s.error(&["lists", "delete", list, "--yes"], 1, "not empty");
    s.error(&["lists", "delete", id(&inbox), "--yes"], 1, "Inbox");
    s.error(&["delete", focus], 2, "--yes");
    s.error(&["edit", due, "--title", "  ", "--json"], 1, "blank");
    let done = s.json(&["complete", due, "--note", "Done!"]);
    assert_eq!(done["description"], "Original");
    assert_eq!(done["completion_note"], "Done!");
    assert!(done["completed_date"].is_string());
    assert_eq!(done["pins"], serde_json::json!([]));
    assert_eq!(ids(&s.json(&["list", "--status", "completed"])), [due]);
    s.json(&["note", due, "Updated note"]);
    assert!(s
        .ok(&["show", due])
        .contains("Completion note: Updated note"));
    s.json(&["complete", next]);
    assert!(ids(&s.json(&["list", "--next"])).is_empty());
    assert_eq!(ids(&s.json(&["today"])), [focus]);
    assert_eq!(s.json(&["reopen", next])["completed_date"], Value::Null);
    assert_eq!(ids(&s.json(&["list", "--next"])), [next]);
    s.json(&["next", next, "--clear"]);
    assert!(ids(&s.json(&["list", "--next"])).is_empty());
    let edit = s.json(&[
        "edit",
        due,
        "--title",
        "New title",
        "--clear-description",
        "--clear-date",
    ]);
    assert_eq!(edit["title"], "New title");
    assert!(edit["description"].is_null() && edit["due_date"].is_null());
    assert_eq!(
        s.json(&["note", due, "--clear"])["completion_note"],
        Value::Null
    );
    s.json(&["delete", due, "--yes"]);
    s.json(&["delete", focus, "--yes"]);
    s.json(&["lists", "delete", list, "--yes"]);
    s.error(&["show", due, "--json"], 1, "no todo");
    assert_eq!(ids(&s.json(&["list"])), [next]);
}

fn legacy(path: &Path) {
    Connection::open(path).unwrap().execute_batch(
        "CREATE TABLE lists(id INTEGER PRIMARY KEY, title TEXT NOT NULL);
         CREATE TABLE todos(id INTEGER PRIMARY KEY, list_id INTEGER, title TEXT NOT NULL, description TEXT,
                            due_date TEXT, completed BOOLEAN NOT NULL, completed_date TEXT);
         INSERT INTO lists VALUES (10, 'Old list');
         INSERT INTO todos VALUES (23, 10, 'Old task', 'Keep me', '2024-05-02', 1, '2024-05-02 11:23:45');"
    ).unwrap();
}

#[test]
fn migrates_only_disposable_copies_without_changing_legacy_rows() {
    let a = Sandbox::new();
    let b = Sandbox::new();
    let original = a.0.join("legacy.db");
    legacy(&original);
    for db in [a.db(), b.db()] {
        fs::copy(&original, db).unwrap();
    }
    let task = a.json(&["show", "23"]);
    assert_eq!(task["title"], "Old task");
    assert_eq!(task["description"], "Keep me");
    assert_eq!(task["completed_date"], "2024-05-02");
    assert_eq!(task["due_date"], "2024-05-02");
    assert_eq!(task["completed"], true);
    assert_eq!(task["list_title"], "Old list");
    assert_eq!(task, a.json(&["show", id(&task)]));
    assert_ne!(id(&task), id(&b.json(&["show", "23"])));
    assert_eq!(a.json(&["inbox"])["title"], "Inbox");
    let version: u32 = Connection::open(original)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 0);
    let raw_date: String = Connection::open(a.db())
        .unwrap()
        .query_row("SELECT completed_date FROM todos WHERE id = 23", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(raw_date, "2024-05-02 11:23:45");
}
