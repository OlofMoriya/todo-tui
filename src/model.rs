use chrono::NaiveDate;
use std::vec::Vec;


#[derive(Debug)]
pub struct Todo {
    pub id: Option<usize>,
    pub list_id: usize,
    pub title: String,
    pub description: Option<String>,
    pub due_date: Option<NaiveDate>,
    pub completed: bool,
    pub completed_date: Option<NaiveDate>,
    pub dependencies: Vec<usize>,
}

#[derive(Debug)]
pub struct TodoList {
    pub id: Option<usize>,
    pub title: String,
}

