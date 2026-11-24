use std::{io, time::Duration};

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
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
    Frame, Terminal,
};
use rusqlite::Connection;

use crate::{
    database::{self as db, Change, Query, Result},
    model::{Pin, Todo, TodoList},
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum View {
    Pinned,
    #[default]
    Lists,
    Today,
}

impl View {
    const ALL: [Self; 3] = [Self::Pinned, Self::Lists, Self::Today];
    const TITLES: [&'static str; 3] = ["Pinned", "Lists", "Today"];
    fn index(self) -> usize {
        Self::ALL.iter().position(|v| *v == self).unwrap()
    }
    fn cycle(self, reverse: bool) -> Self {
        Self::ALL[(self.index() + if reverse { 2 } else { 1 }) % 3]
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
    on_lists: bool,
    lists: Vec<TodoList>,
    rows: Vec<Row>,
    lists_state: ListState,
    tasks_state: ListState,
    form: Option<Form>,
    status: String,
}

impl Ui {
    fn list(&self) -> Option<&TodoList> {
        self.lists.get(self.lists_state.selected()?)
    }
    fn row(&self) -> Option<&Row> {
        self.rows.get(self.tasks_state.selected()?)
    }
    fn refresh(&mut self, conn: &Connection) -> Result<()> {
        let lists = db::lists(conn)?;
        let list_index = keep(
            self.list().map(|l| l.uid.as_str()),
            lists.iter().map(|l| l.uid.as_str()),
            self.lists_state.selected(),
        );
        let tasks = match self.view {
            View::Lists => match list_index {
                Some(i) => db::tasks(
                    conn,
                    Query {
                        list: Some(&lists[i].uid),
                        ..Query::default()
                    },
                )?,
                None => vec![],
            },
            View::Pinned => db::tasks(
                conn,
                Query {
                    pinned: true,
                    ..Query::default()
                },
            )?,
            View::Today => db::tasks(
                conn,
                Query {
                    today: true,
                    ..Query::default()
                },
            )?,
        };
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
        let lists = self.view == View::Lists && self.on_lists;
        match key.code {
            KeyCode::Char('q') => return Ok(false),
            KeyCode::Tab | KeyCode::BackTab => {
                self.view = self.view.cycle(
                    key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT),
                );
                self.tasks_state.select(None);
                self.status.clear();
            }
            KeyCode::Left | KeyCode::Char('h') if self.view == View::Lists => self.on_lists = true,
            KeyCode::Right | KeyCode::Char('l') if self.view == View::Lists => {
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
                    self.form = Some(if key.code == KeyCode::Char('R') {
                        Form::new(
                            Action::RenameList(list.uid.clone()),
                            vec![Field::text("List name", &list.title)],
                        )
                    } else {
                        Form::confirm(Action::DeleteList(list.uid.clone()), &list.title)
                    });
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
                    ' ' if !task.completed => Some(Form::new(
                        Action::Complete(id),
                        vec![Field::text(
                            "Completion note (optional)",
                            task.completion_note.as_deref().unwrap_or_default(),
                        )],
                    )),
                    ' ' => {
                        db::change_task(conn, &id, Change::Reopen)?;
                        self.status = "Reopened".into();
                        None
                    }
                    'n' => {
                        db::change_task(conn, &id, Change::Next(!task.next))?;
                        self.status = "Next tag updated".into();
                        None
                    }
                    'D' => Some(Form::confirm(Action::Delete(id), &task.title)),
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
    DeleteList(String),
    Edit(String),
    Move(String),
    Pin(String),
    Unpin(String),
    Note(String),
    Complete(String),
    Delete(String),
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
    fn confirm(action: Action, title: &str) -> Self {
        Self::new(
            action,
            vec![Field::text(
                &format!("Delete ‘{}’? Type yes", safe(title)),
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
            Action::DeleteList(id) => {
                confirmed()?;
                db::remove_list(conn, id)?;
                return Ok("List deleted");
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
            Action::Complete(id) => (id, Change::Complete(optional(0)?), "Completed"),
            Action::Delete(id) => {
                confirmed()?;
                (id, Change::Delete, "Task deleted")
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
    frame.render_widget(
        Tabs::new(View::TITLES.map(Line::from).to_vec())
            .block(Block::default().title("todo-tui").borders(Borders::ALL))
            .select(ui.view.index())
            .highlight_style(accent),
        outer[0],
    );
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
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if ui.view == View::Lists {
            [
                Constraint::Percentage(22),
                Constraint::Percentage(34),
                Constraint::Percentage(44),
            ]
        } else {
            [
                Constraint::Length(0),
                Constraint::Percentage(52),
                Constraint::Percentage(48),
            ]
        })
        .split(outer[1]);
    let on_lists = ui.view == View::Lists && ui.on_lists;
    if ui.view == View::Lists {
        let lists = ui
            .lists
            .iter()
            .map(|l| ListItem::new(safe(&l.title)))
            .collect::<Vec<_>>();
        frame.render_stateful_widget(
            List::new(lists)
                .highlight_symbol("› ")
                .highlight_style(if on_lists { accent } else { Style::default() })
                .block(Block::default().title("Lists").borders(Borders::ALL)),
            panes[0],
            &mut ui.lists_state,
        );
    }
    let mut group = None;
    let items = ui
        .rows
        .iter()
        .map(|row| {
            let mut lines = vec![];
            if let Some(pin) = &row.pin {
                if group != Some(pin.agent.as_str()) {
                    lines.push(Line::styled(safe(&pin.agent), accent));
                    group = Some(&pin.agent);
                }
                lines.push(Line::from(format!(
                    "{} {} · {}",
                    safe(&pin.avatar),
                    task_label(&row.task),
                    safe(&row.task.list_title)
                )));
            } else {
                lines.push(Line::from(task_label(&row.task)));
            }
            ListItem::new(lines)
        })
        .collect::<Vec<_>>();
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("› ")
            .highlight_style(if on_lists { Style::default() } else { accent })
            .block(
                Block::default()
                    .title(match ui.view {
                        View::Lists => "Tasks",
                        View::Pinned => "By human or agent",
                        View::Today => "Priorities",
                    })
                    .borders(Borders::ALL),
            ),
        panes[1],
        &mut ui.tasks_state,
    );
    frame.render_widget(
        Paragraph::new(
            ui.row()
                .map(|r| detail(&r.task))
                .unwrap_or_else(|| "No tasks".into()),
        )
        .block(Block::default().title("Details").borders(Borders::ALL))
        .wrap(Wrap { trim: false }),
        panes[2],
    );
    let actions = if on_lists {
        "L new list • R rename • D delete • N new task"
    } else {
        "N add • E edit • p pin • u unpin • n Next • m move • Space done/reopen • o note • D delete"
    };
    frame.render_widget(
        Paragraph::new(format!(
            "Tab/Shift-Tab views • hjkl/↑↓ navigate • q quit\n{actions}\n{}",
            safe(&ui.status)
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

pub fn run() -> Result<()> {
    let mut conn = db::open_db()?;
    let mut ui = Ui {
        on_lists: true,
        ..Ui::default()
    };
    ui.refresh(&conn)?;
    enable_raw_mode()?;
    let _tty = Tty;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    loop {
        terminal.draw(|frame| draw(frame, &mut ui))?;
        if event::poll(Duration::from_millis(500))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
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
        if let Err(e) = ui.refresh(&conn) {
            ui.status = e.to_string();
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
        for text in ["By human or agent", "Pikachu", "Robot", "Description"] {
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
        ui.form.as_mut().unwrap().paste("Finished");
        press(&mut ui, &mut conn, KeyCode::Enter);
        assert!(ui.rows.is_empty());
        let done = db::show(&conn, &task.uid).unwrap();
        assert_eq!(done.completion_note.as_deref(), Some("Finished"));
        assert_eq!(done.description.as_deref(), Some("Description"));
    }
}
