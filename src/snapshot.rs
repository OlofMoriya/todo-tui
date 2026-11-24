use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::database::{self as db, Result};

const INBOX: &str = "list:inbox";
pub type Snapshot = BTreeMap<String, Record>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    List {
        title: String,
        #[serde(default)]
        trashed_at: Option<String>,
    },
    Task {
        list: String,
        title: String,
        description: Option<String>,
        due_date: Option<String>,
        completed: bool,
        completed_date: Option<String>,
        next: bool,
        completion_note: Option<String>,
        pins: BTreeMap<String, String>,
        #[serde(default)]
        trashed_at: Option<String>,
    },
}

pub fn read(conn: &Connection) -> Result<Snapshot> {
    let inbox = db::inbox(conn)?.uid;
    let list_key = |uid: &str| {
        if uid == inbox {
            INBOX.into()
        } else {
            format!("list:{uid}")
        }
    };
    let mut data = Snapshot::new();
    let mut lists = conn.prepare("SELECT uid, title, trashed_at FROM lists")?;
    let mut rows = lists.query([])?;
    while let Some(row) = rows.next()? {
        data.insert(
            list_key(row.get_ref(0)?.as_str()?),
            Record::List {
                title: row.get(1)?,
                trashed_at: row.get(2)?,
            },
        );
    }
    let mut todos = conn.prepare(
        "SELECT t.uid, l.uid, t.title, t.description, t.due_date, t.completed, t.completed_date,
                t.is_next, t.completion_note, p.agent, p.avatar, t.trashed_at
         FROM todos t LEFT JOIN lists l ON t.list_id = l.id
         LEFT JOIN pins p ON p.todo_id = t.id ORDER BY t.id, p.agent",
    )?;
    let mut rows = todos.query([])?;
    while let Some(row) = rows.next()? {
        let uid = row.get_ref(0)?.as_str()?;
        let key = format!("task:{uid}");
        if !data.contains_key(&key) {
            let list: Option<String> = row.get(1)?;
            data.insert(
                key.clone(),
                Record::Task {
                    list: list_key(&list.ok_or_else(|| {
                        format!("task {uid} has no valid list; move it to Inbox before syncing")
                    })?),
                    title: row.get(2)?,
                    description: row.get(3)?,
                    due_date: row.get(4)?,
                    completed: row.get(5)?,
                    completed_date: row.get(6)?,
                    next: row.get(7)?,
                    completion_note: row.get(8)?,
                    pins: BTreeMap::new(),
                    trashed_at: row.get(11)?,
                },
            );
        }
        if let (Some(agent), Some(Record::Task { pins, .. })) =
            (row.get::<_, Option<String>>(9)?, data.get_mut(&key))
        {
            pins.insert(agent, row.get(10)?);
        }
    }
    validate(&data)?;
    Ok(data)
}

pub fn keys(a: &Snapshot, b: &Snapshot) -> BTreeSet<String> {
    a.keys().chain(b.keys()).cloned().collect()
}

pub fn delta(from: &Snapshot, to: &Snapshot) -> Vec<Delta> {
    keys(from, to)
        .into_iter()
        .filter_map(|key| {
            let before = from.get(&key);
            let after = to.get(&key);
            (before != after).then(|| Delta {
                key,
                before: before.cloned(),
                after: after.cloned(),
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub key: String,
    pub before: Option<Record>,
    pub after: Option<Record>,
}

pub fn validate(data: &Snapshot) -> Result<()> {
    let uid = |s: &str| {
        s.len() == 32
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    };
    if !matches!(data.get(INBOX), Some(Record::List { title, trashed_at: None }) if title == "Inbox")
    {
        return Err("sync snapshot has no Inbox".into());
    }
    for (key, record) in data {
        match record {
            Record::List { .. } if key == INBOX || key.strip_prefix("list:").is_some_and(uid) => (),
            Record::Task { list, .. } if key.strip_prefix("task:").is_some_and(uid) => {
                if !matches!(data.get(list), Some(Record::List { .. })) {
                    return Err(format!(
                        "{key} would be left in a deleted or missing list ({list})"
                    )
                    .into());
                }
            }
            _ => return Err(format!("invalid sync record key {key:?}").into()),
        }
    }
    Ok(())
}

// Caller holds an IMMEDIATE transaction and has verified `before` is current.
pub fn apply(tx: &Connection, before: &Snapshot, after: &Snapshot) -> Result<()> {
    validate(after)?;
    let inbox = db::inbox(tx)?.uid;
    let changes = delta(before, after);
    for d in &changes {
        if let Some(Record::List { title, trashed_at }) = &d.after {
            tx.execute("INSERT INTO lists (uid,title,trashed_at) VALUES (?1,?2,?3)
                        ON CONFLICT(uid) DO UPDATE SET title=excluded.title,trashed_at=excluded.trashed_at",
                       params![&d.key[5..], title, trashed_at])?;
        }
    }
    for d in &changes {
        if !d.key.starts_with("task:") {
            continue;
        }
        let uid = &d.key[5..];
        match &d.after {
            Some(Record::Task {
                list,
                title,
                description,
                due_date,
                completed,
                completed_date,
                next,
                completion_note,
                pins,
                trashed_at,
            }) => {
                let list = if list == INBOX { &inbox } else { &list[5..] };
                tx.execute(
                    "INSERT INTO todos (uid,list_id,title,description,due_date,completed,completed_date,is_next,completion_note,trashed_at)
                     VALUES (?1,(SELECT id FROM lists WHERE uid=?2),?3,?4,?5,?6,?7,?8,?9,?10)
                     ON CONFLICT(uid) DO UPDATE SET list_id=excluded.list_id,title=excluded.title,description=excluded.description,
                     due_date=excluded.due_date,completed=excluded.completed,completed_date=excluded.completed_date,
                     is_next=excluded.is_next,completion_note=excluded.completion_note,trashed_at=excluded.trashed_at",
                    params![uid,list,title,description,due_date,completed,completed_date,next,completion_note,trashed_at],
                )?;
                let id: i64 =
                    tx.query_row("SELECT id FROM todos WHERE uid=?", [uid], |r| r.get(0))?;
                tx.execute("DELETE FROM pins WHERE todo_id=?", [id])?;
                for (agent, avatar) in pins {
                    tx.execute(
                        "INSERT INTO pins(todo_id,agent,avatar) VALUES (?,?,?)",
                        params![id, agent, avatar],
                    )?;
                }
            }
            None => {
                tx.execute("DELETE FROM todos WHERE uid=?", [uid])?;
            }
            _ => unreachable!(),
        }
    }
    for d in &changes {
        if d.key.starts_with("list:") && d.after.is_none() {
            tx.execute("DELETE FROM lists WHERE uid=?", [&d.key[5..]])?;
        }
    }
    Ok(())
}
