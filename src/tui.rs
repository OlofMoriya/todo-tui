use std::{
    io,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use chrono::NaiveDate;
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
    Frame, Terminal,
};
use rusqlite::Connection;

use crate::{
    database::{self as db, Change, Query, Result, TrashAction, TrashFilter},
    model::{Pin, Todo, TodoList},
    snapshot::Record,
    sync_log::{self, Event as SyncEvent, Kind as SyncKind},
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum View {
    Pinned,
    #[default]
    Lists,
    Today,
    Trash,
}

impl View {
    const ALL: [Self; 3] = [Self::Pinned, Self::Lists, Self::Today];
    const TITLES: [&'static str; 3] = ["Pinned", "Lists", "Today"];
    fn has_list_pane(self) -> bool {
        matches!(self, Self::Lists | Self::Trash)
    }
    fn index(self) -> usize {
        Self::ALL.iter().position(|v| *v == self).unwrap()
    }
    fn cycle(self, reverse: bool) -> Self {
        let n = Self::ALL.len();
        Self::ALL[(self.index() + if reverse { n - 1 } else { 1 }) % n]
    }
}

struct Row {
    task: Todo,
    pin: Option<Pin>,
}
impl Row {
    fn key(&self) -> (&str, Option<&str>) {
        (&self.task.uid, self.pin.as_ref().map(|p| p.agent.as_str()))
    }
}

#[derive(Default)]
struct Ui {
    view: View,
    trash_return: Option<BrowserPosition>,
    on_lists: bool,
    show_details: bool,
    lists: Vec<TodoList>,
    rows: Vec<Row>,
    lists_state: ListState,
    tasks_state: ListState,
    form: Option<Form>,
    status: String,
    sync_status: String,
    sync_notice: String,
    activity: Option<Activity>,
}

struct BrowserPosition {
    view: View,
    on_lists: bool,
    lists_state: ListState,
    tasks_state: ListState,
    lists: Vec<TodoList>,
    rows: Vec<Row>,
}

#[derive(Default)]
struct Activity {
    events: Vec<SyncEvent>,
    list: ListState,
    scroll: u16,
}

impl Activity {
    fn selected(&self) -> Option<&SyncEvent> {
        self.events.get(self.list.selected()?)
    }

    fn load(&mut self, conn: &Connection) -> Result<()> {
        let events = sync_log::recent(conn, 50)?;
        let selected = keep(
            self.selected().map(|e| e.id),
            events.iter().map(|e| e.id),
            self.list.selected(),
        );
        self.events = events;
        self.list.select(selected);
        Ok(())
    }

    fn key(&mut self, conn: &Connection, key: KeyCode) -> Result<()> {
        if self.events.is_empty() {
            return Ok(());
        }
        let current = self.list.selected().unwrap_or(0);
        let index = match key {
            KeyCode::Up | KeyCode::Char('k') => current.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => (current + 1).min(self.events.len() - 1),
            KeyCode::Home => 0,
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(5);
                return Ok(());
            }
            KeyCode::PageDown => {
                let limit = self
                    .selected()
                    .map_or(0, |e| activity_details(e).lines().count().saturating_sub(1));
                self.scroll = self
                    .scroll
                    .saturating_add(5)
                    .min(u16::try_from(limit).unwrap_or(u16::MAX));
                return Ok(());
            }
            _ => return Ok(()),
        };
        if index != current {
            self.scroll = 0;
        }
        self.list.select(Some(index));
        if index == 0 {
            sync_log::mark_seen(conn, self.events[0].id)?;
        }
        Ok(())
    }
}

impl Ui {
    fn list(&self) -> Option<&TodoList> {
        self.lists.get(self.lists_state.selected()?)
    }
    fn row(&self) -> Option<&Row> {
        self.rows.get(self.tasks_state.selected()?)
    }
    fn refresh(&mut self, conn: &Connection) -> Result<()> {
        let read = conn.unchecked_transaction()?;
        let lists = db::lists(&read, self.view == View::Trash)?;
        if self.view == View::Trash && lists.is_empty() {
            self.on_lists = false;
        }
        let list_index = keep(
            self.list().map(|l| l.uid.as_str()),
            lists.iter().map(|l| l.uid.as_str()),
            self.lists_state.selected(),
        );
        let tasks = match self.view {
            View::Lists => match list_index {
                Some(i) => db::tasks(
                    &read,
                    Query {
                        list: Some(&lists[i].uid),
                        ..Query::default()
                    },
                )?,
                None => vec![],
            },
            View::Pinned => db::tasks(
                &read,
                Query {
                    pinned: true,
                    ..Query::default()
                },
            )?,
            View::Trash => db::tasks(
                &read,
                Query {
                    trash: TrashFilter::Trashed,
                    ..Query::default()
                },
            )?,
            View::Today => db::tasks(
                &read,
                Query {
                    today: true,
                    ..Query::default()
                },
            )?,
        };
        read.commit()?;
        let rows = rows_for(self.view, tasks);
        let task_index = keep(
            self.row().map(Row::key),
            rows.iter().map(Row::key),
            self.tasks_state.selected(),
        );
        self.lists = lists;
        self.rows = rows;
        self.lists_state.select(list_index);
        self.tasks_state.select(task_index);
        Ok(())
    }

    fn refresh_activity(&mut self, conn: &Connection) -> Result<()> {
        self.sync_notice = match sync_log::head(conn)? {
            Some(event) if event.entry.kind != SyncKind::Synced => {
                format!("{} • s review sync activity", event.entry.message)
            }
            Some(event) if event.id > sync_log::last_seen(conn)? => {
                format!("{} • s review new activity", event.entry.message)
            }
            _ => String::new(),
        };
        if let Some(activity) = &mut self.activity {
            activity.load(conn)?;
        }
        Ok(())
    }

    fn toggle_trash(&mut self) {
        if let Some(previous) = self.trash_return.take() {
            self.view = previous.view;
            self.on_lists = previous.on_lists;
            self.lists_state = previous.lists_state;
            self.tasks_state = previous.tasks_state;
            self.lists = previous.lists;
            self.rows = previous.rows;
        } else {
            self.trash_return = Some(BrowserPosition {
                view: self.view,
                on_lists: self.on_lists,
                lists_state: std::mem::take(&mut self.lists_state),
                tasks_state: std::mem::take(&mut self.tasks_state),
                lists: std::mem::take(&mut self.lists),
                rows: std::mem::take(&mut self.rows),
            });
            self.view = View::Trash;
            self.on_lists = true;
        }
        self.status.clear();
    }

    fn handle(&mut self, conn: &mut Connection, key: KeyEvent) -> Result<bool> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(false);
        }
        if let Some(form) = self.form.as_mut() {
            if key.code == KeyCode::Esc {
                self.form = None;
                self.status = "Cancelled".into();
            } else if form.key(key) {
                let message = form.apply(conn)?;
                self.status = message.into();
                self.form = None;
            }
            return Ok(true);
        }
        if let Some(activity) = &mut self.activity {
            match key.code {
                KeyCode::Char('q') => return Ok(false),
                KeyCode::Char('s') | KeyCode::Esc => self.activity = None,
                code => activity.key(conn, code)?,
            }
            return Ok(true);
        }
        let lists = self.view.has_list_pane() && self.on_lists;
        match key.code {
            KeyCode::Char('q') => return Ok(false),
            KeyCode::Char('v') => self.show_details = !self.show_details,
            KeyCode::Char('s') => {
                let mut activity = Activity::default();
                activity.load(conn)?;
                if let Some(event) = activity.selected() {
                    sync_log::mark_seen(conn, event.id)?;
                }
                self.activity = Some(activity);
            }
            KeyCode::Char('t') => self.toggle_trash(),
            KeyCode::Esc if self.view == View::Trash => self.toggle_trash(),
            KeyCode::Tab | KeyCode::BackTab if self.view != View::Trash => {
                self.view = self.view.cycle(
                    key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT),
                );
                self.tasks_state.select(None);
                self.status.clear();
            }
            KeyCode::Left | KeyCode::Char('h') if self.view.has_list_pane() => self.on_lists = true,
            KeyCode::Right | KeyCode::Char('l') if self.view.has_list_pane() => {
                self.on_lists = false
            }
            KeyCode::Up | KeyCode::Down | KeyCode::Char('j' | 'k') => {
                let down = matches!(key.code, KeyCode::Down | KeyCode::Char('j'));
                let (state, len) = if lists {
                    (&mut self.lists_state, self.lists.len())
                } else {
                    (&mut self.tasks_state, self.rows.len())
                };
                let old = state.selected().unwrap_or(0);
                let new = if down {
                    old.saturating_add(1)
                } else {
                    old.saturating_sub(1)
                };
                state.select((len != 0).then(|| new.min(len - 1)));
            }
            KeyCode::Char('R' | 'P') if self.view == View::Trash => {
                let restore = key.code == KeyCode::Char('R');
                if lists {
                    if let Some(list) = self.list() {
                        if restore {
                            db::change_list_trash(conn, &list.uid, TrashAction::Restore)?;
                            self.status = "List restored".into();
                        } else {
                            self.form = Some(Form::confirm_purge(
                                Action::PurgeList(list.uid.clone()),
                                &format!("{} and all its tasks", list.title),
                            ));
                        }
                    }
                } else if let Some(row) = self.row() {
                    if restore {
                        db::change_task(conn, &row.task.uid, Change::Trash(TrashAction::Restore))?;
                        self.status = "Task restored".into();
                    } else {
                        self.form = Some(Form::confirm_purge(
                            Action::PurgeTask(row.task.uid.clone()),
                            &row.task.title,
                        ));
                    }
                }
            }
            _ if self.view == View::Trash => (),
            KeyCode::Char('N') => {
                self.form = Some(Form::new(
                    Action::Add,
                    vec![
                        Field::text("Title", ""),
                        Field::text("Description (optional)", ""),
                        Field::text("Due YYYY-MM-DD (optional)", ""),
                        Field::choices("Destination", &self.lists),
                    ],
                ));
            }
            KeyCode::Char('L') => {
                self.form = Some(Form::new(
                    Action::CreateList,
                    vec![Field::text("List name", "")],
                ))
            }
            KeyCode::Char('R' | 'D') if lists => {
                if let Some(list) = self.list() {
                    if key.code == KeyCode::Char('R') {
                        self.form = Some(Form::new(
                            Action::RenameList(list.uid.clone()),
                            vec![Field::text("List name", &list.title)],
                        ));
                    } else {
                        db::change_list_trash(conn, &list.uid, TrashAction::Delete)?;
                        self.status = "List moved to Trash".into();
                    }
                }
            }
            KeyCode::Char(c) if !lists => {
                let Some(row) = self.row() else {
                    return Ok(true);
                };
                let task = &row.task;
                let id = task.uid.clone();
                let pin = row
                    .pin
                    .as_ref()
                    .or_else(|| task.pins.iter().find(|p| p.agent == "me"));
                let agent = pin.map_or("me", |p| p.agent.as_str());
                self.form = match c {
                    'E' => Some(Form::new(
                        Action::Edit(id),
                        vec![
                            Field::text("Title", &task.title),
                            Field::text(
                                "Description (blank clears)",
                                task.description.as_deref().unwrap_or_default(),
                            ),
                            Field::text(
                                "Due YYYY-MM-DD (blank clears)",
                                &task.due_date.map(|d| d.to_string()).unwrap_or_default(),
                            ),
                        ],
                    )),
                    'm' => Some(Form::new(
                        Action::Move(id),
                        vec![Field::choices("Destination", &self.lists)],
                    )),
                    'p' => Some(Form::new(
                        Action::Pin(id),
                        vec![
                            Field::text("Human or agent", agent),
                            Field::text(
                                "Avatar (optional, e.g. Pikachu)",
                                pin.map_or("", |p| p.avatar.as_str()),
                            ),
                        ],
                    )),
                    'u' => Some(Form::new(
                        Action::Unpin(id),
                        vec![Field::text("Human or agent", agent)],
                    )),
                    'o' => Some(Form::new(
                        Action::Note(id),
                        vec![Field::text(
                            "Completion note (blank clears)",
                            task.completion_note.as_deref().unwrap_or_default(),
                        )],
                    )),
                    ' ' => {
                        let (change, status) = if task.completed {
                            (Change::Reopen, "Reopened")
                        } else {
                            (Change::Complete(None), "Completed")
                        };
                        db::change_task(conn, &id, change)?;
                        self.status = status.into();
                        None
                    }
                    'n' => {
                        db::change_task(conn, &id, Change::Next(!task.next))?;
                        self.status = "Next tag updated".into();
                        None
                    }
                    'D' => {
                        db::change_task(conn, &id, Change::Trash(TrashAction::Delete))?;
                        self.status = "Task moved to Trash".into();
                        None
                    }
                    _ => None,
                };
            }
            _ => (),
        }
        Ok(true)
    }
}

fn keep<T: PartialEq>(
    key: Option<T>,
    candidates: impl Iterator<Item = T> + ExactSizeIterator,
    old: Option<usize>,
) -> Option<usize> {
    let len = candidates.len();
    if len == 0 {
        return None;
    }
    key.and_then(|k| candidates.into_iter().position(|v| v == k))
        .or_else(|| Some(old.unwrap_or(0).min(len - 1)))
}

fn rows_for(view: View, tasks: Vec<Todo>) -> Vec<Row> {
    if view != View::Pinned {
        return tasks
            .into_iter()
            .map(|task| Row { task, pin: None })
            .collect();
    }
    let mut rows = tasks
        .into_iter()
        .flat_map(|task| {
            task.pins.clone().into_iter().map(move |pin| Row {
                task: task.clone(),
                pin: Some(pin),
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        a.pin
            .as_ref()
            .unwrap()
            .agent
            .cmp(&b.pin.as_ref().unwrap().agent)
    });
    rows
}

struct Form {
    action: Action,
    fields: Vec<Field>,
    at: usize,
}
enum Action {
    Add,
    CreateList,
    RenameList(String),
    PurgeList(String),
    Edit(String),
    Move(String),
    Pin(String),
    Unpin(String),
    Note(String),
    PurgeTask(String),
}

struct Field {
    label: String,
    input: Input,
}
enum Input {
    Text(String),
    Choice(Vec<(String, String)>, usize),
}
impl Field {
    fn text(label: &str, initial: &str) -> Self {
        Self {
            label: label.into(),
            input: Input::Text(initial.into()),
        }
    }
    fn choices(label: &str, lists: &[TodoList]) -> Self {
        Self {
            label: label.into(),
            input: Input::Choice(
                lists
                    .iter()
                    .map(|l| (l.title.clone(), l.uid.clone()))
                    .collect(),
                0,
            ),
        }
    }
    fn value(&self) -> Option<&str> {
        match &self.input {
            Input::Text(text) => Some(text),
            Input::Choice(values, at) => values.get(*at).map(|(_, id)| id.as_str()),
        }
    }
    fn display(&self) -> String {
        match &self.input {
            Input::Text(text) => safe(text),
            Input::Choice(values, at) => values.get(*at).map_or("No lists".into(), |(title, _)| {
                format!("‹ {} ›  ({}/{})", safe(title), at + 1, values.len())
            }),
        }
    }
}
impl Form {
    fn new(action: Action, fields: Vec<Field>) -> Self {
        Self {
            action,
            fields,
            at: 0,
        }
    }
    fn confirm_purge(action: Action, title: &str) -> Self {
        Self::new(
            action,
            vec![Field::text(
                &format!("Permanently delete ‘{}’? Type yes", safe(title)),
                "",
            )],
        )
    }
    fn paste(&mut self, text: &str) {
        if let Input::Text(input) = &mut self.fields[self.at].input {
            input.push_str(text);
        }
    }
    fn key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Enter if self.at + 1 == self.fields.len() => return true,
            KeyCode::Enter | KeyCode::Down => self.at = (self.at + 1).min(self.fields.len() - 1),
            KeyCode::Up => self.at = self.at.saturating_sub(1),
            code => match &mut self.fields[self.at].input {
                Input::Text(text) => match code {
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        text.push(c)
                    }
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        text.clear()
                    }
                    _ => (),
                },
                Input::Choice(values, at) if !values.is_empty() => match code {
                    KeyCode::Left | KeyCode::Char('h') => {
                        *at = (*at + values.len() - 1) % values.len()
                    }
                    KeyCode::Right | KeyCode::Char('l') => *at = (*at + 1) % values.len(),
                    _ => (),
                },
                _ => (),
            },
        }
        false
    }
    fn apply(&self, conn: &mut Connection) -> Result<&'static str> {
        let v = |n: usize| self.fields[n].value().ok_or("Choose a list");
        let optional = |n| v(n).map(|s| (!s.is_empty()).then_some(s));
        let date = |n| -> Result<Option<NaiveDate>> {
            Ok(optional(n)?
                .map(str::parse)
                .transpose()
                .map_err(|_| "Enter a due date as YYYY-MM-DD, or leave it blank")?)
        };
        let confirmed = || -> Result<()> {
            if v(0)? == "yes" {
                Ok(())
            } else {
                Err("Type yes to confirm (Esc cancels)".into())
            }
        };
        let (id, change, message) = match &self.action {
            Action::Add => {
                db::create_task(conn, Some(v(3)?), v(0)?, optional(1)?, date(2)?)?;
                return Ok("Task added");
            }
            Action::CreateList => {
                db::create_list(conn, v(0)?)?;
                return Ok("List created");
            }
            Action::RenameList(id) => {
                db::rename_list(conn, id, v(0)?)?;
                return Ok("List renamed");
            }
            Action::PurgeList(id) => {
                confirmed()?;
                db::change_list_trash(conn, id, TrashAction::Purge)?;
                return Ok("List and its tasks permanently deleted");
            }
            Action::Edit(id) => (
                id,
                Change::Edit {
                    title: Some(v(0)?),
                    description: Some(optional(1)?),
                    due: Some(date(2)?),
                },
                "Task updated",
            ),
            Action::Move(id) => (id, Change::Move(v(0)?), "Task moved"),
            Action::Pin(id) => (
                id,
                Change::Pin {
                    agent: v(0)?,
                    avatar: optional(1)?,
                },
                "Pinned",
            ),
            Action::Unpin(id) => (id, Change::Unpin(v(0)?), "Unpinned"),
            Action::Note(id) => (id, Change::Note(optional(0)?), "Note updated"),
            Action::PurgeTask(id) => {
                confirmed()?;
                (
                    id,
                    Change::Trash(TrashAction::Purge),
                    "Task permanently deleted",
                )
            }
        };
        db::change_task(conn, id, change)?;
        Ok(message)
    }
}

fn safe(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn safe_multiline(text: &str) -> String {
    text.lines().map(safe).collect::<Vec<_>>().join("\n")
}

fn task_label(t: &Todo) -> String {
    let status = if t.completed {
        "✓ "
    } else if t.next {
        "[Next] "
    } else {
        ""
    };
    let pin = if t.pins.is_empty() { "" } else { "● " };
    let due = t.due_date.map(|d| format!(" ({d})")).unwrap_or_default();
    format!("{pin}{status}{}{due}", safe(&t.title))
}

fn detail(t: &Todo) -> String {
    let mut text = format!(
        "{}\nList: {}\nID: {}",
        task_label(t),
        safe(&t.list_title),
        t.uid
    );
    if let Some(at) = &t.trashed_at {
        text.push_str(&format!("\nTrashed: {}", safe(at)));
    }
    if t.list_trashed_at.is_some() {
        text.push_str("\nParent list is in Trash");
    }
    for p in &t.pins {
        text.push_str(&format!(
            "\nWorking: {} {}",
            safe(&p.avatar),
            safe(&p.agent)
        ));
    }
    if let Some(s) = &t.description {
        text.push_str(&format!("\n\n{}", safe_multiline(s)));
    }
    if let Some(d) = t.completed_date {
        text.push_str(&format!("\n\nCompleted: {d}"));
    }
    if let Some(s) = &t.completion_note {
        text.push_str(&format!("\nCompletion note: {}", safe_multiline(s)));
    }
    text
}

fn activity_details(event: &SyncEvent) -> String {
    let mut text = format!(
        "{}\n{}",
        safe(&event.at),
        safe_multiline(&event.entry.message)
    );
    for (direction, changes) in [
        ("From mini", &event.entry.downloaded),
        ("To mini", &event.entry.uploaded),
    ] {
        for change in changes {
            let title = match change.after.as_ref().or(change.before.as_ref()) {
                Some(Record::List { title, .. } | Record::Task { title, .. }) => safe(title),
                None => String::new(),
            };
            let kind = match (&change.before, &change.after) {
                (None, _) => "added",
                (_, None) => "removed",
                _ => "updated",
            };
            text.push_str(&format!(
                "\n\n{direction}: {kind} {title}\n{}",
                safe(&change.key)
            ));
            let before = serde_json::to_value(&change.before).unwrap_or_default();
            let after = serde_json::to_value(&change.after).unwrap_or_default();
            let fields = before.as_object().or_else(|| after.as_object());
            if let Some(fields) = fields {
                for field in fields.keys().filter(|key| key.as_str() != "kind") {
                    let old = &before[field];
                    let new = &after[field];
                    if old == new {
                        continue;
                    }
                    match (&change.before, &change.after) {
                        (None, _) => text.push_str(&format!("\n  {field}: {new}")),
                        (_, None) => text.push_str(&format!("\n  {field}: {old}")),
                        _ => text.push_str(&format!("\n  {field}: {old} → {new}")),
                    }
                }
            }
        }
    }
    text
}

fn draw_activity<B: Backend>(
    frame: &mut Frame<B>,
    area: ratatui::layout::Rect,
    activity: &mut Activity,
    accent: Style,
) {
    let panes = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height / 3).max(3).min(area.height)),
            Constraint::Min(0),
        ])
        .split(area);
    let rows: Vec<_> = activity
        .events
        .iter()
        .map(|e| ListItem::new(format!("{}  {}", safe(&e.at), safe(&e.entry.message))))
        .collect();
    frame.render_stateful_widget(
        List::new(rows)
            .block(
                Block::default()
                    .title("Sync activity")
                    .borders(Borders::ALL),
            )
            .highlight_symbol("› ")
            .highlight_style(accent),
        panes[0],
        &mut activity.list,
    );
    let detail = activity
        .selected()
        .map_or_else(|| "No sync activity yet".into(), activity_details);
    frame.render_widget(
        Paragraph::new(detail)
            .block(Block::default().title("Sync review").borders(Borders::ALL))
            .scroll((activity.scroll, 0))
            .wrap(Wrap { trim: false }),
        panes[1],
    );
}

fn agent_color(agent: &str) -> Color {
    let palette = [
        Color::LightGreen,
        Color::LightCyan,
        Color::LightMagenta,
        Color::LightYellow,
    ];
    let hash = agent.bytes().fold(0usize, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte.into())
    });
    palette[hash % palette.len()]
}

fn agent_display(agent: &str) -> String {
    if let Some((project, session)) = agent.strip_prefix("Pi/").and_then(|s| s.rsplit_once('/')) {
        return format!("Pi / {}  #{}", safe(project), safe(session));
    }
    safe(agent)
}

fn draw_focus_board<B: Backend>(frame: &mut Frame<B>, area: Rect, ui: &mut Ui) {
    let neon = Style::default()
        .fg(Color::LightGreen)
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(Color::Rgb(137, 186, 177));
    let panel = Color::Rgb(8, 25, 26);
    let board = Block::default()
        .title(Span::styled(" ◈ FOCUS BOARD // AGENT CONSOLE ", neon))
        .style(Style::default().bg(panel))
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Rgb(40, 123, 101)));
    let area = {
        let inside = board.inner(area);
        frame.render_widget(board, area);
        inside
    };
    if area.width == 0 || area.height == 0 {
        return;
    }
    if ui.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("  ◇  STANDBY // No pinned work", neon),
                Line::styled(
                    "     Press p on a task to claim it, or have Pi pin its next work item.",
                    muted,
                ),
            ])
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let mut groups = std::collections::BTreeMap::new();
    for pin in ui.rows.iter().filter_map(|row| row.pin.as_ref()) {
        *groups.entry(pin.agent.as_str()).or_insert(0usize) += 1;
    }
    let mut list_area = area;
    if area.height >= 6 {
        let summary = Line::from(vec![
            Span::styled("  ▰▰▱  ", neon),
            Span::styled(format!("{:02} PINS", ui.rows.len()), neon),
            Span::styled(format!("   //   {:02} IDENTITIES", groups.len()), muted),
        ]);
        frame.render_widget(Paragraph::new(summary), Rect { height: 1, ..area });
        list_area.y += 2;
        list_area.height -= 2;
    }
    let compact = list_area.height < 5;
    let selected = ui.tasks_state.selected();
    let mut previous = None;
    let items = ui
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let mut lines = Vec::new();
            let Some(pin) = &row.pin else {
                return ListItem::new(task_label(&row.task));
            };
            let color = agent_color(&pin.agent);
            let badge = Style::default().fg(color).add_modifier(Modifier::BOLD);
            let avatar = safe(&pin.avatar);
            let avatar = if avatar.trim().is_empty() {
                "◇"
            } else {
                &avatar
            };
            let identity = agent_display(&pin.agent);
            let mut title = vec![
                Span::styled(
                    format!(" ⟦ {avatar} ⟧  "),
                    Style::default()
                        .fg(Color::Black)
                        .bg(color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    safe(&row.task.title),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ];
            if compact {
                title.push(Span::styled(format!("  //  {identity}"), muted));
            } else if previous != Some(pin.agent.as_str()) {
                if previous.is_some() && list_area.height >= 8 {
                    lines.push(Line::default());
                }
                lines.push(Line::from(vec![
                    Span::styled(" ╭─◈  ", badge),
                    Span::styled(identity, badge),
                    Span::styled(
                        format!("  //  {:02} PINS", groups[pin.agent.as_str()]),
                        muted,
                    ),
                ]));
            }
            previous = Some(pin.agent.as_str());
            lines.push(Line::from(title));
            if !compact {
                let mut meta = vec![Span::styled(
                    format!(" ╰─▸  PINNED  /  {}", safe(&row.task.list_title)),
                    muted,
                )];
                if let Some(due) = row.task.due_date {
                    meta.push(Span::styled(format!("  /  DUE {due}"), muted));
                }
                if row.task.next {
                    meta.push(Span::styled("  ·  NEXT", badge));
                }
                lines.push(Line::from(meta));
            }
            let surface = if selected == Some(index) {
                Color::Rgb(20, 60, 53)
            } else {
                panel
            };
            ListItem::new(lines).style(Style::default().fg(Color::White).bg(surface))
        })
        .collect::<Vec<_>>();
    frame.render_stateful_widget(
        List::new(items).style(neon).highlight_symbol("▌ "),
        list_area,
        &mut ui.tasks_state,
    );
}

fn draw<B: Backend>(frame: &mut Frame<B>, ui: &mut Ui) {
    let accent = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(5),
        ])
        .split(frame.size());
    let header = Block::default().title("todo-tui").borders(Borders::ALL);
    if ui.view == View::Trash {
        frame.render_widget(
            Paragraph::new(Line::styled("Trash", accent)).block(header),
            outer[0],
        );
    } else {
        frame.render_widget(
            Tabs::new(View::TITLES.map(Line::from).to_vec())
                .block(header)
                .select(ui.view.index())
                .highlight_style(accent),
            outer[0],
        );
    }
    if let Some(form) = &ui.form {
        let lines = form
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let text = format!(
                    "{} {}: {}",
                    if i == form.at { "›" } else { " " },
                    f.label,
                    f.display()
                );
                if i == form.at {
                    Line::styled(text, accent)
                } else {
                    Line::from(text)
                }
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().title("Edit").borders(Borders::ALL))
                .wrap(Wrap { trim: false }),
            outer[1],
        );
        frame.render_widget(
            Paragraph::new(format!(
                "↑↓ fields • ←→ choices • Enter next/save • Ctrl-U clear • Esc cancel\n{}",
                safe(&ui.status)
            ))
            .wrap(Wrap { trim: false }),
            outer[2],
        );
        return;
    }
    if let Some(activity) = &mut ui.activity {
        draw_activity(frame, outer[1], activity, accent);
        frame.render_widget(
            Paragraph::new("↑↓ events • PgUp/PgDn details • Home latest • s/Esc close"),
            outer[2],
        );
        return;
    }
    let (browser, details) = if ui.show_details {
        let height = outer[1].height;
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length((height / 3).max(5).min(height / 2)),
            ])
            .split(outer[1]);
        (split[0], Some(split[1]))
    } else {
        (outer[1], None)
    };
    let on_lists = ui.view.has_list_pane() && ui.on_lists;
    let task_area = if ui.view.has_list_pane() {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(22), Constraint::Min(0)])
            .split(browser);
        let lists = ui
            .lists
            .iter()
            .map(|l| ListItem::new(safe(&l.title)))
            .collect::<Vec<_>>();
        frame.render_stateful_widget(
            List::new(lists)
                .highlight_symbol("› ")
                .highlight_style(if on_lists { accent } else { Style::default() })
                .block(
                    Block::default()
                        .title(if ui.view == View::Trash {
                            "Trashed lists"
                        } else {
                            "Lists"
                        })
                        .borders(Borders::ALL),
                ),
            panes[0],
            &mut ui.lists_state,
        );
        panes[1]
    } else {
        browser
    };
    if ui.view == View::Pinned {
        draw_focus_board(frame, task_area, ui);
    } else {
        let items = ui
            .rows
            .iter()
            .map(|row| {
                let label = task_label(&row.task);
                ListItem::new(if ui.view == View::Trash {
                    format!("{label} · {}", safe(&row.task.list_title))
                } else {
                    label
                })
            })
            .collect::<Vec<_>>();
        let title = match ui.view {
            View::Lists => "Tasks",
            View::Today => "Priorities",
            View::Trash => "Trashed tasks",
            View::Pinned => unreachable!(),
        };
        frame.render_stateful_widget(
            List::new(items)
                .highlight_symbol("› ")
                .highlight_style(if on_lists { Style::default() } else { accent })
                .block(Block::default().title(title).borders(Borders::ALL)),
            task_area,
            &mut ui.tasks_state,
        );
    }
    if let Some(area) = details {
        frame.render_widget(
            Paragraph::new(
                ui.row()
                    .map(|r| detail(&r.task))
                    .unwrap_or_else(|| "No tasks".into()),
            )
            .block(Block::default().title("Details").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
            area,
        );
    }
    let actions = if ui.view == View::Trash {
        "R restore • P permanently delete"
    } else if on_lists {
        "L new list • R rename • D trash • N new task"
    } else {
        "N add • E edit • p pin • u unpin • n Next • m move • Space done/reopen • o note • D trash"
    };
    let navigation = if ui.view == View::Trash {
        "t/Esc back"
    } else {
        "Tab/Shift-Tab views • t trash"
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{navigation} • hjkl/↑↓ navigate • v details • s activity • S sync • q quit\n{actions}\n{}\n{}",
            safe(&ui.status),
            safe(if ui.sync_notice.is_empty() { &ui.sync_status } else { &ui.sync_notice })
        ))
        .wrap(Wrap { trim: false }),
        outer[2],
    );
}

struct Tty;
impl Drop for Tty {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
    }
}

struct SyncWorker {
    request: mpsc::Sender<crate::sync::Mode>,
    responses: mpsc::Receiver<(Option<String>, u64)>,
    busy: bool,
    next: Instant,
}

impl SyncWorker {
    fn new() -> Self {
        use crate::sync::{self, Mode};
        let (request, jobs) = mpsc::channel();
        let (results, responses) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(mode) = jobs.recv() {
                let mut interval = crate::sync_background::DEFAULT_INTERVAL_SECS;
                let outcome = (|| -> Result<Option<String>> {
                    let (ready, configured_interval) = sync::automatic_settings()?;
                    interval = configured_interval;
                    if mode == Mode::Automatic && (!ready || crate::sync_background::active()) {
                        return Ok(None);
                    }
                    Ok(Some(sync::run(mode)?.message()))
                })()
                .unwrap_or_else(|e| Some(format!("SSH sync: {e}")));
                if results.send((outcome, interval)).is_err() {
                    break;
                }
            }
        });
        Self {
            request,
            responses,
            busy: false,
            next: Instant::now(),
        }
    }

    fn tick(&mut self, status: &mut String, manual: bool) {
        if let Ok((message, interval)) = self.responses.try_recv() {
            self.busy = false;
            self.next = Instant::now() + Duration::from_secs(interval);
            if let Some(message) = message {
                *status = message;
            }
        }
        if !self.busy && (manual || Instant::now() >= self.next) {
            let mode = if manual {
                crate::sync::Mode::Normal
            } else {
                crate::sync::Mode::Automatic
            };
            self.busy = self.request.send(mode).is_ok();
            if manual && self.busy {
                *status = "Checking SSH sync…".into();
            }
        }
    }
}

fn data_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA data_version", [], |row| row.get(0))
}

const LIVE_REFRESH_INTERVAL: Duration = Duration::from_millis(500);

struct LiveRefresh {
    version: Option<i64>,
    last_check: Instant,
}

impl LiveRefresh {
    fn new() -> Self {
        Self {
            version: None,
            last_check: Instant::now(),
        }
    }

    fn check(&mut self, ui: &mut Ui, conn: &Connection, input_changed: bool) -> Result<()> {
        let now = Instant::now();
        let latest = data_version(conn)?;
        let external_write = self.version != Some(latest);
        // A bounded re-query is also useful without version changes (e.g.
        // Today depends on the date, and data_version ignores local writes).
        if input_changed
            || external_write
            || now.duration_since(self.last_check) >= LIVE_REFRESH_INTERVAL
        {
            ui.refresh(conn)?;
            if input_changed || external_write {
                ui.refresh_activity(conn)?;
            }
            self.version = Some(latest);
            self.last_check = now;
        }
        Ok(())
    }
}

pub fn run() -> Result<()> {
    let mut conn = db::open_db()?;
    sync_log::init(&conn)?;
    let mut ui = Ui {
        on_lists: true,
        ..Ui::default()
    };
    let mut refresh = LiveRefresh::new();
    refresh.check(&mut ui, &conn, false)?;
    enable_raw_mode()?;
    let _tty = Tty;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let mut sync = SyncWorker::new();
    loop {
        let mut input_changed = false;
        sync.tick(&mut ui.sync_status, false);
        // Poll even when there's no keyboard input: CLI and sync writes use
        // separate SQLite connections and don't send terminal input events.
        terminal.draw(|frame| draw(frame, &mut ui))?;
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        && key.code == KeyCode::Char('S')
                        && ui.form.is_none()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    sync.tick(&mut ui.sync_status, true);
                }
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    input_changed = true;
                    match ui.handle(&mut conn, key) {
                        Ok(false) => break,
                        Ok(true) => (),
                        Err(e) => ui.status = e.to_string(),
                    }
                }
                Event::Paste(text) => {
                    if let Some(form) = ui.form.as_mut() {
                        form.paste(&text);
                    }
                }
                _ => (),
            }
        }
        if let Err(e) = refresh.check(&mut ui, &conn, input_changed) {
            ui.status = format!("Refresh failed: {e}");
        } else if ui.status.starts_with("Refresh failed: ") {
            ui.status.clear();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn press(ui: &mut Ui, conn: &mut Connection, code: KeyCode) {
        assert!(ui
            .handle(conn, KeyEvent::new(code, KeyModifiers::NONE))
            .unwrap());
        ui.refresh(conn).unwrap();
    }

    #[test]
    fn another_sqlite_connection_invalidates_the_visible_view() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("todos.sqlite");
        let mut writer = Connection::open(&path).unwrap();
        db::migrate(&mut writer).unwrap();
        let reader = Connection::open(&path).unwrap();
        let mut ui = Ui::default();
        ui.refresh(&reader).unwrap();
        assert!(ui.rows.is_empty());
        let initial = data_version(&reader).unwrap();
        let task = db::create_task(&mut writer, None, "New from CLI", None, None).unwrap();
        let added = data_version(&reader).unwrap();
        assert_ne!(added, initial);
        ui.refresh(&reader).unwrap();
        assert_eq!(ui.row().unwrap().task.uid, task.uid);
        ui.view = View::Pinned;
        ui.refresh(&reader).unwrap();
        assert!(ui.rows.is_empty());
        db::change_task(
            &mut writer,
            &task.uid,
            Change::Pin {
                agent: "Agent",
                avatar: Some("Pikachu"),
            },
        )
        .unwrap();
        let pinned = data_version(&reader).unwrap();
        assert_ne!(pinned, added);
        ui.refresh(&reader).unwrap();
        assert_eq!(ui.row().unwrap().pin.as_ref().unwrap().avatar, "Pikachu");
        db::change_task(&mut writer, &task.uid, Change::Unpin("Agent")).unwrap();
        assert_ne!(data_version(&reader).unwrap(), pinned);
        ui.refresh(&reader).unwrap();
        assert!(ui.rows.is_empty());
    }

    #[test]
    fn idle_lists_refresh_across_writers_and_preserve_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("todos.sqlite");
        let mut reader = Connection::open(&path).unwrap();
        db::migrate(&mut reader).unwrap();
        sync_log::init(&reader).unwrap();
        let mut writer = Connection::open(&path).unwrap();
        let work = db::create_list(&mut writer, "Work").unwrap();
        let old = db::create_task(&mut writer, Some(&work.uid), "Original", None, None).unwrap();
        let mut ui = Ui::default();
        let mut refresh = LiveRefresh::new();
        refresh.check(&mut ui, &reader, false).unwrap();
        ui.lists_state.select(Some(1));
        refresh.check(&mut ui, &reader, true).unwrap();
        db::create_list(&mut writer, "A list above Work").unwrap();
        db::rename_list(&mut writer, &work.uid, "Work renamed").unwrap();
        let added =
            db::create_task(&mut writer, Some(&work.uid), "External task", None, None).unwrap();
        refresh.check(&mut ui, &reader, false).unwrap();
        assert_eq!(ui.list().unwrap().title, "Work renamed");
        assert_eq!(ui.row().unwrap().task.uid, old.uid);
        assert_eq!(ui.rows.len(), 2);

        let inbox = db::inbox(&writer).unwrap();
        let mut other = Ui::default();
        other.refresh(&writer).unwrap();
        db::change_task(&mut writer, &old.uid, Change::Move(&inbox.uid)).unwrap();
        other.lists_state.select(Some(2));
        other.refresh(&writer).unwrap();
        press(&mut other, &mut writer, KeyCode::Char(' '));
        refresh.check(&mut ui, &reader, false).unwrap();
        assert_eq!(ui.rows.len(), 1);
        assert_eq!(ui.row().unwrap().task.uid, added.uid);
        assert!(ui.row().unwrap().task.completed);

        // Same-connection writes do not change PRAGMA data_version.
        db::change_task(&mut reader, &added.uid, Change::Reopen).unwrap();
        assert_eq!(refresh.version, Some(data_version(&reader).unwrap()));
        refresh.last_check -= LIVE_REFRESH_INTERVAL;
        refresh.check(&mut ui, &reader, false).unwrap();
        assert!(!ui.row().unwrap().task.completed);
    }

    #[test]
    fn sync_activity_announces_and_reviews_downloads() {
        let mut conn = Connection::open_in_memory().unwrap();
        sync_log::init(&conn).unwrap();
        let change = crate::snapshot::Delta {
            key: "list:test".into(),
            before: Some(Record::List {
                title: "Before".into(),
                trashed_at: None,
            }),
            after: Some(Record::List {
                title: "After".into(),
                trashed_at: None,
            }),
        };
        sync_log::write(
            &conn,
            sync_log::Entry {
                kind: SyncKind::Synced,
                message: "Downloaded one change".into(),
                uploaded: vec![],
                downloaded: vec![change],
            },
        )
        .unwrap();
        let mut ui = Ui::default();
        ui.refresh_activity(&conn).unwrap();
        assert!(ui.sync_notice.contains("review new activity"));
        ui.handle(
            &mut conn,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        )
        .unwrap();
        let event = ui.activity.as_ref().unwrap().selected().unwrap();
        assert!(activity_details(event).contains("\"Before\" → \"After\""));
        ui.refresh_activity(&conn).unwrap();
        assert!(ui.sync_notice.is_empty());
        sync_log::write(
            &conn,
            sync_log::Entry::issue(SyncKind::Error, "Master offline".into()),
        )
        .unwrap();
        ui.refresh_activity(&conn).unwrap();
        assert!(ui.sync_notice.contains("Master offline"));
    }

    #[test]
    fn trash_browses_restores_and_confirms_purge() {
        let mut conn = Connection::open_in_memory().unwrap();
        db::migrate(&mut conn).unwrap();
        let task = db::create_task(&mut conn, None, "Task", None, None).unwrap();
        let mut ui = Ui::default();
        ui.refresh(&conn).unwrap();
        press(&mut ui, &mut conn, KeyCode::Char('D'));
        assert!(ui.rows.is_empty());
        press(&mut ui, &mut conn, KeyCode::Tab);
        assert_eq!(ui.view, View::Today);
        press(&mut ui, &mut conn, KeyCode::Tab);
        assert_eq!(ui.view, View::Pinned);
        press(&mut ui, &mut conn, KeyCode::Char('t'));
        assert_eq!(ui.view, View::Trash);
        press(&mut ui, &mut conn, KeyCode::Tab);
        assert_eq!(ui.view, View::Trash);
        press(&mut ui, &mut conn, KeyCode::Esc);
        assert_eq!(ui.view, View::Pinned);
        press(&mut ui, &mut conn, KeyCode::Char('t'));
        assert_eq!(ui.row().unwrap().task.uid, task.uid);
        press(&mut ui, &mut conn, KeyCode::Char('R'));
        assert!(ui.rows.is_empty());
        db::change_task(&mut conn, &task.uid, Change::Trash(TrashAction::Delete)).unwrap();
        ui.refresh(&conn).unwrap();
        press(&mut ui, &mut conn, KeyCode::Char('P'));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(ui.handle(&mut conn, enter).is_err());
        assert!(db::show(&conn, &task.uid).is_ok());
        ui.form.as_mut().unwrap().paste("yes");
        press(&mut ui, &mut conn, KeyCode::Enter);
        assert!(db::show(&conn, &task.uid).is_err());
    }

    #[test]
    fn details_are_hidden_by_default_and_toggle_below_the_browser() {
        let mut conn = Connection::open_in_memory().unwrap();
        db::migrate(&mut conn).unwrap();
        db::create_task(&mut conn, None, "Focus", Some("Task description"), None).unwrap();
        let mut ui = Ui {
            on_lists: true,
            ..Ui::default()
        };
        ui.refresh(&conn).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut render = |ui: &mut Ui| {
            terminal.draw(|f| draw(f, ui)).unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .chunks(100)
                .map(|line| {
                    line.iter()
                        .map(|cell| cell.symbol.as_str())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        let hidden = render(&mut ui);
        assert!(hidden[3].contains("Tasks"));
        assert!(!hidden.iter().any(|line| line.contains("Details")));
        press(&mut ui, &mut conn, KeyCode::Char('v'));
        let shown = render(&mut ui);
        let details = shown
            .iter()
            .position(|line| line.starts_with("┌Details"))
            .unwrap();
        assert!(details > 3);
        assert!(shown[details].ends_with('┐'));
        assert!(shown[details + 1..]
            .iter()
            .any(|line| line.contains("Task description")));
        press(&mut ui, &mut conn, KeyCode::Char('v'));
        let closed = render(&mut ui);
        assert_eq!(closed, hidden);
    }

    #[test]
    fn tabs_pins_moves_and_completion_share_the_store() {
        let mut conn = Connection::open_in_memory().unwrap();
        db::migrate(&mut conn).unwrap();
        let list = db::create_list(&mut conn, "Work").unwrap();
        let task = db::create_task(&mut conn, None, "Focus", Some("Description"), None).unwrap();
        let mut ui = Ui::default();
        ui.refresh(&conn).unwrap();
        assert_eq!(ui.view, View::Lists);
        press(&mut ui, &mut conn, KeyCode::Char('p'));
        ui.form.as_mut().unwrap().paste("-Ada");
        press(&mut ui, &mut conn, KeyCode::Enter);
        ui.form.as_mut().unwrap().paste("Pikachu");
        press(&mut ui, &mut conn, KeyCode::Tab);
        assert_eq!(
            ui.view,
            View::Lists,
            "can't switch tabs while entering text"
        );
        press(&mut ui, &mut conn, KeyCode::Enter);
        db::change_task(
            &mut conn,
            &task.uid,
            Change::Pin {
                agent: "Bo",
                avatar: Some("Robot"),
            },
        )
        .unwrap();
        press(&mut ui, &mut conn, KeyCode::BackTab);
        assert_eq!(ui.view, View::Pinned);
        assert_eq!(
            ui.rows
                .iter()
                .map(|r| r.pin.as_ref().unwrap().agent.as_str())
                .collect::<Vec<_>>(),
            ["Bo", "me-Ada"]
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut ui)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        for text in [
            "FOCUS BOARD",
            "AGENT CONSOLE",
            "02 PINS",
            "02 IDENTITIES",
            "⟦ Pikachu ⟧",
            "⟦ Robot ⟧",
            "PINNED  /  Inbox",
        ] {
            assert!(rendered.contains(text), "missing {text}");
        }
        press(&mut ui, &mut conn, KeyCode::Char('m'));
        press(&mut ui, &mut conn, KeyCode::Right);
        press(&mut ui, &mut conn, KeyCode::Enter);
        assert_eq!(db::show(&conn, &task.uid).unwrap().list_uid, list.uid);
        press(&mut ui, &mut conn, KeyCode::Char('n'));
        press(&mut ui, &mut conn, KeyCode::BackTab);
        assert_eq!(ui.view, View::Today);
        assert_eq!(ui.row().unwrap().task.uid, task.uid);
        press(&mut ui, &mut conn, KeyCode::Char(' '));
        assert!(ui.form.is_none());
        assert!(ui.rows.is_empty());
        let done = db::show(&conn, &task.uid).unwrap();
        assert!(done.completed);
        assert_eq!(done.completion_note, None);
        assert_eq!(done.description.as_deref(), Some("Description"));
        ui.view = View::Lists;
        ui.lists_state.select(Some(1));
        ui.refresh(&conn).unwrap();
        press(&mut ui, &mut conn, KeyCode::Char('o'));
        ui.form.as_mut().unwrap().paste("Finished");
        press(&mut ui, &mut conn, KeyCode::Enter);
        assert_eq!(
            db::show(&conn, &task.uid)
                .unwrap()
                .completion_note
                .as_deref(),
            Some("Finished")
        );
        press(&mut ui, &mut conn, KeyCode::Char(' '));
        assert!(!db::show(&conn, &task.uid).unwrap().completed);
        press(&mut ui, &mut conn, KeyCode::Char(' '));
        let completed_again = db::show(&conn, &task.uid).unwrap();
        assert!(completed_again.completed);
        assert_eq!(completed_again.completion_note.as_deref(), Some("Finished"));
    }
}
