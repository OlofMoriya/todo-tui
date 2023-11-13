use std::{
    cmp::min,
    error::Error,
    io::{self, Stdout},
    time::Duration,
};

use chrono::{Local, Days, NaiveDate};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use database::{add_list, add_todo, delete_list, delete_todo, fetch_lists, toggle_todo_completion, update_todo};
use model::{Todo, TodoList};
use ratatui::{
    prelude::{Alignment, Constraint, CrosstermBackend, Direction, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph},
    Terminal,
};

use crate::database::{fetch_incomplete_todos, fetch_todos};

mod database;
mod model;

use clap::Parser;

#[derive(Debug, Copy, Clone)]
enum InputField {
    Title,
    Description,
    DueDate,
}

enum AppState {
    List(Option<usize>),
    Create(Option<InputField>, Option<usize>),
    CreateList(Option<InputField>),
}

struct State {
    pub list_title: String,
    pub todo_description: String,
    pub todo_title: String,
    pub todo_due_date: Option<NaiveDate>,
    pub state: AppState,
    pub input: String,
    pub lists_list_state: ListState,
    pub todo_list_state: ListState,
    pub selecting_list: bool,
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Return all incomplete todos
    #[arg(short, long)]
    date: Option<NaiveDate>,

    /// Only return amount of incomplete todos
    #[clap(short, long)]
    count: bool,
}
fn main() -> Result<(), Box<dyn Error>> {

    let args: Args = Args::parse(); 
    let date = args.date;
    let count = args.count;
    if date.is_some() || count {
        let todos = fetch_incomplete_todos(date.unwrap_or(Local::now().naive_local().date()));
        match todos {
            Ok(todos) =>
                match count {
                    true => println!("{}", todos.len()),
                    false => { 
                        todos.iter().for_each(|t| println!("{}\t{}\t{:?}", t.id.unwrap_or(0), t.due_date.expect("Has to have a date to be fetched"), t.title,));
                    }
                },
            Err(e) => println!("Err: {:?}", e)
        };
        return Ok(()); 
    } 

    let state = State {
        state: AppState::List(None),
        list_title: "".to_string(),
        input: "".to_string(),
        todo_title: "".to_string(),
        todo_description: "".to_string(),
        todo_due_date: None,
        lists_list_state: ListState::default(),
        todo_list_state: ListState::default(),
        selecting_list: true,
    };
    let mut terminal = setup_terminal()?;
    run(&mut terminal, state)?;
    restore_terminal(&mut terminal)?;
    Ok(())
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>, Box<dyn Error>> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<(), Box<dyn Error>> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen,)?;
    Ok(terminal.show_cursor()?)
}

fn get_todos(list_id: usize) -> Vec<Todo> {
    let todos = fetch_todos(list_id);
    return match todos {
        Ok(mut todos) => {
            todos.sort_by_key(|t| t.due_date);
            todos.sort_by_key(|t| !t.due_date.is_some());
            todos.sort_by_key(|t| t.completed);
            return todos;
        },
        Err(_) => vec![],
    };
}

fn get_lists() -> Vec<TodoList> {
    let lists = fetch_lists();
    return match lists {
        Ok(it) => it,
        Err(_) => return vec![],
    };
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    mut state: State,
) -> Result<(), Box<dyn Error>> {
    let mut lists = get_lists();
    let mut todos = vec![];

    Ok(loop {
        match state.state {
            AppState::List(detail) => {
                lists = get_lists();
                todos = match state.lists_list_state.selected() {
                    Some(list_index) => get_todos(lists[list_index].id.expect("Id exists")),
                    None => vec![],
                };
                match detail {
                    Some(v) => draw_lists_with_details(terminal, &lists, &todos, &mut state, v),
                    None => draw_lists(terminal, &lists, &todos, &mut state),
                }
            }
            AppState::Create(field, _) => draw_create_todo(terminal, &state, field),

            AppState::CreateList(field) => draw_create_list(terminal, &state, field),
        };

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                match state.state {
                    AppState::List(detail) => match key.code {
                        KeyCode::Char('q') => {
                            break;
                        }
                        KeyCode::Char('v') => {
                            match detail {
                                Some(_) => state.state = AppState::List(None),
                                None => {
                                    match state.todo_list_state.selected() {
                                        Some(index) => {state.state = AppState::List(Some(index))}
                                        None => ()
                                    }
                                }
                            }
                        }
                        KeyCode::Char('E') => {
                            if state.lists_list_state.selected().is_some() {
                                if let Some(edit_todo_index) = state.todo_list_state.selected() {
                                    let todo = &todos[edit_todo_index];
                                    state.todo_description = todo.description.clone().unwrap_or("".to_string());
                                    state.input = todo.title.clone();
                                    state.todo_title = todo.title.clone();
                                    state.todo_due_date = todo.due_date.clone();
                                    state.state = AppState::Create(Some(InputField::Title), Some(edit_todo_index));
                                }
                            }
                        }
                        KeyCode::Char('N') => {
                            if state.lists_list_state.selected().is_some() {
                                state.state = AppState::Create(Some(InputField::Title), None)
                            }
                        }
                        KeyCode::Char('L') => {
                            state.state = AppState::CreateList(Some(InputField::Title))
                        }
                        KeyCode::Char('D') => match state.selecting_list {
                            true => match state.lists_list_state.selected() {
                                Some(list_index) => {
                                    delete_list(
                                        lists[list_index]
                                            .id
                                            .expect("Should get an id from the database create")
                                            .clone(),
                                    )
                                    .ok();
                                    state.lists_list_state.select(None);
                                    state.todo_list_state.select(None);
                                }
                                None => {}
                            },
                            false => match state.todo_list_state.selected() {
                                Some(todo_index) => {
                                    delete_todo(
                                        todos[todo_index]
                                            .id
                                            .expect("Should get an id from the database create"),
                                    )
                                    .ok();
                                }
                                None => {}
                            },
                        },
                        KeyCode::Char('j') => match state.selecting_list {
                            true => {
                                lists_move_down(&mut state, &lists);
                            }
                            false => {
                                todos_move_down(&mut state, &todos);
                            }
                        },
                        KeyCode::Char('k') => match state.selecting_list {
                            true => {
                                lists_move_up(&mut state);
                            }
                            false => {
                                todos_move_up(&mut state);
                            }
                        },
                        KeyCode::Char('h') => match state.selecting_list {
                            true => {}
                            false => {
                                state.selecting_list = true;
                                state.state = AppState::List(None);
                                state.todo_list_state.select(None);
                            }
                        },
                        KeyCode::Char('l') => match state.selecting_list {
                            true => {
                                state.selecting_list = false;
                                todos = match state.lists_list_state.selected() {
                                    Some(index) => get_todos(lists[index].id.expect("Id exists")),
                                    None => vec![],
                                };
                                if todos.len() > 0 {
                                    state.todo_list_state.select(Some(0));
                                }
                            }
                            false => {
                                toggle_todo(&mut state, &todos);
                            }
                        },
                        KeyCode::Char(' ') => match state.selecting_list {
                            true => {}
                            false => {
                                toggle_todo(&mut state, &todos);
                            }
                        },
                        _ => {}
                    },
                    AppState::Create(field, edit_todo_index) => match field {
                        Some(f) => match key.code {
                            KeyCode::Char(c) => {
                                state.input = format!("{}{}", state.input, c);
                            }
                            KeyCode::Backspace => {
                                state.input.pop();
                            }
                            KeyCode::Esc => {
                                state.input = "".to_string();
                                state.state = AppState::Create(None, edit_todo_index)
                            }
                            KeyCode::Enter => match f {
                                InputField::Title => {
                                    state.todo_title = state.input.clone();
                                    state.input = "".to_string();
                                    state.state = AppState::Create(Some(InputField::Description), edit_todo_index);
                                }
                                InputField::Description => {
                                    state.todo_description = state.input.clone();
                                    state.input = "".to_string();
                                    state.state = AppState::Create(Some(InputField::DueDate), edit_todo_index);
                                }
                                InputField::DueDate => {
                                    let duedatestring = state.input.clone();
                                    state.todo_due_date = match duedatestring.parse::<u64>() {
                                        Ok(v) => Some(Local::now().checked_add_days(Days::new(v)).expect("in range").naive_local().date()),
                                        Err(_) => None
                                    };
                                    state.input = "".to_string();
                                    state.state = AppState::Create(None, edit_todo_index);
                                }
                            },
                            _ => {}
                        },
                        None => match key.code {
                            KeyCode::Esc => {
                                state.state = AppState::List(None);
                            }
                            KeyCode::Char('q') => {
                                state.state = AppState::List(None);
                            }
                            KeyCode::Char('D') => {
                                state.state = AppState::Create(Some(InputField::DueDate), edit_todo_index);
                            }
                            KeyCode::Char('d') => {
                                state.state = AppState::Create(Some(InputField::Description), edit_todo_index);
                                state.input = state.todo_description.clone();
                            }
                            KeyCode::Char('t') => {
                                state.state = AppState::Create(Some(InputField::Title), edit_todo_index);
                                state.input = state.todo_title.clone();
                            }
                            KeyCode::Char('s') => {
                                match edit_todo_index {
                                    Some(index) => {
                                        let mut updated_todo = todos[index].clone();
                                        updated_todo.due_date = state.todo_due_date;
                                        updated_todo.title = state.todo_title;
                                        updated_todo.description = Some(state.todo_description);
                                        // Should handle error
                                        _ = update_todo(&updated_todo);
                                    }
                                    None => {
                                        save_todo(
                                            &state,
                                            lists[state
                                                .lists_list_state
                                                .selected()
                                                .expect("Need list id to create todo")]
                                            .id
                                            .expect("Id exists"),
                                        );
                                    }
                                }
                                state.todo_title = "".to_string();
                                state.todo_description = "".to_string();
                                state.todo_due_date = None;
                                state.state = AppState::List(None);
                            }
                            _ => {}
                        },
                    },
                    AppState::CreateList(field) => match field {
                        Some(f) => match key.code {
                            KeyCode::Char(c) => {
                                state.input = format!("{}{}", state.input, c);
                            }
                            KeyCode::Backspace => {
                                state.input.pop();
                            }
                            KeyCode::Esc => {
                                state.input = "".to_string();
                                state.state = AppState::CreateList(None)
                            }
                            KeyCode::Enter => match f {
                                InputField::Title => {
                                    state.list_title = state.input.clone();
                                    state.input = "".to_string();
                                    state.state = AppState::CreateList(None);
                                }
                                _ => {}
                            },
                            _ => {}
                        },
                        None => match key.code {
                            KeyCode::Esc => {
                                state.state = AppState::List(None);
                            }
                            KeyCode::Char('q') => {
                                state.state = AppState::List(None);
                            }
                            KeyCode::Char('t') => {
                                state.state = AppState::CreateList(Some(InputField::Title));
                            }
                            KeyCode::Char('s') => {
                                save_todo_list(state.list_title.clone());
                                state.input = "".to_string();
                                state.state = AppState::List(None);
                            }
                            _ => {}
                        },
                    },
                }
            }
        }
    })
}

fn save_todo_list(title: String) {
    let list = TodoList { title, id: None };
    add_list(&list).ok();
}

fn save_todo(state: &State, list_id: usize) {
    let todo = Todo {
        id: None,
        list_id,
        title: state.todo_title.clone(),
        description: Some(state.todo_description.clone()),
        due_date: state.todo_due_date.clone(),
        completed: false,
        completed_date: None,
        dependencies: vec![],
    };
    add_todo(&todo).ok();
}

fn toggle_todo(state: &mut State, todos: &[Todo]) {
    match state.todo_list_state.selected() {
        Some(todo_index) => {
            toggle_todo_completion(
                todos[todo_index]
                    .id
                    .expect("Should have an id from the database creation"),
                !todos[todo_index].completed,
            )
            .ok();
        }
        None => {}
    }
}

fn todos_move_up(state: &mut State) {
    match state.todo_list_state.selected() {
        Some(v) => {
            let max = match v {
                0 => None,
                v => Some(v - 1),
            };
            state.todo_list_state.select(max);
        }
        None => {
            state.todo_list_state.select(Some(0));
        }
    }
}

fn lists_move_up(state: &mut State) {
    match state.lists_list_state.selected() {
        Some(v) => {
            let max = match v {
                0 => None,
                v => Some(v - 1),
            };
            state.lists_list_state.select(max);
        }
        None => {
            state.lists_list_state.select(Some(0));
        }
    }
}

fn todos_move_down(state: &mut State, todos: &[Todo]) {
    match state.todo_list_state.selected() {
        Some(v) => {
            state
                .todo_list_state
                .select(Some(min(v + 1, todos.len() - 1)));
        }
        None => {
            state.todo_list_state.select(Some(0));
        }
    }
}

fn lists_move_down(state: &mut State, lists: &Vec<TodoList>) {
    match state.lists_list_state.selected() {
        Some(v) => {
            state
                .lists_list_state
                .select(Some(min(v + 1, lists.len() - 1)));
        }
        None => {
            state.lists_list_state.select(Some(0));
        }
    }
}

fn draw_create_list(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    state: &State,
    input_field: Option<InputField>,
) {
    terminal
        .draw(|frame| {
            let size = frame.size();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(2)
                .constraints(
                    [
                        Constraint::Length(2),
                        Constraint::Min(5),
                        Constraint::Length(4),
                    ]
                    .as_ref(),
                )
                .split(size);

            frame.render_widget(
                Paragraph::new("New list")
                    .style(Style::default())
                    .alignment(Alignment::Center),
                chunks[0],
            );

            let text = vec![
                Line::from("(t) Input title"),
                Line::from("(s) Save list".green().italic()),
                Line::from("(esc) Cancel".red()),
            ];

            frame.render_widget(
                Paragraph::new(text.clone())
                    .style(Style::default())
                    .alignment(Alignment::Center),
                chunks[1],
            );

            frame.render_widget(
                Paragraph::new(match input_field {
                    Some(InputField::Title) => state.input.clone(),
                    _ => state.list_title.clone(),
                })
                .block(
                    Block::default()
                        .title("Title")
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded),
                )
                .style(Style::default().fg(match input_field {
                    Some(InputField::Title) => Color::Yellow,
                    _ => Color::White,
                }))
                .alignment(Alignment::Center),
                chunks[2],
            );
        })
        .ok();
}

fn draw_lists(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    lists: &Vec<TodoList>,
    todos: &Vec<Todo>,
    state: &mut State,
) {
    let lists_items: Vec<_> = lists
        .iter()
        .map(|list| {
            ListItem::new(Line::from(vec![Span::styled(
                list.title.clone(),
                Style::default(),
            )]))
        })
        .collect();

    let lists_ui = List::new(lists_items)
        .block(Block::default().title("List").borders(Borders::ALL))
        .style(Style::default().fg(Color::White))
        .highlight_style(Style::default().add_modifier(Modifier::ITALIC))
        .highlight_symbol(">>");

    let todo_items: Vec<_> = todos
        .iter()
        .map(|todo: &Todo| {

            let overdue = !todo.completed && todo.due_date.is_some() && todo.due_date.unwrap() <= Local::now().date_naive();

            ListItem::new(Line::from(vec![Span::styled(
                format!(
                    "{} {} {}",
                    todo.id.or(Some(9)).expect("or is being used"),
                    match todo.completed {
                        true => "[x]",
                        false => "[ ]",
                    },
                    todo.title.clone()
                ),
                Style::default().fg(match overdue { true => Color::Red, false => Color::White}),
            )]))
        })
        .collect();

    let todo_ui = List::new(todo_items)
        .block(Block::default().title("Todos").borders(Borders::ALL))
        .style(Style::default().fg(Color::White))
        .highlight_style(Style::default().add_modifier(Modifier::ITALIC))
        .highlight_symbol(">>");

    terminal
        .draw(|frame| {
            let size = frame.size();
            let vert_chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(2)
                .constraints(
                    [
                        Constraint::Length(2),
                        Constraint::Min(20),
                    ]
                    .as_ref(),
                )
                .split(size);

            let list_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .margin(2)
                .constraints(
                    [
                        Constraint::Percentage(30),
                        Constraint::Min(20),
                    ]
                    .as_ref(),
                )
                .split(vert_chunks[1]);

            frame.render_widget(
                Paragraph::new("(N) new task, (L) new list, (h,j,k,l) move, (D) delete, (esc, q) exit")
                    .style(Style::default())
                    .alignment(Alignment::Center),
                vert_chunks[0],
            );
            frame.render_stateful_widget(lists_ui, list_chunks[0], &mut state.lists_list_state);
            frame.render_stateful_widget(todo_ui, list_chunks[1], &mut state.todo_list_state);
        })
        .ok();
}


fn draw_lists_with_details(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    lists: &Vec<TodoList>,
    todos: &Vec<Todo>,
    state: &mut State,
    details_index: usize
) {
    let lists_items: Vec<_> = lists
        .iter()
        .map(|list| {
            ListItem::new(Line::from(vec![Span::styled(
                list.title.clone(),
                Style::default(),
            )]))
        })
        .collect();

    let lists_ui = List::new(lists_items)
        .block(Block::default().title("List").borders(Borders::ALL))
        .style(Style::default().fg(Color::White))
        .highlight_style(Style::default().add_modifier(Modifier::ITALIC))
        .highlight_symbol(">>");

    let todo_items: Vec<_> = todos
        .iter()
        .map(|todo| {
            ListItem::new(Line::from(vec![Span::styled(
                format!(
                    "{} {} {}",
                    todo.id.or(Some(9)).expect("or is being used"),
                    match todo.completed {
                        true => "[x]",
                        false => "[ ]",
                    },
                    todo.title.clone()
                ),
                Style::default(),
            )]))
        })
        .collect();

    let todo_ui = List::new(todo_items)
        .block(Block::default().title("Todos").borders(Borders::ALL))
        .style(Style::default().fg(Color::White))
        .highlight_style(Style::default().add_modifier(Modifier::ITALIC))
        .highlight_symbol(">>");

    terminal
        .draw(|frame| {
            let size = frame.size();
            let vert_chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(2)
                .constraints(
                    [
                        Constraint::Length(2),
                        Constraint::Min(20),
                        Constraint::Length(2),
                        Constraint::Length(4),
                    ]
                    .as_ref(),
                )
                .split(size);

            let list_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .margin(2)
                .constraints(
                    [
                        Constraint::Percentage(30),
                        Constraint::Min(20),
                    ]
                    .as_ref(),
                )
                .split(vert_chunks[1]);

            frame.render_widget(
                Paragraph::new("(N) new task, (L) new list, (h,j,k,l) move, (D) delete, (esc, q) exit")
                    .style(Style::default())
                    .alignment(Alignment::Center),
                vert_chunks[0],
            );
            frame.render_stateful_widget(lists_ui, list_chunks[0], &mut state.lists_list_state);
            frame.render_stateful_widget(todo_ui, list_chunks[1], &mut state.todo_list_state);

            let selected_todo = todos.get(details_index);
            match selected_todo {
                Some(v) => {

            frame.render_widget(
                Paragraph::new(v.title.clone())
                    .style(Style::default())
                    .alignment(Alignment::Center),
                vert_chunks[2],
            );
            frame.render_widget(
                Paragraph::new(v.description.clone().or(Some("".to_string())).expect("or"))
                    .style(Style::default())
                    .alignment(Alignment::Center),
                vert_chunks[3],
            );
                },
                None =>{}
            };
        })
        .ok();
}

fn draw_create_todo(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    state: &State,
    input_field: Option<InputField>,
) {
    terminal
        .draw(|frame| {
            let size = frame.size();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(2)
                .constraints(
                    [
                        Constraint::Min(2),
                        Constraint::Min(5),
                        Constraint::Length(4),
                        Constraint::Length(4),
                        Constraint::Length(4),
                    ]
                    .as_ref(),
                )
                .split(size);

            frame.render_widget(
                Paragraph::new("New todo")
                    .style(Style::default())
                    .alignment(Alignment::Center),
                chunks[0],
            );

            let text = vec![
                Line::from("Create a todo"),
                Line::from("(t) Input title"),
                Line::from("(d) Input description"),
                Line::from("(D) Input due date"),
                Line::from("(s) Save todo".green().italic()),
                Line::from("(esc) Cancel".red()),
            ];

            frame.render_widget(
                Paragraph::new(text.clone())
                    .style(Style::default())
                    .alignment(Alignment::Center),
                chunks[1],
            );

            frame.render_widget(
                Paragraph::new(match input_field {
                    Some(InputField::Title) => state.input.clone(),
                    _ => state.todo_title.clone(),
                })
                .block(
                    Block::default()
                        .title("Title")
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded),
                )
                .style(Style::default().fg(match input_field {
                    Some(InputField::Title) => Color::Yellow,
                    _ => Color::White,
                }))
                .alignment(Alignment::Center),
                chunks[2],
            );

            frame.render_widget(
                Paragraph::new(match input_field {
                    Some(InputField::Description) => state.input.clone(),
                    _ => state.todo_description.clone(),
                })
                .block(
                    Block::default()
                        .title("Description")
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded),
                )
                .style(Style::default().fg(match input_field {
                    Some(InputField::Description) => Color::Yellow,
                    _ => Color::White,
                }))
                .alignment(Alignment::Center),
                chunks[3],
            );
            
            frame.render_widget(
                Paragraph::new(match input_field {
                    Some(InputField::DueDate) => state.input.clone(),
                    _ => match state.todo_due_date.clone(){ None => "".to_string(), Some(v) => v.to_string()},
                })
                .block(
                    Block::default()
                        .title("Due date +days from now")
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded),
                )
                .style(Style::default().fg(match input_field {
                    Some(InputField::DueDate) => Color::Yellow,
                    _ => Color::White,
                }))
                .alignment(Alignment::Center),
                chunks[4],
            );
        })
        .ok();
}
