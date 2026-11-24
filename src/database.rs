use std::{env, fs, path::PathBuf, time::Duration};

use chrono::{Local, NaiveDate};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::model::{Pin, Todo, TodoList};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn path() -> Result<PathBuf> {
    Ok(match env::var_os("TODO_TUI_DB") {
        Some(path) => PathBuf::from(path),
        None => {
            PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?).join(".todo/todos.sqlite")
        }
    })
}

pub fn open_db() -> Result<Connection> {
    open(false)
}

pub fn open(backup: bool) -> Result<Connection> {
    let path = path()?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut db = Connection::open(&path)?;
    db.busy_timeout(Duration::from_secs(5))?;
    if backup {
        sync_backup(&db, &path)?;
    }
    db.execute_batch("PRAGMA foreign_keys = ON")?;
    migrate(&mut db)?;
    Ok(db)
}

pub fn backup_path(path: &std::path::Path) -> PathBuf {
    let mut file = path.as_os_str().to_owned();
    file.push(".pre-ssh-sync.sqlite");
    file.into()
}

fn sync_backup(db: &Connection, path: &std::path::Path) -> Result<()> {
    let destination = backup_path(path);
    if destination.is_file() {
        return Ok(());
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let temp =
        tempfile::NamedTempFile::new_in(parent.unwrap_or_else(|| std::path::Path::new(".")))?;
    let mut copy = Connection::open(temp.path())?;
    {
        use rusqlite::backup::{Backup, StepResult};
        let backup = Backup::new(db, &mut copy)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if matches!(backup.step(100)?, StepResult::Done) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err("timed out creating sync backup".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    drop(copy);
    match temp.persist_noclobber(destination) {
        Ok(_) => Ok(()),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn migrate(db: &mut Connection) -> Result<()> {
    fn version(db: &Connection) -> rusqlite::Result<u32> {
        db.query_row("PRAGMA user_version", [], |row| row.get(0))
    }
    match version(db)? {
        1 => return Ok(()),
        0 => (),
        _ => return Err("database schema is newer than this version of todo-tui".into()),
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    match version(&tx)? {
        1 => return Ok(()),
        0 => (),
        _ => return Err("database schema is newer than this version of todo-tui".into()),
    }
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS lists (id INTEGER PRIMARY KEY, title TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS todos (
             id INTEGER PRIMARY KEY, list_id INTEGER, title TEXT NOT NULL, description TEXT,
             due_date TEXT, completed BOOLEAN NOT NULL, completed_date TEXT
         );
         ALTER TABLE lists ADD COLUMN uid TEXT;
         ALTER TABLE todos ADD COLUMN uid TEXT;
         ALTER TABLE todos ADD COLUMN is_next BOOLEAN NOT NULL DEFAULT 0;
         ALTER TABLE todos ADD COLUMN completion_note TEXT;
         UPDATE lists SET uid = lower(hex(randomblob(16)));
         UPDATE todos SET uid = lower(hex(randomblob(16)));
         CREATE UNIQUE INDEX lists_uid ON lists(uid);
         CREATE UNIQUE INDEX todos_uid ON todos(uid);
         CREATE INDEX todos_list ON todos(list_id);
         CREATE TABLE pins (
             todo_id INTEGER NOT NULL REFERENCES todos(id) ON DELETE CASCADE,
             agent TEXT NOT NULL, avatar TEXT NOT NULL, PRIMARY KEY(todo_id, agent)
         );
         INSERT INTO lists(title, uid) SELECT 'Inbox', lower(hex(randomblob(16)))
             WHERE NOT EXISTS (SELECT 1 FROM lists WHERE title = 'Inbox');
         PRAGMA user_version = 1;",
    )?;
    tx.commit()?;
    Ok(())
}

fn required(text: &str, name: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(format!("{name} cannot be blank").into());
    }
    Ok(())
}

fn resolve_id(db: &Connection, table: &str, id: &str) -> Result<usize> {
    // Stable IDs are always 32 characters, unlike legacy SQLite row IDs.
    let found = if id.len() == 32 {
        db.query_row(
            &format!("SELECT id FROM {table} WHERE uid = ?"),
            [id],
            |r| r.get(0),
        )
        .optional()?
    } else if let Ok(old) = id.parse::<i64>() {
        db.query_row(
            &format!("SELECT id FROM {table} WHERE id = ?"),
            [old],
            |r| r.get(0),
        )
        .optional()?
    } else {
        None
    };
    found.ok_or_else(|| format!("no {} with ID {id:?}", table.trim_end_matches('s')).into())
}

pub fn inbox(db: &Connection) -> Result<TodoList> {
    Ok(db.query_row(
        "SELECT id, uid, title FROM lists WHERE title = 'Inbox' ORDER BY id LIMIT 1",
        [],
        read_list,
    )?)
}

fn read_list(row: &rusqlite::Row<'_>) -> rusqlite::Result<TodoList> {
    Ok(TodoList {
        id: row.get(0)?,
        uid: row.get(1)?,
        title: row.get(2)?,
    })
}

pub fn lists(db: &Connection) -> Result<Vec<TodoList>> {
    let mut stmt = db.prepare(
        "SELECT id, uid, title FROM lists ORDER BY title != 'Inbox', title COLLATE NOCASE, id",
    )?;
    let rows = stmt
        .query_map([], read_list)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn create_list(db: &mut Connection, title: &str) -> Result<TodoList> {
    required(title, "list title")?;
    if title == "Inbox" {
        return Err("Inbox already exists".into());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO lists(title, uid) VALUES (?, lower(hex(randomblob(16))) )",
        [title],
    )?;
    let list = tx.query_row(
        "SELECT id, uid, title FROM lists WHERE id = ?",
        [tx.last_insert_rowid()],
        read_list,
    )?;
    tx.commit()?;
    Ok(list)
}

pub fn rename_list(db: &mut Connection, id: &str, title: &str) -> Result<TodoList> {
    required(title, "list title")?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let id = resolve_id(&tx, "lists", id)?;
    if title == "Inbox" || inbox(&tx)?.id == Some(id) {
        return Err("Inbox is reserved".into());
    }
    tx.execute(
        "UPDATE lists SET title = ? WHERE id = ?",
        params![title, id],
    )?;
    let list = tx.query_row(
        "SELECT id, uid, title FROM lists WHERE id = ?",
        [id],
        read_list,
    )?;
    tx.commit()?;
    Ok(list)
}

pub fn remove_list(db: &mut Connection, id: &str) -> Result<TodoList> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let id = resolve_id(&tx, "lists", id)?;
    if inbox(&tx)?.id == Some(id) {
        return Err("Inbox cannot be deleted".into());
    }
    let list = tx.query_row(
        "SELECT id, uid, title FROM lists WHERE id = ?",
        [id],
        read_list,
    )?;
    let changed = tx.execute(
        "DELETE FROM lists WHERE id = ? AND NOT EXISTS (SELECT 1 FROM todos WHERE list_id = ?)",
        [id, id],
    )?;
    if changed == 0 {
        return Err("list is not empty; move or delete its tasks first".into());
    }
    tx.commit()?;
    Ok(list)
}

#[derive(Default)]
pub struct Query<'a> {
    pub list: Option<&'a str>,
    pub due: Option<NaiveDate>,
    pub completed: Option<bool>,
    pub next: bool,
    pub today: bool,
    pub pinned: bool,
    pub agent: Option<&'a str>,
    pub id: Option<usize>,
}

fn date(raw: Option<String>) -> Option<NaiveDate> {
    // Older releases stored completion timestamps rather than dates.
    raw.and_then(|s| s.get(..10).and_then(|d| d.parse().ok()))
}

pub fn tasks(db: &Connection, q: Query<'_>) -> Result<Vec<Todo>> {
    let list_id = q.list.map(|id| resolve_id(db, "lists", id)).transpose()?;
    let mut stmt = db.prepare(
        "SELECT t.id, t.uid, l.uid, l.title, t.title, t.description, t.due_date,
                t.completed, t.completed_date, t.is_next, t.completion_note, p.agent, p.avatar
         FROM todos t LEFT JOIN lists l ON l.id = t.list_id
         LEFT JOIN pins p ON p.todo_id = t.id
         WHERE (?1 IS NULL OR t.id = ?1) AND (?2 IS NULL OR t.list_id = ?2)
           AND (?3 IS NULL OR t.due_date <= ?3) AND (?4 IS NULL OR t.completed = ?4)
           AND (NOT ?5 OR (t.is_next AND NOT t.completed))
           AND (NOT ?6 OR (NOT t.completed AND (t.is_next OR t.due_date <= ?7)))
           AND (NOT ?8 OR p.agent IS NOT NULL) AND (?9 IS NULL OR p.agent = ?9)
         ORDER BY t.completed, EXISTS (SELECT 1 FROM pins WHERE todo_id = t.id) DESC,
                  t.is_next DESC, t.due_date IS NULL, t.due_date, t.id, p.agent",
    )?;
    let mut rows = stmt.query(params![
        q.id,
        list_id,
        q.due.map(|d| d.to_string()),
        q.completed,
        q.next,
        q.today,
        Local::now().date_naive().to_string(),
        q.pinned,
        q.agent
    ])?;
    let mut result: Vec<Todo> = Vec::new();
    while let Some(row) = rows.next()? {
        let id = row.get(0)?;
        if result.last().is_none_or(|task| task.id != Some(id)) {
            result.push(Todo {
                id: Some(id),
                uid: row.get(1)?,
                list_uid: row.get::<_, Option<_>>(2)?.unwrap_or_default(),
                list_title: row
                    .get::<_, Option<_>>(3)?
                    .unwrap_or_else(|| "(missing list)".into()),
                title: row.get(4)?,
                description: row.get(5)?,
                due_date: date(row.get(6)?),
                completed: row.get(7)?,
                completed_date: date(row.get(8)?),
                next: row.get(9)?,
                completion_note: row.get(10)?,
                pins: vec![],
            });
        }
        if let Some(agent) = row.get(11)? {
            result.last_mut().unwrap().pins.push(Pin {
                agent,
                avatar: row.get(12)?,
            });
        }
    }
    Ok(result)
}

pub fn show(db: &Connection, id: &str) -> Result<Todo> {
    let id = resolve_id(db, "todos", id)?;
    task_by_row(db, id)
}

fn task_by_row(db: &Connection, id: usize) -> Result<Todo> {
    tasks(
        db,
        Query {
            id: Some(id),
            ..Query::default()
        },
    )?
    .pop()
    .ok_or_else(|| "task no longer exists".into())
}

pub fn create_task(
    db: &mut Connection,
    list: Option<&str>,
    title: &str,
    description: Option<&str>,
    due: Option<NaiveDate>,
) -> Result<Todo> {
    required(title, "task title")?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let list_id = match list {
        Some(id) => resolve_id(&tx, "lists", id)?,
        None => inbox(&tx)?.id.unwrap(),
    };
    tx.execute(
        "INSERT INTO todos(uid, list_id, title, description, due_date, completed)
         VALUES (lower(hex(randomblob(16))), ?, ?, ?, ?, 0)",
        params![list_id, title, description, due.map(|d| d.to_string())],
    )?;
    let task = task_by_row(&tx, tx.last_insert_rowid().try_into()?)?;
    tx.commit()?;
    Ok(task)
}

pub enum Change<'a> {
    Edit {
        title: Option<&'a str>,
        description: Option<Option<&'a str>>,
        due: Option<Option<NaiveDate>>,
    },
    Move(&'a str),
    Complete(Option<&'a str>),
    Reopen,
    Note(Option<&'a str>),
    Next(bool),
    Pin {
        agent: &'a str,
        avatar: Option<&'a str>,
    },
    Unpin(&'a str),
    Delete,
}

pub fn change_task(db: &mut Connection, id: &str, change: Change<'_>) -> Result<Todo> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let id = resolve_id(&tx, "todos", id)?;
    match change {
        Change::Edit {
            title,
            description,
            due,
        } => {
            if let Some(title) = title {
                required(title, "task title")?;
                tx.execute(
                    "UPDATE todos SET title = ? WHERE id = ?",
                    params![title, id],
                )?;
            }
            if let Some(description) = description {
                tx.execute(
                    "UPDATE todos SET description = ? WHERE id = ?",
                    params![description, id],
                )?;
            }
            if let Some(due) = due {
                tx.execute(
                    "UPDATE todos SET due_date = ? WHERE id = ?",
                    params![due.map(|d| d.to_string()), id],
                )?;
            }
        }
        Change::Move(dest) => {
            let dest = resolve_id(&tx, "lists", dest)?;
            tx.execute("UPDATE todos SET list_id = ? WHERE id = ?", [dest, id])?;
        }
        Change::Complete(note) => {
            tx.execute(
                "UPDATE todos SET completed = 1, completed_date = COALESCE(completed_date, ?),
                        completion_note = COALESCE(?, completion_note) WHERE id = ?",
                params![Local::now().date_naive().to_string(), note, id],
            )?;
            tx.execute("DELETE FROM pins WHERE todo_id = ?", [id])?;
        }
        Change::Reopen => {
            tx.execute(
                "UPDATE todos SET completed = 0, completed_date = NULL WHERE id = ?",
                [id],
            )?;
        }
        Change::Note(note) => {
            tx.execute(
                "UPDATE todos SET completion_note = ? WHERE id = ?",
                params![note, id],
            )?;
        }
        Change::Next(next) => {
            tx.execute(
                "UPDATE todos SET is_next = ? WHERE id = ?",
                params![next, id],
            )?;
        }
        Change::Pin { agent, avatar } => {
            required(agent, "agent")?;
            if let Some(avatar) = avatar {
                required(avatar, "avatar")?;
            }
            let completed: bool =
                tx.query_row("SELECT completed FROM todos WHERE id = ?", [id], |r| {
                    r.get(0)
                })?;
            if completed {
                return Err("cannot pin a completed task".into());
            }
            tx.execute("INSERT INTO pins(todo_id, agent, avatar) VALUES (?, ?, COALESCE(?, '●'))
                        ON CONFLICT(todo_id, agent) DO UPDATE SET avatar = COALESCE(?3, pins.avatar)",
                       params![id, agent, avatar])?;
        }
        Change::Unpin(agent) => {
            required(agent, "agent")?;
            tx.execute(
                "DELETE FROM pins WHERE todo_id = ? AND agent = ?",
                params![id, agent],
            )?;
        }
        Change::Delete => {
            let before = task_by_row(&tx, id)?;
            tx.execute("DELETE FROM todos WHERE id = ?", [id])?;
            tx.commit()?;
            return Ok(before);
        }
    }
    let task = task_by_row(&tx, id)?;
    tx.commit()?;
    Ok(task)
}
