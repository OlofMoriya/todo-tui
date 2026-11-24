# todo-tui rebuild

## Rebuild status

- **Rebuilt:** local task/list workflows, the JSON CLI, and the Pinned, Lists, and Today tabs. The CLI and TUI use the same SQLite queries and mutations. The migration is covered by tests against disposable copies of the legacy schema.
- **Rebuilt:** optional local-first SSH sync with first-import preview, SQLite backups, offline edits, conflict resolution, and retry receipts. The TUI refreshes from local writes continuously and syncs in the background when configured. No normal CLI query makes a network call.
- **Still open:** Owl actions.
- **Owl prerequisite:** verify a tool-disabled request path before sending task text. The installed Owl CLI exposes agent roles, but selecting a role is not the same as disabling tools.

## Keep working

- Local SQLite remains the default. The TUI and existing `list` and `add` commands keep working without SSH or an agent.
- Preserve existing todos and lists. Test schema migrations on copies before using real data.

## 1. Task workflow

- **Currently working:** Humans and agents can pin and unpin tasks through the TUI or CLI. Each pin records who is working on it and shows their selectable avatar (e.g. Pikachu). Support multiple simultaneous pins across lists so different agents can show their work. Clearing a pin does not complete the task.
- **Next:** Add a `next` tag that actually affects ordering: show those tasks ahead of ordinary open tasks, below the current focus. Expose the tag in CLI output and filters; completed tasks should not appear in the Next view.
- **Completion comments:** Store a separate completion note, not a rewrite of the task description. Humans and agents can set it when completing a task and update it later by ID. Show it in task details and JSON.
- **Move between lists:** Move an existing task to a different list from either the TUI or CLI, keeping its ID, dates, status, tags, and completion note. Reject a nonexistent destination without changing the task.
- **Inbox:** Keep a dedicated Inbox as the default destination for new tasks, including agent-created ones, unless a list is explicitly provided. Make triage a move, not a copy.

### Three TUI views

Use `Tab` and `Shift-Tab` to cycle between views when not editing text. Keep the active view visible in the tab bar.

| View | What it shows |
| --- | --- |
| **Pinned** | All currently pinned tasks across lists, grouped by human or agent, with avatars and task details. |
| **Lists** | The regular list browser, including Inbox, task details, and moves between lists. |
| **Today** | The most pressing open work: overdue items, items due today, and tasks tagged Next. Works without Owl; an optional Owl summary can explain priorities. |

## 2. Agent-friendly CLI

**The CLI is a first-class interface and ships before the new TUI.** Every TUI query and action must also work noninteractively from a shell. Use one shared application layer for validation, ordering, mutations, and sync so behavior cannot diverge. Include human-readable and stable JSON output for queries and command results, documented exit codes, and help with runnable examples. Test each workflow through the actual binary with a disposable database; no TTY should be required.

- **Pinned:** List pinned tasks across lists, filter by agent, and pin/unpin by task ID and agent identity. Allow choosing or changing the pin's avatar.
- **Lists:** Browse, create, rename, and delete lists; inspect Inbox and task details; move tasks by ID; set or clear Next; and set or update completion comments. Keep TUI and CLI behavior consistent, including safeguards for destructive actions.
- **Today:** Return the same ordered priorities as the Today tab, with an explicit command for the opt-in Owl summary. No Owl call for a normal Today query.
- **Sync:** Expose configuration and status, first-import preview, a manual sync, and conflict inspection/resolution without opening the TUI.

- List and show todos as JSON, with filters for list, due date, and completion. Preserve current human-readable output and `list --count`.
- Give todos stable IDs that work across machines. Add, edit, complete, and reopen by ID without a TUI or prompts.
- Return useful errors and exit codes. Keep JSON on stdout and errors on stderr.
- Expose current focus, Next, completion notes, and list moves to agents through the same CLI. Bulk edits and search are open questions.

## 3. Local-first sync with an optional master

- Designate the Mac mini as the master over SSH. The MacBook always reads and writes local SQLite, including while offline.
- With sync configured, queue edits durably and push/pull on startup, periodically while the TUI is open, and on demand with `todo-tui sync`. With sync off, make no network calls.
- Sync todos and lists, including moves, focus, Next, completion notes, and deletions; retries must not duplicate tasks or resurrect deleted items. Preserve both versions of conflicting edits rather than silently overwriting one.
- Show last successful sync, pending changes, conflicts, and whether the master is reachable.
- Before first sync, back up both databases and preview what will be imported. Do not treat matching old integer IDs as proof two todos are the same.
- Acceptance test: with two disposable databases, edit offline, reconnect, push and pull, retry, and verify additions, updates, deletions, and conflicts.

## 4. Optional Owl assistance

- Off by default. Configure Owl explicitly, then offer on-demand actions to review tasks and summarize today's most important work.
- Ground the summary in the current focus, Next tags, and due dates. Let the user choose which lists are included.
- Keep suggestions read-only by default. Do not send todo text to Owl until the user opts in and requests an action; make any task changes explicit.
- If Owl is unavailable, show a useful error and keep all ordinary todo features working.

## Later

- Build a `todo-tui mcp` subcommand for Claude Desktop on macOS, using local stdio where supported by the client. Claude Desktop calls typed MCP tools without shell access; the client launches the server. The MCP tools, CLI, and TUI share the same application layer and local database. Start with Today, show, pin/unpin, and complete with a note. Expose no arbitrary shell commands. Test discovery and a complete workflow in Claude Desktop. Claude on the web would need a separate reachable connector; do not expose this local server by default.
- Keep Calendar, Notes, and broader Owl integration out of this MCP subcommand for now.
- Commit and push each working milestone before starting the next one.
