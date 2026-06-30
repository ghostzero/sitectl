use anyhow::{Context, Result};
use std::process::Command;

use crate::system::{domain_to_user, WWW_ROOT};

/// Detect the project domain from the current working directory.
/// Requires cwd to be /var/www/<domain> or a subdirectory of it.
fn detect_domain() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let www = std::path::Path::new(WWW_ROOT);
    let rel = cwd.strip_prefix(www).ok()?;
    rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned())
}

pub fn cmd_exec(domain: Option<&str>, args: &[String]) -> Result<()> {
    if args.is_empty() {
        anyhow::bail!("no command specified");
    }

    let domain = match domain {
        Some(d) => d.to_string(),
        None => detect_domain()
            .ok_or_else(|| anyhow::anyhow!(
                "could not detect project from current directory — use -d <domain>"
            ))?,
    };

    let user = domain_to_user(&domain);
    let dir = format!("{WWW_ROOT}/{domain}");

    if !std::path::Path::new(&dir).exists() {
        anyhow::bail!("project directory {dir} does not exist");
    }

    let mut cmd = Command::new("sudo");
    cmd.args(["-H", "-u", &user, "--"]);

    // Forward the SSH agent socket so git+yubikey works inside the exec'd command.
    // Use setfacl to grant access only to this project user — avoids exposing
    // the socket to every user on the system.
    if let Ok(sock) = std::env::var("SSH_AUTH_SOCK") {
        let sock_path = std::path::Path::new(&sock);
        if sock_path.exists() {
            let acl = format!("u:{user}:x");
            if let Some(parent) = sock_path.parent() {
                Command::new("setfacl").args(["-m", &acl, parent.to_str().unwrap_or("")]).status().ok();
            }
            let acl = format!("u:{user}:rw");
            Command::new("setfacl").args(["-m", &acl, &sock]).status().ok();
        }
        cmd.args(["env", &format!("SSH_AUTH_SOCK={sock}")]);
    }

    let status = cmd
        .args(args)
        .current_dir(&dir)
        .status()
        .context("failed to execute sudo")?;

    if !status.success() {
        anyhow::bail!("command exited with {status}");
    }
    Ok(())
}
