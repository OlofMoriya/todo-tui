use clap::Parser;

mod cli;
mod database;
mod model;
mod snapshot;
mod sync;
mod sync_transport;
mod tui;

fn main() -> std::process::ExitCode {
    match cli::Cli::parse().run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("todo-tui: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
