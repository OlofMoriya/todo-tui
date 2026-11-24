use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Todo {
    #[serde(skip)]
    pub id: Option<usize>,
    #[serde(rename = "id")]
    pub uid: String,
    #[serde(rename = "list_id")]
    pub list_uid: String,
    pub list_title: String,
    pub title: String,
    pub description: Option<String>,
    pub due_date: Option<NaiveDate>,
    pub completed: bool,
    pub completed_date: Option<NaiveDate>,
    pub next: bool,
    pub completion_note: Option<String>,
    pub pins: Vec<Pin>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pin {
    pub agent: String,
    pub avatar: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TodoList {
    #[serde(skip)]
    pub id: Option<usize>,
    #[serde(rename = "id")]
    pub uid: String,
    pub title: String,
}
