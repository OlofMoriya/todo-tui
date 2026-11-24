use std::{
    io::{self, Read, Write},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::{
    database::{self as db, Result},
    snapshot::{self, Snapshot},
};

const MAX: u64 = 32 * 1024 * 1024;
const PROTOCOL: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Target {
    pub host: String,
    pub remote_bin: String,
}

impl Target {
    pub fn new(host: String, remote_bin: String) -> Result<Self> {
        let valid = |c: u8| c.is_ascii_alphanumeric() || b"._-".contains(&c);
        let (user, name) = host
            .split_once('@')
            .map_or((None, host.as_str()), |(u, h)| (Some(u), h));
        let starts = |s: &str| s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric);
        if !starts(name)
            || !name.bytes().all(valid)
            || user.is_some_and(|u| !starts(u) || !u.bytes().all(valid))
        {
            return Err("use an SSH host alias or [user@]hostname (configure custom ports and IPv6 in ~/.ssh/config)".into());
        }
        if remote_bin != "todo-tui"
            && !(remote_bin.starts_with('/') && remote_bin.bytes().all(|c| c == b'/' || valid(c)))
        {
            return Err("--remote-bin must be an absolute executable path without spaces".into());
        }
        Ok(Self { host, remote_bin })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Attempt {
    pub id: String,
    pub local: Snapshot,
    pub expected: Snapshot,
    pub desired: Snapshot,
}

#[derive(Deserialize, Serialize)]
struct Request {
    version: u32,
    action: Action,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Action {
    Fetch,
    Apply {
        master: String,
        id: String,
        expected: Snapshot,
        desired: Snapshot,
    },
}

#[derive(Deserialize, Serialize)]
struct Reply {
    version: u32,
    response: Response,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Response {
    Fetched { master: String, data: Snapshot },
    Applied,
    Stale,
    Error { message: String },
}

fn limited(input: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    input.take(MAX + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX {
        return Err(io::Error::other(
            "sync snapshot exceeds the 32 MiB protocol limit",
        ));
    }
    Ok(bytes)
}

fn call(target: &Target, action: Action) -> Result<Response> {
    let target = Target::new(target.host.clone(), target.remote_bin.clone())?;
    let data = serde_json::to_vec(&Request {
        version: PROTOCOL,
        action,
    })?;
    if data.len() as u64 > MAX {
        return Err("sync request exceeds the 32 MiB protocol limit".into());
    }
    let mut child = Command::new("ssh")
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=2",
            "--",
            &target.host,
            &target.remote_bin,
            "sync",
            "--stdio",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let errors = child.stderr.take().unwrap();
    let writer = thread::spawn(move || input.write_all(&data));
    let reader = thread::spawn(move || limited(output));
    let stderr = thread::spawn(move || {
        let mut errors = errors;
        let mut first = Vec::new();
        errors.by_ref().take(4096).read_to_end(&mut first)?;
        io::copy(&mut errors, &mut io::sink())?;
        Ok::<_, io::Error>(first)
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(result
                    .err()
                    .unwrap_or_else(|| {
                        io::Error::new(io::ErrorKind::TimedOut, "SSH sync timed out")
                    })
                    .into());
            }
        }
    };
    let error_text = stderr.join().map_err(|_| "SSH stderr reader stopped")??;
    let received = reader.join().map_err(|_| "SSH response reader stopped")?;
    let sent = writer.join().map_err(|_| "SSH request writer stopped")?;
    if !status.success() {
        return Err(format!(
            "SSH master {}: {} ({status})",
            target.host,
            String::from_utf8_lossy(&error_text).trim()
        )
        .into());
    }
    sent?;
    let reply: Reply = serde_json::from_slice(&received?).map_err(|e| {
        format!("invalid response from SSH master (install a matching todo-tui): {e}")
    })?;
    if reply.version != PROTOCOL {
        return Err("SSH master uses a different sync protocol version".into());
    }
    match reply.response {
        Response::Error { message } => Err(format!("master rejected sync: {message}").into()),
        response => Ok(response),
    }
}

pub fn fetch(target: &Target) -> Result<(String, Snapshot)> {
    match call(target, Action::Fetch)? {
        Response::Fetched { master, data } => {
            snapshot::validate(&data)?;
            Ok((master, data))
        }
        _ => Err("unexpected SSH fetch reply".into()),
    }
}

pub fn apply(target: &Target, master: &str, attempt: &Attempt) -> Result<bool> {
    match call(
        target,
        Action::Apply {
            master: master.into(),
            id: attempt.id.clone(),
            expected: attempt.expected.clone(),
            desired: attempt.desired.clone(),
        },
    )? {
        Response::Applied => Ok(true),
        Response::Stale => Ok(false),
        _ => Err("unexpected SSH apply reply".into()),
    }
}

pub fn random_id(conn: &Connection) -> Result<String> {
    Ok(conn.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?)
}

fn respond(request: Request) -> Result<Response> {
    if request.version != PROTOCOL {
        return Err("incompatible sync protocol".into());
    }
    let mut conn = db::open(true)?;
    super::sync::init(&conn)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if super::sync::is_client(&tx)? {
        return Err("this database is already configured as a sync client".into());
    }
    tx.execute_batch("CREATE TABLE IF NOT EXISTS sync_master(id TEXT NOT NULL);
                      CREATE TABLE IF NOT EXISTS sync_receipts(id TEXT PRIMARY KEY);
                      INSERT INTO sync_master SELECT lower(hex(randomblob(16))) WHERE NOT EXISTS (SELECT 1 FROM sync_master);")?;
    let identity: String = tx.query_row("SELECT id FROM sync_master", [], |r| r.get(0))?;
    let response = match request.action {
        Action::Fetch => Response::Fetched {
            master: identity,
            data: snapshot::read(&tx)?,
        },
        Action::Apply {
            master,
            id,
            expected,
            desired,
        } => {
            if identity != master {
                return Err("master database identity changed".into());
            }
            if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("invalid sync operation ID".into());
            }
            let seen = tx
                .query_row("SELECT 1 FROM sync_receipts WHERE id=?", [&id], |_| Ok(()))
                .optional()?
                .is_some();
            if seen {
                Response::Applied
            } else {
                let current = snapshot::read(&tx)?;
                if current != expected {
                    Response::Stale
                } else {
                    snapshot::apply(&tx, &current, &desired)?;
                    tx.execute("INSERT INTO sync_receipts(id) VALUES (?)", params![id])?;
                    Response::Applied
                }
            }
        }
    };
    tx.commit()?;
    Ok(response)
}

pub fn serve() -> Result<()> {
    let request = limited(io::stdin().lock())
        .map_err(|e| e.into())
        .and_then(|raw| Ok(serde_json::from_slice(&raw)?));
    let response = request
        .and_then(respond)
        .unwrap_or_else(|e| Response::Error {
            message: e.to_string(),
        });
    let mut data = serde_json::to_vec(&Reply {
        version: PROTOCOL,
        response,
    })?;
    if data.len() as u64 > MAX {
        data = serde_json::to_vec(&Reply {
            version: PROTOCOL,
            response: Response::Error {
                message: "master snapshot exceeds the 32 MiB protocol limit".into(),
            },
        })?;
    }
    io::stdout().lock().write_all(&data)?;
    Ok(())
}
