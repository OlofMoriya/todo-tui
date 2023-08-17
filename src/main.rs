use std::{
    cmp::min,
    error::Error,
    io::{self, Stdout},
    time::Duration,
};

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use database::{fetch_lists, add_todo};
use model::{Todo, TodoList};
use ratatui::{
    prelude::{Alignment, Constraint, CrosstermBackend, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Spans},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph},
    Terminal,
};

use crate::database::fetch_todos;

mod database;
mod model;

#[derive(Debug, Copy, Clone)]
enum InputField {
    ListTitle,
    TaskTitle,
    TaskDescription,
}

enum AppState {
    List,
    Create,
    CreateList,
    Input(InputField),
}

struct State {
    pub todo_description: String,
    pub todo_title: String,
    pub state: AppState,
    pub input: String,
    pub lists_list_state: ListState,
    pub todo_list_state: ListState,
    pub selecting_list: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let state = State {
        state: AppState::List,
        input: "".to_string(),
        todo_title: "".to_string(),
        todo_description: "".to_string(),
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

fn get_todos(list_id: i32) -> Vec<Todo> {
    let todos = fetch_todos(list_id);
    return match todos {
        Ok(todos) => todos,
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
    let mut todos;
    Ok(loop {
        match state.state {
            AppState::List => {
                lists = get_lists();
                todos = match state.lists_list_state.selected() {
                    Some(id) => get_todos(id as i32),
                    None => vec![],
                };
                let lists_items: Vec<_> = lists
                    .iter()
                    .map(|list| {
                        ListItem::new(Spans::from(vec![Span::styled(
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
                    .map(|list| {
                        ListItem::new(Spans::from(vec![Span::styled(
                            list.title.clone(),
                            Style::default(),
                        )]))
                    })
                    .collect();

                let todo_ui = List::new(todo_items)
                    .block(Block::default().title("Todos").borders(Borders::ALL))
                    .style(Style::default().fg(Color::White))
                    .highlight_style(Style::default().add_modifier(Modifier::ITALIC))
                    .highlight_symbol(">>");

                terminal.draw(|frame| {
                    let size = frame.size();
                    let chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .margin(2)
                        .constraints([Constraint::Percentage(30), Constraint::Min(2)].as_ref())
                        .split(size);

                    frame.render_stateful_widget(lists_ui, chunks[0], &mut state.lists_list_state);
                    frame.render_stateful_widget(todo_ui, chunks[1], &mut state.todo_list_state);
                })?;
            }
            AppState::Create => {
                terminal.draw(|frame| {
                    frame.render_widget(
                        Paragraph::new("Create todo")
                            .style(Style::default())
                            .alignment(Alignment::Center),
                        frame.size(),
                    )
                })?;
            }
            AppState::CreateList => {
                terminal.draw(|frame| {
                    frame.render_widget(
                        Paragraph::new("Create list")
                            .style(Style::default())
                            .alignment(Alignment::Center),
                        frame.size(),
                    )
                })?;
            }
            AppState::Input(field) => {
                terminal.draw(|frame| {
                    let size = frame.size();
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .margin(2)
                        .constraints(
                            [
                                Constraint::Length(3),
                                Constraint::Min(2),
                                Constraint::Length(3),
                            ]
                            .as_ref(),
                        )
                        .split(size);

                    frame.render_widget(
                        Paragraph::new(match field {
                            InputField::ListTitle => "List title",
                            InputField::TaskTitle => "Task title",
                            InputField::TaskDescription => "Task description",
                        })
                        .style(Style::default())
                        .alignment(Alignment::Center),
                        chunks[1],
                    );
                    frame.render_widget(
                        Paragraph::new(state.input.clone())
                            .block(
                                Block::default()
                                    .title(match field {
                                        InputField::ListTitle => "List title",
                                        InputField::TaskTitle => "Task title",
                                        InputField::TaskDescription => "Task description",
                                    })
                                    .borders(Borders::ALL)
                                    .border_type(BorderType::Rounded),
                            )
                            .style(Style::default())
                            .alignment(Alignment::Center),
                        chunks[2],
                    )
                })?;
            }
        };

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                match state.state {
                    AppState::List => match key.code {
                        KeyCode::Char('q') => {
                            break;
                        }
                        KeyCode::Char('N') => {
                            if state.lists_list_state.selected().is_some() {
                                state.state = AppState::Create
                            }
                        }
                        KeyCode::Char('L') => state.state = AppState::CreateList,
                        KeyCode::Char('j') => {
                            match state.selecting_list {
                                true => match state.lists_list_state.selected() {
                                    Some(v) => {
                                        state
                                            .lists_list_state
                                            .select(Some(min(v + 1, lists.len())));
                                    }
                                    None => {
                                        state.lists_list_state.select(Some(0));
                                    }
                                },
                                false => {
                                    match state.todo_list_state.selected() {
                                        //TODO: not lists len... should be todos len
                                        Some(v) => {
                                            state
                                                .todo_list_state
                                                .select(Some(min(v + 1, lists.len())));
                                        }
                                        None => {
                                            state.todo_list_state.select(Some(0));
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char('k') => match state.selecting_list {
                            true => match state.lists_list_state.selected() {
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
                            },
                            false => match state.todo_list_state.selected() {
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
                            },
                        },
                        KeyCode::Char('h') => match state.selecting_list {
                            true => {}
                            false => {
                                state.selecting_list = true;
                            }
                        },
                        KeyCode::Char('l') => match state.selecting_list {
                            true => {
                                state.selecting_list = false;
                            }
                            false => {}
                        },
                        _ => {}
                    },

                    AppState::Create => match key.code {
                        KeyCode::Char('q') => {
                            state.state = AppState::List;
                        }
                        KeyCode::Char('d') => {
                            state.state = AppState::Input(InputField::TaskDescription);
                        }
                        KeyCode::Char('t') => {
                            state.state = AppState::Input(InputField::TaskTitle);
                        }
                        KeyCode::Char('s') => {
                            let todo = Todo {
                                id: None,
                                list_id: state
                                    .lists_list_state
                                    .selected()
                                    .expect("Need list id to create todo"),
                                title: state.todo_title.clone(),
                                description: Some(state.todo_description.clone()),
                                due_date: None,
                                completed: false,
                                completed_date: None,
                                dependencies: vec![],
                            };
                            add_todo(&todo).ok();
                            state.todo_title = "".to_string();
                            state.todo_description = "".to_string();
                            state.state = AppState::List;
                        }
                        _ => {}
                    },
                    AppState::CreateList => match key.code {
                        KeyCode::Char('q') => {
                            state.state = AppState::List;
                        }
                        KeyCode::Char('t') => {
                            state.state = AppState::Input(InputField::ListTitle);
                        }
                        _ => {}
                    },
                    AppState::Input(field) => match key.code {
                        KeyCode::Char(c) => {
                            state.input = format!("{}{}", state.input, c);
                        }
                        KeyCode::Backspace => {
                            state.input.pop();
                        }
                        KeyCode::Esc => {
                            state.input = "".to_string();
                            state.state = AppState::List
                        }
                        KeyCode::Enter => match field {
                            InputField::ListTitle => {
                                let list = TodoList {
                                    title: state.input.clone(),
                                    id: None
                                };
                                state.input = "".to_string();
                                database::add_list(&list).ok();
                                state.state = AppState::List;
                            }
                            InputField::TaskTitle => {
                                state.todo_title = state.input.clone();
                                state.input = "".to_string();
                                state.state = AppState::Create;
                            }
                            InputField::TaskDescription => {
                                state.todo_description = state.input.clone();
                                state.input = "".to_string();
                                state.state = AppState::Create;
                            }
                        },

                        _ => {}
                    },
                }
            }
        }
    })
}
