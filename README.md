# todo-tui

A terminal todo manager backed by local SQLite. Run `todo-tui` for the interactive UI or `todo-tui --help` for noninteractive commands. No server or agent is required.

## CLI examples

```sh
todo-tui add 'Write tests' --due 0 --json
todo-tui lists create Work --json
todo-tui inbox --json
todo-tui list --status open --json
todo-tui today --json
todo-tui pinned --agent Ada --json
```

Use the `id` from a task or list result in later commands. For example, with `TASK` and `LIST` set to those IDs:

```sh
todo-tui edit "$TASK" --description 'Use a disposable database'
todo-tui pin "$TASK" --agent Ada --avatar '🐱'
todo-tui next "$TASK"
todo-tui move "$TASK" "$LIST"
todo-tui complete "$TASK" --note 'Tests pass'
todo-tui note "$TASK" 'Confirmed on macOS'
todo-tui show "$TASK" --json
```

Plain `list` and `list --count` still show/count open tasks due through today (or through `--date`). Use `list --json` or `list --status all` for all tasks, `--list ID` for a specific list, and `--next` for open Next tasks. `today` orders overdue, due-today and Next work with pins first, then Next, then due date. Completing a task clears its pins; unpinning does not complete it. Lists must be empty before deletion, and Inbox cannot be renamed or deleted. Use `--yes` for CLI deletions.

`--json` writes one value: an object for a detail or mutation, an array for a collection, or a number for `--count`. Task and list IDs in JSON are opaque stable strings. Legacy numeric IDs still work as local inputs; don't use them across databases. Errors go to stderr: exit `0` on success, `1` on a database/workflow error, and `2` for invalid CLI syntax.

## Data and testing

The default database is `~/.todo/todos.sqlite`. Before upgrading, close the old app and back up the database (for example with SQLite's `.backup` command). The first use migrates its schema in a transaction; don't run older versions of the app against the migrated database. Use `TODO_TUI_DB=/path/to/copy.sqlite` to try the new version against a copy. `cargo test --locked` runs binary-level CLI and migration tests using disposable databases.
