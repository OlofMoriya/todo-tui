# todo-tui

A terminal todo manager backed by local SQLite. Run `todo-tui` for the interactive UI or `todo-tui --help` for noninteractive commands. No server or agent is required.

Install with `brew install olofmoriya/tap/todo-tui`, or build from source with `cargo build --release --locked`.

## Terminal UI

Cycle **Pinned**, **Lists**, and **Today** with Tab or Shift-Tab. Press `t` to open **Trash**, and `t` or Esc to return. Use arrow keys or `hjkl` to navigate. Press `v` to show or hide the selected task's details in a full-width pane at the bottom. The pane starts hidden. In a form, move between fields with Up/Down, select a list with Left/Right, and save with Enter on the last field. Ctrl-U clears a text field; Esc cancels.

- `N` adds a task (Inbox is the default); `L` creates a list. In the Lists pane, `R` renames and `D` moves a list to Trash.
- In a task pane, `E` edits, `p` pins or changes the avatar, `u` unpins, `n` toggles Next, and `m` moves the task.
- Space completes or reopens a task immediately. Use `o` to add or edit a completion note later. `D` moves a task to Trash. In Trash, use `R` to restore or `P` to permanently delete after typing `yes`.
- The TUI refreshes the visible lists and tasks while idle, including additions, moves, and completions from another TUI, the CLI, or a sync pull. This local refresh is independent of the SSH sync interval. Press `s` to review sync activity and before/after changes, or `S` to sync now. The footer flags new sync activity and sync problems.

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

Plain `list` and `list --count` still show/count open tasks due through today (or through `--date`). Use `list --json` or `list --status all` for all tasks, `--list ID` for a specific list, and `--next` for open Next tasks. `today` orders overdue, due-today and Next work with pins first, then Next, then due date. Completing a task clears its pins; unpinning does not complete it. Inbox cannot be renamed, trashed, or purged.

`--json` writes one value: an object for a detail or mutation, an array for a collection, or a number for `--count`. Task and list IDs in JSON are opaque stable strings. Legacy numeric IDs still work as local inputs; don't use them across databases. Errors go to stderr: exit `0` on success, `1` on a database/workflow error, and `2` for invalid CLI syntax.

## Trash and recovery

```sh
todo-tui delete "$TASK"                 # archive is an alias
todo-tui lists delete "$LIST"
todo-tui trash --json                   # lists and tasks (including tasks hidden by their list)
todo-tui lists restore "$LIST"          # restores its tasks unless individually trashed
todo-tui restore "$TASK"                # restore its parent list first, if needed
todo-tui purge "$TASK" --yes             # permanent, only from Trash
todo-tui lists purge "$LIST" --yes       # permanent, including every task in a trashed list
```

Trash is hidden from regular lists, Today, and Pinned. `show` still works for a trashed task by ID. There is no automatic purge.

## Optional SSH sync

1. Install the same `todo-tui` version on the Mac mini and MacBook. Upgrade both to 0.3.0 or newer before syncing Trash; older peers are rejected. Enable macOS Remote Login on the mini. As the MacBook user, verify the mini's host key and set up key-based SSH so `ssh -o BatchMode=yes mini true` succeeds. Use your SSH config for an alias, custom port, or IPv6 address.
2. On the MacBook, configure the mini. This starts the first sync immediately and installs a macOS login agent that checks every 60 seconds while you are logged in, even with the TUI closed. Change the interval with `todo-tui sync interval 120` (seconds); applying it reloads the login agent for an existing configuration.

   ```sh
   todo-tui sync configure mini --remote-bin /opt/homebrew/bin/todo-tui
   ```

   Use the mini's actual executable path (`/usr/local/bin/todo-tui` on many Intel Macs); omit `--remote-bin` if SSH can already find it on the mini's PATH. The remote command uses the SSH account's `~/.todo/todos.sqlite`. SQLite backups are created beside both databases as `todos.sqlite.pre-ssh-sync.sqlite` before the first import; they are not overwritten. To review the first import manually instead, configure with `--preview-only`, then run `todo-tui sync preview` and `todo-tui sync --accept-import`. If either database changes after a manual preview, approval shows a fresh preview instead of applying it. Existing records are matched by stable IDs, not by their titles or old integer IDs. Each machine keeps its own Inbox ID.
3. Run `todo-tui sync` whenever you want to push and pull from the shell. `todo-tui sync status --json` reports the pending local changes, last successful sync and reachability from the last check. `todo-tui sync log --json` shows recent transfers (including before/after values), conflicts, and failures. History is stored locally and retains the last 50 events. `todo-tui sync off` removes the login agent, stops future connections, and retains the baseline. Your tasks stay local and editable while disconnected. The mini doesn't need a separate daemon beyond OpenSSH.

If both sides change the same task or list, sync pauses without replacing either version. Inspect them with `todo-tui sync conflicts --json`, then run `todo-tui sync resolve 'task:<id>' local` (or `master`), followed by `todo-tui sync`. For a first-import conflict, follow up with `sync --accept-import` instead. `list:<id>` identifies a list conflict. A list deletion that would orphan a task must be resolved by keeping the list or moving/deleting the remaining task first. `preview`, `conflicts`, and `retry` are normal sync outcomes; use `--json` to distinguish them from `synced`. Transport and local errors use exit status `1`.

This version uses full snapshots, intended for personal-sized databases (32 MiB limit per SSH request/reply). Do not copy an actively syncing SQLite file to repoint a client; the client is bound to the original master database identity.

## Pi agent integration

The [Pi extension](integrations/pi/todo-focus.ts) shows the current Pi session’s pinned work in Pi’s footer. It instructs Pi to call `todo_tui_focus` before substantial work: pin an existing task by ID, or create and pin a short Inbox task. Quick questions and small edits should not create tasks. Pi can release a pin without completing it, or mark finished work complete. The footer refreshes as pins change; `/todo-focus` lists this agent’s pins and IDs.

Install from a local checkout, then use `/reload` in Pi:

```sh
mkdir -p ~/.pi/agent/extensions
ln -s "$PWD/integrations/pi/todo-focus.ts" ~/.pi/agent/extensions/todo-focus.ts
```

It uses `todo-tui` on `PATH` and the same local database as the CLI, including `TODO_TUI_DB` if set. `PI_TODO_BIN` can specify another binary. Each Pi session gets its own pin identity (stable when resumed); set `PI_TODO_AGENT` to use a named identity instead, and `PI_TODO_AVATAR` to change the default avatar. Task titles can sync to your configured master, so keep them free of sensitive details.

## Data and testing

The default database is `~/.todo/todos.sqlite`. Before upgrading, close the old app and back up the database (for example with SQLite's `.backup` command). The first use migrates its schema in a transaction; don't run older versions of the app against the migrated database. Use `TODO_TUI_DB=/path/to/copy.sqlite` to try the new version against a copy. `cargo test --locked` runs binary-level CLI and migration tests using disposable databases.
