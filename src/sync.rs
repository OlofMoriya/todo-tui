use std::collections::BTreeMap;

use chrono::Utc;
use clap::{Subcommand, ValueEnum};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::{
    database::{self as db, Result},
    snapshot::{self, Delta, Record, Snapshot},
    sync_background::DEFAULT_INTERVAL_SECS,
    sync_log::{self, Entry, Kind},
    sync_transport::{self as ssh, Attempt, Target},
};

#[derive(Subcommand)]
pub enum SyncAction {
    /// Configure the client and start syncing automatically
    Configure {
        host: String,
        #[arg(long, default_value = "todo-tui")]
        remote_bin: String,
        /// Require manual approval for the first import
        #[arg(long)]
        preview_only: bool,
    },
    /// Disable all client SSH connections, keeping the existing baseline
    Off,
    /// Show the stored status without connecting
    Status,
    /// Set how often automatic SSH sync runs (in seconds)
    Interval {
        #[arg(value_parser = clap::value_parser!(u64).range(1..=86_400))]
        seconds: u64,
    },
    /// Review recent transfers and sync issues
    Log,
    /// Show exactly what the first import would push and pull
    Preview,
    /// Inspect both versions (and the prior baseline) of each conflict
    Conflicts,
    /// Choose which version to keep; run sync again to apply
    Resolve {
        key: String,
        #[arg(value_enum)]
        keep: Choice,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    Local,
    Master,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Conflict {
    key: String,
    base: Option<Record>,
    local: Option<Record>,
    master: Option<Record>,
    reason: String,
    keep: Option<Choice>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Preview {
    master: String,
    local: Snapshot,
    remote: Snapshot,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct State {
    target: Option<Target>,
    #[serde(default)]
    automatic_import: bool,
    #[serde(default)]
    interval_secs: Option<u64>,
    master: Option<String>,
    base: Option<Snapshot>,
    preview: Option<Preview>,
    pending: Option<Attempt>,
    conflicts: Vec<Conflict>,
    last_success: Option<String>,
    reachable: Option<bool>,
    last_error: Option<String>,
}

impl State {
    fn interval(&self) -> u64 {
        self.interval_secs.unwrap_or(DEFAULT_INTERVAL_SECS)
    }
}

pub fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS sync_client(singleton INTEGER PRIMARY KEY CHECK(singleton=1), state TEXT NOT NULL)")?;
    sync_log::init(conn)
}

fn load(conn: &Connection) -> Result<State> {
    let state: Option<String> = conn
        .query_row("SELECT state FROM sync_client WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .optional()?;
    state
        .map(|s| Ok(serde_json::from_str(&s)?))
        .transpose()
        .map(|s| s.unwrap_or_default())
}

pub fn is_client(conn: &Connection) -> Result<bool> {
    let s = load(conn)?;
    Ok(s.target.is_some() || s.master.is_some())
}

fn save(conn: &Connection, state: &State) -> Result<()> {
    conn.execute("INSERT INTO sync_client VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET state=excluded.state", [serde_json::to_string(state)?])?;
    Ok(())
}

fn update(
    conn: &mut Connection,
    f: impl FnOnce(&Connection, &mut State) -> Result<()>,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut state = load(&tx)?;
    f(&tx, &mut state)?;
    save(&tx, &state)?;
    tx.commit()?;
    Ok(())
}

fn unchanged(current: &State, expected: &State) -> Result<()> {
    if current != expected {
        return Err("another sync command changed the sync state; retry".into());
    }
    Ok(())
}

fn record_conflict(
    key: &str,
    reason: &str,
    base: &Snapshot,
    local: &Snapshot,
    remote: &Snapshot,
    old: &[Conflict],
) -> Conflict {
    let mut conflict = Conflict {
        key: key.into(),
        base: base.get(key).cloned(),
        local: local.get(key).cloned(),
        master: remote.get(key).cloned(),
        reason: reason.into(),
        keep: None,
    };
    conflict.keep = old
        .iter()
        .find(|c| {
            c.key == conflict.key
                && c.base == conflict.base
                && c.local == conflict.local
                && c.master == conflict.master
        })
        .and_then(|c| c.keep);
    conflict
}

fn merge(
    base: &Snapshot,
    local: &Snapshot,
    remote: &Snapshot,
    old: &[Conflict],
) -> (Snapshot, Vec<Conflict>) {
    let mut desired = Snapshot::new();
    let mut conflicts = BTreeMap::new();
    for key in snapshot::keys(local, remote) {
        let (b, l, r) = (base.get(&key), local.get(&key), remote.get(&key));
        let chosen = if l == r || r == b {
            l
        } else if l == b {
            r
        } else {
            let conflict = record_conflict(
                &key,
                "both sides changed this record",
                base,
                local,
                remote,
                old,
            );
            let chosen = if conflict.keep == Some(Choice::Master) {
                r
            } else {
                l
            };
            conflicts.insert(key.clone(), conflict);
            chosen
        };
        if let Some(r) = chosen {
            desired.insert(key, r.clone());
        }
    }
    let missing: Vec<_> = desired
        .values()
        .filter_map(|r| match r {
            Record::Task { list, .. } if !desired.contains_key(list) => Some(list.clone()),
            _ => None,
        })
        .collect();
    for list in missing {
        let mut conflict = record_conflict(&list, "deleting this list would orphan a task; keep the list or move/delete its remaining tasks first", base, local, remote, old);
        let retained = match conflict.keep {
            Some(Choice::Local) => conflict.local.as_ref(),
            Some(Choice::Master) => conflict.master.as_ref(),
            None => None,
        };
        if let Some(retained) = retained {
            desired.insert(list.clone(), retained.clone());
        } else {
            conflict.keep = None;
        }
        conflicts.insert(list, conflict);
    }
    (desired, conflicts.into_values().collect())
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Mode {
    Normal,
    Preview,
    Accept,
    Automatic,
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Off,
    AwaitingPreview,
    Preview {
        master: String,
        upload: Vec<Delta>,
        download: Vec<Delta>,
        changed_since_preview: bool,
        conflicting_keys: Vec<String>,
    },
    Conflicts {
        unresolved: usize,
    },
    Synced {
        uploaded: usize,
        downloaded: usize,
    },
    Retry,
}

impl Outcome {
    pub fn message(&self) -> String {
        match self {
            Self::Off => "SSH sync is off".into(),
            Self::AwaitingPreview => "SSH sync needs first-import approval: run `todo-tui sync preview`".into(),
            Self::Preview { upload, download, changed_since_preview, .. } => format!("{}First import: {} record(s) to upload, {} to download. Run `todo-tui sync --accept-import` to apply.", if *changed_since_preview { "Data changed; review this new preview. " } else { "" }, upload.len(), download.len()),
            Self::Conflicts { unresolved } => format!("SSH sync paused on {unresolved} conflict(s); see `todo-tui sync conflicts`"),
            Self::Synced { uploaded, downloaded } => format!("SSH sync complete ({uploaded} uploaded, {downloaded} downloaded)"),
            Self::Retry => "The master changed during sync. No snapshot was overwritten; retry sync.".into(),
        }
    }
}

fn open_state() -> Result<Connection> {
    let conn = db::open_db()?;
    init(&conn)?;
    Ok(conn)
}

fn success(tx: &Connection, uploaded: Vec<Delta>, downloaded: Vec<Delta>) -> Result<Outcome> {
    let outcome = Outcome::Synced {
        uploaded: uploaded.len(),
        downloaded: downloaded.len(),
    };
    if !uploaded.is_empty()
        || !downloaded.is_empty()
        || sync_log::head(tx)?.is_none_or(|event| event.entry.kind != Kind::Synced)
    {
        sync_log::write(
            tx,
            Entry {
                kind: Kind::Synced,
                message: outcome.message(),
                uploaded,
                downloaded,
            },
        )?;
    }
    Ok(outcome)
}

fn finalize(conn: &mut Connection, state: &State, attempt: &Attempt) -> Result<Outcome> {
    let mut outcome = Outcome::Retry;
    update(conn, |tx, s| {
        unchanged(s, state)?;
        let local = snapshot::read(tx)?;
        let (desired, conflicts) = merge(&attempt.local, &local, &attempt.desired, &s.conflicts);
        let unresolved = conflicts.iter().filter(|c| c.keep.is_none()).count();
        s.conflicts = conflicts;
        s.reachable = Some(true);
        s.last_error = None;
        if unresolved > 0 {
            outcome = Outcome::Conflicts { unresolved };
            return Ok(());
        }
        snapshot::apply(tx, &local, &desired)?;
        s.last_success = Some(Utc::now().to_rfc3339());
        s.base = Some(attempt.desired.clone());
        s.pending = None;
        s.preview = None;
        s.conflicts.clear();
        outcome = success(
            tx,
            snapshot::delta(&attempt.expected, &attempt.desired),
            snapshot::delta(&local, &desired),
        )?;
        Ok(())
    })?;
    Ok(outcome)
}

fn cycle(
    conn: &mut Connection,
    initial: State,
    mode: Mode,
    reachable: &mut Option<bool>,
) -> Result<Outcome> {
    let Some(target) = initial.target.as_ref() else {
        return Ok(Outcome::Off);
    };
    if mode == Mode::Automatic
        && !initial.automatic_import
        && initial.base.is_none()
        && initial.pending.is_none()
    {
        return Ok(Outcome::AwaitingPreview);
    }
    if mode == Mode::Preview && (initial.base.is_some() || initial.pending.is_some()) {
        return Err("first import already accepted; use `todo-tui sync`".into());
    }
    if mode == Mode::Accept && initial.base.is_some() && initial.pending.is_none() {
        return Err("first import already completed; use `todo-tui sync`".into());
    }
    // The existing client config was saved only after making a SQLite backup.
    if let Some(attempt) = initial.pending.as_ref() {
        *reachable = Some(false);
        let accepted = ssh::apply(
            target,
            initial.master.as_deref().ok_or("missing bound master")?,
            attempt,
        )?;
        *reachable = Some(true);
        if accepted {
            return finalize(conn, &initial, attempt);
        }
        update(conn, |_, s| {
            unchanged(s, &initial)?;
            s.pending = None;
            s.reachable = Some(true);
            s.last_error = None;
            Ok(())
        })?;
        return Ok(Outcome::Retry);
    }
    *reachable = Some(false);
    let (master, remote) = ssh::fetch(target)?;
    *reachable = Some(true);
    if initial.master.as_ref().is_some_and(|m| *m != master) {
        return Err(
            "SSH target is not the bound master database; refusing to overwrite either database"
                .into(),
        );
    }
    let mut result = None;
    let mut next = None;
    update(conn, |tx, s| {
        unchanged(s, &initial)?;
        let local = snapshot::read(tx)?;
        s.reachable = Some(true);
        s.last_error = None;
        if s.base.is_none() {
            let current = Preview {
                master: master.clone(),
                local: local.clone(),
                remote: remote.clone(),
            };
            if mode != Mode::Accept || s.preview.as_ref() != Some(&current) {
                let empty = Snapshot::new();
                let (merged, conflicts) = merge(&empty, &local, &remote, &[]);
                let conflicting_keys: Vec<_> = conflicts.into_iter().map(|c| c.key).collect();
                let changes = |from| {
                    snapshot::delta(from, &merged)
                        .into_iter()
                        .filter(|d| !conflicting_keys.contains(&d.key))
                        .collect()
                };
                result = Some(Outcome::Preview {
                    master: master.clone(),
                    upload: changes(&remote),
                    download: changes(&local),
                    changed_since_preview: mode == Mode::Accept && s.preview.is_some(),
                    conflicting_keys,
                });
                s.preview = Some(current);
                return Ok(());
            }
        }
        let (desired, conflicts) = merge(
            s.base.as_ref().unwrap_or(&Snapshot::new()),
            &local,
            &remote,
            &s.conflicts,
        );
        let unresolved = conflicts.iter().filter(|c| c.keep.is_none()).count();
        s.conflicts = conflicts;
        if unresolved > 0 {
            result = Some(Outcome::Conflicts { unresolved });
            return Ok(());
        }
        if local == remote && desired == remote {
            s.master = Some(master.clone());
            s.base = Some(desired);
            s.preview = None;
            s.conflicts.clear();
            s.last_success = Some(Utc::now().to_rfc3339());
            result = Some(success(tx, vec![], vec![])?);
            return Ok(());
        }
        let attempt = Attempt {
            id: ssh::random_id(tx)?,
            local,
            expected: remote.clone(),
            desired,
        };
        s.master = Some(master.clone());
        s.pending = Some(attempt);
        next = Some(s.clone());
        Ok(())
    })?;
    if let Some(result) = result {
        return Ok(result);
    }
    let next = next.ok_or("sync did not prepare an operation")?;
    let attempt = next.pending.as_ref().ok_or("sync operation missing")?;
    *reachable = Some(false);
    let accepted = ssh::apply(target, &master, attempt)?;
    *reachable = Some(true);
    if accepted {
        finalize(conn, &next, attempt)
    } else {
        update(conn, |_, s| {
            unchanged(s, &next)?;
            s.pending = None;
            Ok(())
        })?;
        Ok(Outcome::Retry)
    }
}

pub fn run(mode: Mode) -> Result<Outcome> {
    let mut conn = open_state()?;
    let state = load(&conn)?;
    let mut reachable = None;
    let attempt = (|| -> Result<Outcome> {
        let result = cycle(&mut conn, state.clone(), mode, &mut reachable)?;
        if mode == Mode::Automatic
            && state.automatic_import
            && matches!(result, Outcome::Preview { .. })
        {
            let reviewed = load(&conn)?;
            if reviewed.target == state.target
                && reviewed.automatic_import
                && reviewed.base.is_none()
            {
                return cycle(&mut conn, reviewed, Mode::Accept, &mut reachable);
            }
        }
        Ok(result)
    })();
    let issue = match &attempt {
        Err(e) => Some(Entry::issue(Kind::Error, e.to_string())),
        Ok(outcome @ Outcome::Conflicts { .. }) => {
            Some(Entry::issue(Kind::Conflicts, outcome.message()))
        }
        Ok(outcome @ Outcome::Retry) => Some(Entry::issue(Kind::Retry, outcome.message())),
        _ => None,
    };
    if let Some(issue) = issue {
        let logged = update(&mut conn, |tx, current| {
            if current.target == state.target && current.target.is_some() {
                if issue.kind == Kind::Error {
                    if reachable.is_some() {
                        current.reachable = reachable;
                    }
                    current.last_error = Some(issue.message.clone());
                }
                sync_log::write(tx, issue)?;
            }
            Ok(())
        });
        if attempt.is_ok() {
            logged?;
        }
    }
    attempt
}

#[derive(Serialize)]
struct Status {
    target: Option<Target>,
    background_active: bool,
    interval_secs: u64,
    first_import_pending: bool,
    pending_changes: Option<usize>,
    pending_attempt: bool,
    conflicts: usize,
    reachable: Option<bool>,
    last_success: Option<String>,
    last_error: Option<String>,
}

fn status(conn: &mut Connection) -> Result<Status> {
    let tx = conn.transaction()?;
    let s = load(&tx)?;
    let pending_changes = if s.target.is_some() || s.base.is_some() {
        let snap = snapshot::read(&tx)?;
        let empty = Snapshot::new();
        Some(
            snapshot::delta(s.base.as_ref().unwrap_or(&empty), &snap)
                .iter()
                .filter(|d| d.key != "list:inbox")
                .count(),
        )
    } else {
        None
    };
    Ok(Status {
        first_import_pending: s.target.is_some() && s.base.is_none(),
        background_active: s.target.is_some() && crate::sync_background::active(),
        interval_secs: s.interval(),
        target: s.target,
        pending_changes,
        pending_attempt: s.pending.is_some(),
        conflicts: s.conflicts.iter().filter(|c| c.keep.is_none()).count(),
        reachable: s.reachable,
        last_success: s.last_success,
        last_error: s.last_error,
    })
}

pub fn automatic_settings() -> Result<(bool, u64)> {
    let conn = open_state()?;
    let s = load(&conn)?;
    Ok((
        s.target.is_some() && (s.automatic_import || s.base.is_some() || s.pending.is_some()),
        s.interval(),
    ))
}

pub fn cli(
    action: Option<SyncAction>,
    accept: bool,
    stdio: bool,
    automatic: bool,
    json: bool,
) -> Result<()> {
    if stdio {
        return ssh::serve();
    }
    fn out(value: &impl Serialize, json: bool, human: impl FnOnce()) -> Result<()> {
        if json {
            println!("{}", serde_json::to_string(value)?);
        } else {
            human();
        }
        Ok(())
    }
    match action {
        None | Some(SyncAction::Preview) => {
            let mode = if action.is_some() {
                Mode::Preview
            } else if accept {
                Mode::Accept
            } else if automatic {
                Mode::Automatic
            } else {
                Mode::Normal
            };
            let result = run(mode)?;
            out(&result, json, || {
                println!("{}", result.message());
                if let Outcome::Preview {
                    upload,
                    download,
                    conflicting_keys,
                    ..
                } = &result
                {
                    for key in conflicting_keys {
                        println!("  conflict {key}: approval will pause to review both versions");
                    }
                    for (direction, changes) in [("upload", upload), ("download", download)] {
                        for d in changes {
                            let operation = match (&d.before, &d.after) {
                                (None, _) => "add",
                                (_, None) => "delete",
                                _ => "update",
                            };
                            let title = match d.after.as_ref().or(d.before.as_ref()) {
                                Some(Record::List { title, .. } | Record::Task { title, .. }) => {
                                    title.as_str()
                                }
                                None => "",
                            };
                            println!("  {direction} {operation} {} {title:?}", d.key);
                        }
                    }
                }
            })
        }
        Some(SyncAction::Configure {
            host,
            remote_bin,
            preview_only,
        }) => {
            let target = Target::new(host, remote_bin)?;
            let mut conn = db::open(true)?;
            init(&conn)?;
            update(&mut conn, |tx, s| {
                let master = tx
                    .query_row(
                        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='sync_master'",
                        [],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if master {
                    return Err("this database already serves as an SSH master".into());
                }
                if s.target.as_ref() != Some(&target) {
                    s.target = Some(target.clone());
                    s.preview = None;
                    s.reachable = None;
                    s.last_error = None;
                }
                s.automatic_import = !preview_only;
                Ok(())
            })?;
            let first = if preview_only {
                Ok(None)
            } else {
                run(Mode::Automatic).map(Some)
            };
            let interval_secs = load(&conn)?.interval();
            let background_active = crate::sync_background::enable(interval_secs)?;
            let first = first?;
            #[derive(Serialize)]
            struct Configured<'a> {
                #[serde(flatten)]
                target: &'a Target,
                background_active: bool,
                sync: Option<Outcome>,
            }
            let configured = Configured {
                target: &target,
                background_active,
                sync: first,
            };
            out(&configured, json, || {
                println!("SSH master: {}", target.host);
                if let Some(outcome) = &configured.sync {
                    println!("{}", outcome.message());
                } else {
                    println!("Run `todo-tui sync preview` for the first import.");
                }
                if background_active {
                    println!("Background sync is active, even when the TUI is closed.");
                } else {
                    println!("No login agent is running right now. The open TUI can sync in the background.");
                }
            })
        }
        Some(SyncAction::Interval { seconds }) => {
            let mut conn = open_state()?;
            let mut configured = false;
            update(&mut conn, |_, s| {
                s.interval_secs = Some(seconds);
                configured = s.target.is_some();
                Ok(())
            })?;
            let background_active = configured && crate::sync_background::enable(seconds)?;
            out(
                &serde_json::json!({ "interval_secs": seconds, "background_active": background_active }),
                json,
                || {
                    println!("Automatic SSH sync interval: {seconds} seconds");
                },
            )
        }
        Some(SyncAction::Off) => {
            let mut conn = open_state()?;
            update(&mut conn, |_, s| {
                s.target = None;
                s.preview = None;
                s.reachable = None;
                s.last_error = None;
                Ok(())
            })?;
            crate::sync_background::disable()?;
            out(&Outcome::Off, json, || {
                println!("SSH sync is off; local tasks and the sync baseline are unchanged")
            })
        }
        Some(SyncAction::Status) => {
            let s = status(&mut open_state()?)?;
            out(&s, json, || {
                println!(
                    "Master: {}",
                    s.target.as_ref().map_or("off", |t| t.host.as_str())
                );
                println!(
                    "Background login agent: {}",
                    if s.background_active {
                        "active"
                    } else {
                        "inactive"
                    }
                );
                println!(
                    "Reachable (last check): {}",
                    s.reachable
                        .map_or("unknown", |b| if b { "yes" } else { "no" })
                );
                println!(
                    "Last successful sync: {}",
                    s.last_success.as_deref().unwrap_or("never")
                );
                if let Some(n) = s.pending_changes {
                    println!("Pending local changes: {n}");
                }
                println!("Automatic sync interval: {} seconds", s.interval_secs);
                println!("Unresolved conflicts: {}", s.conflicts);
                if s.first_import_pending {
                    println!("First import pending");
                }
                if s.pending_attempt {
                    println!("An interrupted sync will resume on retry");
                }
                if let Some(e) = &s.last_error {
                    println!("Last error: {e}");
                }
            })
        }
        Some(SyncAction::Log) => {
            let rows = sync_log::recent(&open_state()?, 50)?;
            out(&rows, json, || {
                if rows.is_empty() {
                    println!("No sync activity yet");
                }
                for event in &rows {
                    println!("{}  {}", event.at, event.entry.message);
                }
            })
        }
        Some(SyncAction::Conflicts) => {
            let conflicts = load(&open_state()?)?.conflicts;
            out(&conflicts, json, || {
                if conflicts.is_empty() {
                    println!("No sync conflicts");
                }
                for conflict in &conflicts {
                    println!(
                        "{}: {}{}",
                        conflict.key,
                        conflict.reason,
                        conflict
                            .keep
                            .map_or(String::new(), |c| format!(" (keep {c:?})"))
                    );
                    for (label, record) in [
                        ("Base", &conflict.base),
                        ("Local", &conflict.local),
                        ("Master", &conflict.master),
                    ] {
                        println!(
                            "  {label}: {}",
                            serde_json::to_string(record).expect("record is serializable")
                        );
                    }
                }
            })
        }
        Some(SyncAction::Resolve { key, keep }) => {
            let mut conn = open_state()?;
            let mut resolution = None;
            update(&mut conn, |_, s| {
                let conflict = s
                    .conflicts
                    .iter_mut()
                    .find(|c| c.key == key)
                    .ok_or("unknown sync conflict key")?;
                if conflict.reason.starts_with("deleting this list")
                    && match keep {
                        Choice::Local => conflict.local.is_none(),
                        Choice::Master => conflict.master.is_none(),
                    }
                {
                    return Err(
                        "that version deletes a list with tasks; move or delete those tasks first"
                            .into(),
                    );
                }
                conflict.keep = Some(keep);
                resolution = Some(conflict.clone());
                Ok(())
            })?;
            out(&resolution, json, || {
                println!("Resolution recorded; run `todo-tui sync` (or `todo-tui sync --accept-import` during first import)")
            })
        }
    }
}
