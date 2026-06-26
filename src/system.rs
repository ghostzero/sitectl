use anyhow::{Context, Result};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

pub const WWW_ROOT: &str = "/var/www";

/// Convert a domain name to a valid Linux username.
/// e.g. "events.anikeen.com" -> "events_anikeen_com"
pub fn domain_to_user(domain: &str) -> String {
    let slug: String = domain
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    // Linux usernames max 32 chars
    slug.chars().take(32).collect()
}

/// Run a command, return trimmed stdout on success.
pub fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("failed to execute {cmd}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!("{cmd} failed: {stderr}")
    }
}

/// Run a command, print stdout/stderr live, return success/failure.
pub fn run_status(cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .with_context(|| format!("failed to execute {cmd}"))?;

    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("{cmd} exited with status {status}")
    }
}

pub fn user_exists(user: &str) -> bool {
    Command::new("id")
        .arg(user)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn available_php_versions() -> Vec<String> {
    let mut versions = Vec::new();
    for entry in std::fs::read_dir("/etc/php").into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(v) = path.file_name().and_then(|n| n.to_str()) {
                if v.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    versions.push(v.to_string());
                }
            }
        }
    }
    versions.sort();
    versions
}

pub fn fpm_pool_path(domain: &str, php: &str) -> std::path::PathBuf {
    format!("/etc/php/{php}/fpm/pool.d/{domain}.conf").into()
}

pub fn fpm_socket_path(domain: &str, php: &str) -> String {
    let user = domain_to_user(domain);
    format!("/run/php/php{php}-fpm-{user}.sock")
}

/// Octal permission bits of a path as a string, e.g. "750"
pub fn perms_octal(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(m) => format!("{:o}", m.mode() & 0o777),
        Err(_) => "???".to_string(),
    }
}

/// Owner username of a path
pub fn owner_name(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(m) => {
            let uid = m.uid();
            unsafe {
                let pw = libc::getpwuid(uid);
                if pw.is_null() {
                    return uid.to_string();
                }
                std::ffi::CStr::from_ptr((*pw).pw_name)
                    .to_string_lossy()
                    .to_string()
            }
        }
        Err(_) => "?".to_string(),
    }
}

pub fn reload_fpm(php: &str) -> Result<()> {
    run_status("systemctl", &["reload", &format!("php{php}-fpm")])
}

pub fn reload_nginx() -> Result<()> {
    run("nginx", &["-t"])?;
    run_status("systemctl", &["reload", "nginx"])
}
