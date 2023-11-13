use std::{env, path::PathBuf, fs};

use crate::model::{Todo, TodoList};
use chrono::{Local, NaiveDate};
use rusqlite::{params, Connection, Result};

#[derive(Debug)]
pub enum DatabaseError {
    RusqliteError(rusqlite::Error),
}

impl From<rusqlite::Error> for DatabaseError {
    fn from(error: rusqlite::Error) -> Self {
        DatabaseError::RusqliteError(error)
    }
}

pub type SqlResult<T> = std::result::Result<T, DatabaseError>;

fn get_path() -> PathBuf {
    let home_dir: PathBuf = match env::var_os("HOME") {
        Some(home) => home.into(),
        None => {
            println!("Error: could not determine home directory.");
            std::process::exit(2);
        }
    };
    let dir = home_dir.join(".todo/");
    if !dir.is_dir() {
        fs::create_dir_all(dir).ok();
    }
    return home_dir.join(".todo/todos.sqlite");
}

pub fn open_db() -> SqlResult<Connection> {
    let conn = Connection::open(get_path())?;
    init_db(&conn)?;
    Ok(conn)
}

fn init_db(conn: &Connection) -> SqlResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS todos (
            id INTEGER PRIMARY KEY,
            list_id INTEGER,
            title TEXT NOT NULL,
            description TEXT,
            due_date TEXT,
            completed BOOLEAN NOT NULL,
            completed_date TEXT
        )",
        params![],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS lists (
            id INTEGER PRIMARY KEY,
            title TEXT NOT NULL
        )",
        params![],
    )?;

    Ok(())
}

pub fn add_todo(todo: &Todo) -> SqlResult<()> {
    let conn = open_db()?;

    conn.execute(
        "INSERT INTO todos (list_id, title, description, due_date, completed, completed_date) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            todo.list_id,
            todo.title,
            todo.description,
            todo.due_date.map(|d| d.to_string()),
            todo.completed,
            todo.completed_date.map(|d| d.to_string())
        ],
    )?;

    Ok(())
}

pub fn update_todo(todo: &Todo) -> SqlResult<()> {
    let conn = open_db()?;

    conn.execute(
        "UPDATE todos SET 
        list_id = ?2,
        title = ?3,
        description = ?4,
        due_date = ?5,
        completed = ?6,
        completed_date = ?7
        WHERE id = ?1
        ",
        params![
            todo.id,
            todo.list_id,
            todo.title,
            todo.description,
            todo.due_date.map(|d| d.to_string()),
            todo.completed,
            todo.completed_date.map(|d| d.to_string())
        ],
    )?;

    Ok(())
}

pub fn toggle_todo_completion(todo_id: usize, completed: bool) -> SqlResult<()> {
    let conn = open_db()?;
    let completed_date = if completed {
        Some(Local::now().naive_local().to_string())
    } else {
        None
    };

    conn.execute(
        "UPDATE todos SET 
            completed = ?2, 
            completed_date = ?3
        WHERE id = ?1",
        params![todo_id, completed, completed_date],
    )?;

    Ok(())
}

pub fn delete_todo(todo_id: usize) -> SqlResult<()> {
    let conn = open_db()?;
    conn.execute("DELETE FROM todos WHERE id = ?", params![todo_id])?;
    Ok(())
}

pub fn fetch_incomplete_todos(date: NaiveDate) -> SqlResult<Vec<Todo>> {
    let conn = open_db()?;

    // println!("{}", date.format( "%Y-%m-%d").to_string());
    let mut stmt = conn.prepare("SELECT * FROM todos WHERE completed = false and due_date <= ?")?;
    let rows = stmt.query_map(params![date.format( "%Y-%m-%d").to_string()], |row| {
        Ok(Todo {
            id: row.get(0)?,
            list_id: row.get(1)?,
            title: row.get(2)?,
            description: row.get(3)?,
            due_date: row
                .get::<_, Option<String>>(4)?
                .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
            completed: row.get(5)?,
            completed_date: row
                .get::<_, Option<String>>(6)?
                .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
            dependencies: vec![], // Fetch dependencies if needed.
        })
    })?;

    let todos: Vec<Todo> = rows.filter_map(Result::ok).collect();

    Ok(todos)
}

pub fn fetch_todos(list_id: usize) -> SqlResult<Vec<Todo>> {
    let conn = open_db()?;

    // Replace "WHERE 1" with your desired filter condition.
    let mut stmt = conn.prepare("SELECT * FROM todos WHERE list_id = ?")?;
    let rows = stmt.query_map(params![list_id], |row| {
        Ok(Todo {
            id: row.get(0)?,
            list_id: row.get(1)?,
            title: row.get(2)?,
            description: row.get(3)?,
            due_date: row
                .get::<_, Option<String>>(4)?
                .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
            completed: row.get(5)?,
            completed_date: row
                .get::<_, Option<String>>(6)?
                .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
            dependencies: vec![], // Fetch dependencies if needed.
        })
    })?;

    let todos: Vec<Todo> = rows.filter_map(Result::ok).collect();

    Ok(todos)
}

pub fn add_list(list: &TodoList) -> SqlResult<()> {
    let conn = open_db()?;
    conn.execute("INSERT INTO lists (title) VALUES (?)", params![list.title])?;
    Ok(())
}

pub fn delete_list(list_id: usize) -> SqlResult<()> {
    let conn = open_db()?;
    conn.execute("DELETE FROM lists WHERE id = ?", params![list_id])?;
    conn.execute("DELETE FROM todos WHERE list_id = ?", params![list_id])?;
    Ok(())
}

pub fn fetch_lists() -> SqlResult<Vec<TodoList>> {
    let conn = open_db()?;
    let mut stmt = conn.prepare("SELECT * FROM lists")?;
    let rows = stmt.query_map(params![], |row| {
        Ok(TodoList {
            id: row.get(0)?,
            title: row.get(1)?,
        })
    })?;

    let lists: Vec<TodoList> = rows.filter_map(Result::ok).collect();
    Ok(lists)
}
