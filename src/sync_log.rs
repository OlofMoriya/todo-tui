use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{database::Result, snapshot::Delta};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Synced,
    Error,
    Conflicts,
    Retry,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Entry {
    pub kind: Kind,
    pub message: String,
    pub uploaded: Vec<Delta>,
    pub downloaded: Vec<Delta>,
}

impl Entry {
    pub fn issue(kind: Kind, message: String) -> Self {
        Self {
            kind,
            message,
            uploaded: vec![],
            downloaded: vec![],
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub id: i64,
    pub at: String,
    #[serde(flatten)]
    pub entry: Entry,
}

pub fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS sync_log(id INTEGER PRIMARY KEY, at TEXT NOT NULL, entry TEXT NOT NULL);
                        CREATE TABLE IF NOT EXISTS sync_log_review(singleton INTEGER PRIMARY KEY CHECK(singleton=1), last_seen INTEGER NOT NULL)")?;
    Ok(())
}

pub fn write(conn: &Connection, entry: Entry) -> Result<()> {
    let encoded = serde_json::to_string(&entry)?;
    let previous: Option<String> = conn
        .query_row(
            "SELECT entry FROM sync_log ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if entry.kind != Kind::Synced && previous.as_ref() == Some(&encoded) {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO sync_log(at, entry) VALUES(?,?)",
        params![Utc::now().to_rfc3339(), encoded],
    )?;
    conn.execute(
        "DELETE FROM sync_log WHERE id <= (SELECT max(id) - 50 FROM sync_log)",
        [],
    )?;
    Ok(())
}

pub fn last_seen(conn: &Connection) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT last_seen FROM sync_log_review WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_default())
}

pub fn mark_seen(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("INSERT INTO sync_log_review VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET last_seen=max(last_seen, excluded.last_seen)", [id])?;
    Ok(())
}

pub fn head(conn: &Connection) -> Result<Option<Event>> {
    let mut events = recent(conn, 1)?;
    Ok(events.pop())
}

pub fn recent(conn: &Connection, limit: u32) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare("SELECT id, at, entry FROM sync_log ORDER BY id DESC LIMIT ?")?;
    let mut rows = stmt.query([limit])?;
    let mut list = vec![];
    while let Some(row) = rows.next()? {
        list.push(Event {
            id: row.get(0)?,
            at: row.get(1)?,
            entry: serde_json::from_str(row.get_ref(2)?.as_str()?)?,
        });
    }
    Ok(list)
}
