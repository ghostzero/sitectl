use anyhow::Result;
use colored::Colorize;
use std::os::unix::fs::PermissionsExt;
use walkdir::WalkDir;

use crate::system::WWW_ROOT;

struct EnvFile {
    path: std::path::PathBuf,
    perms: u32,
    owner: String,
}

fn find_env_files(domain: Option<&str>) -> Vec<EnvFile> {
    let root = match domain {
        Some(d) => format!("{WWW_ROOT}/{d}"),
        None => WWW_ROOT.to_string(),
    };

    WalkDir::new(&root)
        .max_depth(5)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            name == ".env" || name.ends_with(".env")
        })
        .filter_map(|e| {
            let path = e.path().to_path_buf();
            let meta = std::fs::metadata(&path).ok()?;
            let perms = meta.permissions().mode() & 0o777;
            let owner = crate::system::owner_name(&path);
            Some(EnvFile { path, perms, owner })
        })
        .collect()
}

pub fn cmd_list() -> Result<()> {
    let files = find_env_files(None);

    if files.is_empty() {
        println!("no .env files found");
        return Ok(());
    }

    println!(
        "{:<55} {:<8} {:<12} {}",
        "Path".bold(),
        "Perms".bold(),
        "Owner".bold(),
        "Status".bold()
    );
    println!("{}", "-".repeat(90));

    for f in &files {
        let perms_str = format!("{:o}", f.perms);
        let status = if f.perms & 0o044 != 0 {
            "world/group readable".red().to_string()
        } else {
            "ok".green().to_string()
        };
        let perms_colored = if f.perms & 0o044 != 0 {
            perms_str.red().to_string()
        } else {
            perms_str.green().to_string()
        };

        println!(
            "{:<55} {:<8} {:<12} {}",
            f.path.display(),
            perms_colored,
            f.owner,
            status
        );
    }

    let exposed = files.iter().filter(|f| f.perms & 0o044 != 0).count();
    if exposed > 0 {
        println!(
            "\n{} {exposed} file(s) are group/world readable — run `sitectl env secure` to fix",
            "warn:".yellow()
        );
    }

    Ok(())
}

pub fn cmd_secure(domain: Option<&str>) -> Result<()> {
    let files = find_env_files(domain);

    if files.is_empty() {
        println!("no .env files found");
        return Ok(());
    }

    let mut fixed = 0;
    let mut already_ok = 0;

    for f in &files {
        if f.perms & 0o044 != 0 {
            std::fs::set_permissions(&f.path, std::fs::Permissions::from_mode(0o600))?;
            println!("{} {} (was {:o})", "secured:".green(), f.path.display(), f.perms);
            fixed += 1;
        } else {
            already_ok += 1;
        }
    }

    println!("\n{fixed} file(s) secured, {already_ok} already ok");
    Ok(())
}
