use chrono::{Days, Local, NaiveDate};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

use crate::{
    database::{self as db, Change, Query, Result, TrashAction, TrashFilter},
    model::{Todo, TodoList},
};

#[derive(Parser)]
#[command(
    version,
    about = "Local-first terminal todo manager",
    after_help = "Examples:\n  todo-tui add 'Write tests' --due 0 --json\n  todo-tui list --json\n  todo-tui today\n  todo-tui pin 1 --agent Ada --avatar 🐱\n  todo-tui complete 1 --note 'Tests passed'\n\nExit status: 0 success, 1 database or workflow error, 2 invalid command line."
)]
pub struct Cli {
    /// Print a single JSON value on stdout
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, ValueEnum)]
pub enum Status {
    Open,
    Completed,
    All,
}

#[derive(Subcommand)]
pub enum Command {
    /// List tasks (plain list defaults to open tasks due by today)
    List {
        /// Include only tasks due on or before this date (YYYY-MM-DD)
        #[arg(short, long)]
        date: Option<NaiveDate>,
        #[arg(short, long)]
        count: bool,
        /// Filter by list ID (see `lists`)
        #[arg(long)]
        list: Option<String>,
        #[arg(long, value_enum)]
        status: Option<Status>,
        /// Only open tasks tagged Next
        #[arg(long)]
        next: bool,
    },
    /// Create a task in Inbox unless --list is specified
    Add {
        title: String,
        #[arg(short = 'D', long)]
        description: Option<String>,
        /// Due date in days from now
        #[arg(short, long, conflicts_with = "date")]
        due: Option<u64>,
        /// Due date (YYYY-MM-DD)
        #[arg(long)]
        date: Option<NaiveDate>,
        #[arg(long)]
        list: Option<String>,
    },
    /// Show a task by stable ID (or legacy local numeric ID)
    Show {
        id: String,
    },
    /// Update a task without changing unspecified fields
    Edit {
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, conflicts_with = "clear_description")]
        description: Option<String>,
        #[arg(long)]
        clear_description: bool,
        #[arg(long, conflicts_with = "clear_date")]
        date: Option<NaiveDate>,
        #[arg(long)]
        clear_date: bool,
    },
    Complete {
        id: String,
        #[arg(long)]
        note: Option<String>,
    },
    Reopen {
        id: String,
    },
    /// Replace or clear a task's completion note
    Note {
        id: String,
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        text: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// Set (or clear) the Next tag
    Next {
        id: String,
        #[arg(long)]
        clear: bool,
    },
    /// Move a task to another list without copying it
    Move {
        id: String,
        list: String,
    },
    /// Move a task to Trash (reversible)
    #[command(visible_alias = "archive")]
    Delete {
        id: String,
        #[arg(long)]
        yes: bool,
    },
    /// Restore a task from Trash
    Restore {
        id: String,
    },
    /// Permanently delete a task already in Trash
    Purge {
        id: String,
        #[arg(long, required = true)]
        yes: bool,
    },
    /// Show trashed lists and tasks
    Trash,
    /// Pin a task, or update the avatar on an existing pin
    Pin {
        id: String,
        #[arg(long, default_value = "me")]
        agent: String,
        #[arg(long)]
        avatar: Option<String>,
    },
    Unpin {
        id: String,
        #[arg(long, default_value = "me")]
        agent: String,
    },
    /// Show pinned tasks across lists
    Pinned {
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show overdue, due today, and Next tasks
    Today,
    /// Browse or manage lists
    Lists {
        #[command(subcommand)]
        action: Option<ListAction>,
    },
    /// Show the dedicated Inbox list
    Inbox,
    /// Synchronize with an optional SSH master
    #[command(args_conflicts_with_subcommands = true)]
    Sync {
        /// Apply the reviewed first-import preview
        #[arg(long, conflicts_with = "stdio")]
        accept_import: bool,
        #[arg(long, hide = true)]
        stdio: bool,
        #[command(subcommand)]
        action: Option<crate::sync::SyncAction>,
    },
}

#[derive(Subcommand)]
pub enum ListAction {
    Create {
        title: String,
    },
    Rename {
        id: String,
        title: String,
    },
    /// Move a list and its tasks to Trash (Inbox is protected)
    #[command(visible_alias = "archive")]
    Delete {
        id: String,
        #[arg(long)]
        yes: bool,
    },
    /// Restore a list and any of its tasks not trashed individually
    Restore {
        id: String,
    },
    /// Permanently delete a trashed list and all its tasks
    Purge {
        id: String,
        #[arg(long, required = true)]
        yes: bool,
    },
}

#[derive(Serialize)]
struct Trash {
    lists: Vec<TodoList>,
    tasks: Vec<Todo>,
}

fn output<T: Serialize>(json: bool, value: &T, human: impl FnOnce(&T)) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(value)?);
    } else {
        human(value);
    }
    Ok(())
}

fn task_line(t: &Todo) {
    let status = if t.completed {
        " [done]"
    } else if t.next {
        " [Next]"
    } else {
        ""
    };
    let pins = t
        .pins
        .iter()
        .map(|p| format!("{} {}", p.avatar, p.agent))
        .collect::<Vec<_>>()
        .join(", ");
    let pins = if pins.is_empty() {
        pins
    } else {
        format!(" [{pins}]")
    };
    println!(
        "{}\t{}\t{}\t{}{}{}",
        t.uid,
        t.list_title,
        t.due_date
            .map(|d| d.to_string())
            .unwrap_or_else(|| "-".into()),
        t.title,
        status,
        pins
    );
}

fn task_detail(t: &Todo) {
    task_line(t);
    if let Some(date) = &t.trashed_at {
        println!("Trashed: {date}");
    }
    if let Some(date) = &t.list_trashed_at {
        println!("Parent list trashed: {date}");
    }
    if let Some(description) = &t.description {
        println!("Description: {description}");
    }
    if let Some(date) = &t.completed_date {
        println!("Completed: {date}");
    }
    if let Some(note) = &t.completion_note {
        println!("Completion note: {note}");
    }
}

fn list_line(list: &TodoList) {
    let status = if list.trashed_at.is_some() {
        " [Trash]"
    } else {
        ""
    };
    println!("{}\t{}{status}", list.uid, list.title);
}

pub fn days_from_now(days: u64) -> Result<NaiveDate> {
    Local::now()
        .date_naive()
        .checked_add_days(Days::new(days))
        .ok_or_else(|| "due date is out of range".into())
}

impl Cli {
    pub fn run(self) -> Result<()> {
        let Some(command) = self.command else {
            return if self.json {
                Err("--json requires a command".into())
            } else {
                crate::tui::run()
            };
        };
        let json = self.json;
        if let Command::Sync {
            action,
            accept_import,
            stdio,
        } = command
        {
            return crate::sync::cli(action, accept_import, stdio, json);
        }
        let mut conn = db::open_db()?;
        match command {
            Command::List {
                date,
                count,
                list,
                status,
                next,
            } => {
                let legacy = (!json || count) && list.is_none() && status.is_none() && !next;
                let rows = db::tasks(
                    &conn,
                    Query {
                        due: if legacy {
                            Some(date.unwrap_or_else(|| Local::now().date_naive()))
                        } else {
                            date
                        },
                        list: list.as_deref(),
                        next,
                        completed: match status {
                            Some(Status::Completed) => Some(true),
                            Some(Status::Open) => Some(false),
                            None if legacy => Some(false),
                            _ => None,
                        },
                        ..Query::default()
                    },
                )?;
                return if count {
                    output(json, &rows.len(), |n| println!("{n}"))
                } else {
                    output(json, &rows, |tasks| {
                        for t in tasks {
                            if legacy {
                                if let Some(date) = t.due_date {
                                    println!("{}\t{}\t{:?}", t.id.unwrap(), date, t.title);
                                }
                            } else {
                                task_line(t);
                            }
                        }
                    })
                };
            }
            Command::Add {
                title,
                description,
                due,
                date,
                list,
            } => {
                let task = db::create_task(
                    &mut conn,
                    list.as_deref(),
                    &title,
                    description.as_deref(),
                    due.map(days_from_now).transpose()?.or(date),
                )?;
                return output(json, &task, |t| {
                    println!("✓ Added todo '{}' to {}", t.title, t.list_title);
                    if let Some(date) = t.due_date {
                        println!("  Due: {date}");
                    }
                    println!("  ID: {}", t.uid);
                });
            }
            Command::Show { id } => return output(json, &db::show(&conn, &id)?, task_detail),
            Command::Pinned { agent } => {
                return output(
                    json,
                    &db::tasks(
                        &conn,
                        Query {
                            pinned: true,
                            agent: agent.as_deref(),
                            ..Query::default()
                        },
                    )?,
                    |tasks| tasks.iter().for_each(task_line),
                )
            }
            Command::Today => {
                return output(
                    json,
                    &db::tasks(
                        &conn,
                        Query {
                            today: true,
                            ..Query::default()
                        },
                    )?,
                    |tasks| tasks.iter().for_each(task_line),
                )
            }
            Command::Inbox => return output(json, &db::inbox(&conn)?, list_line),
            Command::Trash => {
                let read = conn.transaction()?;
                let trash = Trash {
                    lists: db::lists(&read, true)?,
                    tasks: db::tasks(
                        &read,
                        Query {
                            trash: TrashFilter::Trashed,
                            ..Query::default()
                        },
                    )?,
                };
                return output(json, &trash, |trash| {
                    println!("Trashed lists:");
                    trash.lists.iter().for_each(list_line);
                    println!("Trashed tasks:");
                    trash.tasks.iter().for_each(task_line);
                });
            }
            Command::Lists { action } => {
                return match action {
                    None => output(json, &db::lists(&conn, false)?, |lists| {
                        lists.iter().for_each(list_line)
                    }),
                    Some(ListAction::Create { title }) => {
                        output(json, &db::create_list(&mut conn, &title)?, list_line)
                    }
                    Some(ListAction::Rename { id, title }) => {
                        output(json, &db::rename_list(&mut conn, &id, &title)?, list_line)
                    }
                    Some(action) => {
                        let (id, action) = match action {
                            ListAction::Delete { id, .. } => (id, TrashAction::Delete),
                            ListAction::Restore { id } => (id, TrashAction::Restore),
                            ListAction::Purge { id, .. } => (id, TrashAction::Purge),
                            _ => unreachable!(),
                        };
                        output(
                            json,
                            &db::change_list_trash(&mut conn, &id, action)?,
                            list_line,
                        )
                    }
                };
            }
            command => {
                let task = match &command {
                    Command::Edit {
                        id,
                        title,
                        description,
                        clear_description,
                        date,
                        clear_date,
                    } => (
                        id,
                        Change::Edit {
                            title: title.as_deref(),
                            description: if *clear_description {
                                Some(None)
                            } else {
                                description.as_deref().map(Some)
                            },
                            due: if *clear_date {
                                Some(None)
                            } else {
                                date.map(Some)
                            },
                        },
                    ),
                    Command::Complete { id, note } => (id, Change::Complete(note.as_deref())),
                    Command::Reopen { id } => (id, Change::Reopen),
                    Command::Note { id, text, .. } => (id, Change::Note(text.as_deref())),
                    Command::Next { id, clear } => (id, Change::Next(!clear)),
                    Command::Move { id, list } => (id, Change::Move(list)),
                    Command::Delete { id, .. } => (id, Change::Trash(TrashAction::Delete)),
                    Command::Restore { id } => (id, Change::Trash(TrashAction::Restore)),
                    Command::Purge { id, .. } => (id, Change::Trash(TrashAction::Purge)),
                    Command::Pin { id, agent, avatar } => (
                        id,
                        Change::Pin {
                            agent,
                            avatar: avatar.as_deref(),
                        },
                    ),
                    Command::Unpin { id, agent } => (id, Change::Unpin(agent)),
                    _ => unreachable!(),
                };
                return output(
                    json,
                    &db::change_task(&mut conn, task.0, task.1)?,
                    task_detail,
                );
            }
        }
    }
}
