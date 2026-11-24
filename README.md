# todo-tui

A terminal todo manager backed by local SQLite. Run `todo-tui` for the interactive UI or `todo-tui --help` for noninteractive commands. No server or agent is required.

Install with `brew install olofmoriya/tap/todo-tui`, or build from source with `cargo build --release --locked`.

## Terminal UI

Cycle **Pinned**, **Lists**, and **Today** with Tab or Shift-Tab. Use arrow keys or `hjkl` to navigate. Press `v` to show or hide the selected task's details in a full-width pane at the bottom. The pane starts hidden. In a form, move between fields with Up/Down, select a list with Left/Right, and save with Enter on the last field. Ctrl-U clears a text field; Esc cancels.

- `N` adds a task (Inbox is the default); `L` creates a list. In the Lists pane, `R` renames and `D` deletes an empty list.
- In a task pane, `E` edits, `p` pins or changes the avatar, `u` unpins, `n` toggles Next, and `m` moves the task.
- Space completes or reopens a task; `o` edits the completion note. `D` asks for confirmation before deleting.
- Adds, pins, and other writes from another local process appear automatically. With SSH sync enabled and the first import approved, the TUI checks the master at startup and every five seconds in the background. Press `S` to check now.

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

## Optional SSH sync

1. Install the same `todo-tui` version on the Mac mini and MacBook. Enable macOS Remote Login on the mini. As the MacBook user, verify the mini's host key and set up key-based SSH so `ssh -o BatchMode=yes mini true` succeeds. Use your SSH config for an alias, custom port, or IPv6 address.
2. On the MacBook, configure the mini, preview the first import, and approve it:

   ```sh
   todo-tui sync configure mini --remote-bin /opt/homebrew/bin/todo-tui
   todo-tui sync preview
   todo-tui sync --accept-import
   ```

   Use the mini's actual executable path (`/usr/local/bin/todo-tui` on many Intel Macs); omit `--remote-bin` if SSH can already find it on the mini's PATH. The remote command uses the SSH account's `~/.todo/todos.sqlite`. SQLite backups are created beside both databases as `todos.sqlite.pre-ssh-sync.sqlite` before the first import; they are not overwritten. The preview does not change task/list contents. If either database changes after the preview, approval shows a fresh preview instead of applying it. Existing records are matched by stable IDs, not by their titles or old integer IDs. Each machine keeps its own Inbox ID.
3. Run `todo-tui sync` whenever you want to push and pull from the shell. `todo-tui sync status --json` reports the pending local changes, last successful sync and reachability from the last check. `todo-tui sync off` stops future connections and retains the baseline. Your tasks stay local and editable while disconnected. The mini doesn't need a separate daemon beyond OpenSSH.

If both sides change the same task or list, sync pauses without replacing either version. Inspect them with `todo-tui sync conflicts --json`, then run `todo-tui sync resolve 'task:<id>' local` (or `master`), followed by `todo-tui sync`. For a first-import conflict, follow up with `sync --accept-import` instead. `list:<id>` identifies a list conflict. A list deletion that would orphan a task must be resolved by keeping the list or moving/deleting the remaining task first. `preview`, `conflicts`, and `retry` are normal sync outcomes; use `--json` to distinguish them from `synced`. Transport and local errors use exit status `1`.

This version uses full snapshots, intended for personal-sized databases (32 MiB limit per SSH request/reply). Do not copy an actively syncing SQLite file to repoint a client; the client is bound to the original master database identity.

## Data and testing

The default database is `~/.todo/todos.sqlite`. Before upgrading, close the old app and back up the database (for example with SQLite's `.backup` command). The first use migrates its schema in a transaction; don't run older versions of the app against the migrated database. Use `TODO_TUI_DB=/path/to/copy.sqlite` to try the new version against a copy. `cargo test --locked` runs binary-level CLI and migration tests using disposable databases.
