use crate::database::Result;

pub const DEFAULT_INTERVAL_SECS: u64 = 60;

#[cfg(target_os = "macos")]
mod macos {
    use std::{
        env, fs,
        path::PathBuf,
        process::{Command, Stdio},
    };

    use super::Result;

    const LABEL: &str = "com.olofmoriya.todo-tui.sync";

    fn location() -> Result<Option<(String, PathBuf)>> {
        // Isolated/test databases must never install or control the login agent.
        if env::var_os("TODO_TUI_DB").is_some() {
            return Ok(None);
        }
        let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
        let output = Command::new("/usr/bin/id").arg("-u").output()?;
        if !output.status.success() {
            return Err("could not identify the macOS login user".into());
        }
        let uid: u32 = String::from_utf8(output.stdout)?.trim().parse()?;
        Ok(Some((
            format!("gui/{uid}"),
            home.join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
        )))
    }

    fn loaded(target: &str) -> bool {
        Command::new("/bin/launchctl")
            .args(["print", target])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn launch(args: &[&str]) -> Result<()> {
        let output = Command::new("/bin/launchctl").args(args).output()?;
        if !output.status.success() {
            return Err(format!(
                "launchctl {}: {}",
                args[0],
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
        Ok(())
    }

    fn executable() -> Result<PathBuf> {
        let current = env::current_exe()?;
        let real = current.canonicalize()?;
        // The Homebrew symlink, unlike a versioned Cellar path, survives upgrades.
        for name in ["/opt/homebrew/bin/todo-tui", "/usr/local/bin/todo-tui"] {
            let path = PathBuf::from(name);
            if path.canonicalize().is_ok_and(|p| p == real) {
                return Ok(path);
            }
        }
        Ok(current)
    }

    fn xml(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    pub fn enable(interval_secs: u64) -> Result<bool> {
        let Some((domain, path)) = location()? else {
            return Ok(false);
        };
        let target = format!("{domain}/{LABEL}");
        let executable = executable()?;
        let binary = xml(executable
            .to_str()
            .ok_or("invalid executable path for launchd")?);
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>{binary}</string><string>sync</string><string>--automatic</string></array>
<key>RunAtLoad</key><true/>
<key>StartInterval</key><integer>{interval_secs}</integer>
</dict></plist>
"#
        );
        if fs::read_to_string(&path).is_ok_and(|old| old == plist) && loaded(&target) {
            return Ok(true);
        }
        let parent = path.parent().ok_or("missing LaunchAgents directory")?;
        fs::create_dir_all(parent)?;
        let temp = tempfile::NamedTempFile::new_in(parent)?;
        fs::write(temp.path(), plist)?;
        if loaded(&target) {
            launch(&["bootout", &target])?;
        }
        temp.persist(&path)?;
        if !loaded(&domain) {
            return Ok(false);
        } // launchd will load it at the next GUI login.
        launch(&[
            "bootstrap",
            &domain,
            path.to_str().ok_or("invalid LaunchAgent path")?,
        ])?;
        Ok(true)
    }

    pub fn disable() -> Result<()> {
        let Some((domain, path)) = location()? else {
            return Ok(());
        };
        let target = format!("{domain}/{LABEL}");
        if loaded(&target) {
            launch(&["bootout", &target])?;
        }
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn active() -> bool {
        location()
            .ok()
            .flatten()
            .is_some_and(|(domain, path)| path.is_file() && loaded(&format!("{domain}/{LABEL}")))
    }
}

#[cfg(target_os = "macos")]
pub use macos::{active, disable, enable};

#[cfg(not(target_os = "macos"))]
pub fn enable(_interval_secs: u64) -> Result<bool> {
    Ok(false)
}
#[cfg(not(target_os = "macos"))]
pub fn disable() -> Result<()> {
    Ok(())
}
#[cfg(not(target_os = "macos"))]
pub fn active() -> bool {
    false
}
