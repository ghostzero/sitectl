use anyhow::Result;
use colored::Colorize;
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::system::{run, WWW_ROOT};

const BACKUP_ROOT: &str = "/var/backups/sitectl";

fn parse_dotenv(path: &str) -> HashMap<String, String> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut map = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let val = val.trim();
            let val = if val.len() >= 2
                && ((val.starts_with('"') && val.ends_with('"'))
                    || (val.starts_with('\'') && val.ends_with('\'')))
            {
                val[1..val.len() - 1].to_string()
            } else {
                val.to_string()
            };
            map.insert(key.trim().to_string(), val);
        }
    }
    map
}

fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn path_size_bytes(path: &str) -> u64 {
    run("du", &["-sb", path])
        .ok()
        .and_then(|out| out.split_whitespace().next().and_then(|n| n.parse().ok()))
        .unwrap_or(0)
}

pub fn cmd_backup(domain: &str) -> Result<()> {
    let project_dir = format!("{WWW_ROOT}/{domain}");
    if !Path::new(&project_dir).exists() {
        anyhow::bail!("project not found: {project_dir}");
    }

    let env_path = format!("{project_dir}/.env");
    let env = if Path::new(&env_path).exists() {
        parse_dotenv(&env_path)
    } else {
        println!("{} no .env found — skipping database backup", "warn:".yellow());
        HashMap::new()
    };

    let db_connection = env.get("DB_CONNECTION").map(String::as_str).unwrap_or("none");

    let ts = run("date", &["+%Y%m%d-%H%M%S"])?;
    let backup_name = format!("{domain}-{ts}");
    let backup_dir = format!("{BACKUP_ROOT}/{domain}");
    std::fs::create_dir_all(&backup_dir)?;
    // Only root should read backup archives
    std::fs::set_permissions(&backup_dir, std::fs::Permissions::from_mode(0o700))?;

    let stage_dir = format!("/tmp/sitectl-backup-{ts}");
    let stage_inner = format!("{stage_dir}/{backup_name}");
    std::fs::create_dir_all(&stage_inner)?;

    println!("{} {}", "Backing up:".bold(), domain);
    println!();

    // --- Database ---
    let mut db_size: u64 = 0;
    let mut db_label = String::new();

    match db_connection {
        "mysql" | "mariadb" => {
            let host = env.get("DB_HOST").map(String::as_str).unwrap_or("127.0.0.1");
            let port = env.get("DB_PORT").map(String::as_str).unwrap_or("3306");
            let database = env.get("DB_DATABASE").map(String::as_str).unwrap_or("");
            let username = env.get("DB_USERNAME").map(String::as_str).unwrap_or("root");
            let password = env.get("DB_PASSWORD").map(String::as_str).unwrap_or("");

            if database.is_empty() {
                println!("  {} DB_DATABASE not set", "warn:".yellow());
            } else {
                print!("  {} MySQL '{database}'... ", "database:".cyan());
                std::io::stdout().flush().ok();

                let creds_path = format!("{stage_dir}/.my.cnf");
                std::fs::write(
                    &creds_path,
                    format!("[mysqldump]\nhost={host}\nport={port}\nuser={username}\npassword={password}\n"),
                )?;
                std::fs::set_permissions(&creds_path, std::fs::Permissions::from_mode(0o600))?;

                let dump_path = format!("{stage_inner}/db.sql");
                let result = std::process::Command::new("mysqldump")
                    .args([
                        format!("--defaults-extra-file={creds_path}").as_str(),
                        "--single-transaction",
                        "--quick",
                        database,
                    ])
                    .stdout(std::fs::File::create(&dump_path)?)
                    .stderr(std::process::Stdio::null())
                    .status();

                let _ = std::fs::remove_file(&creds_path);

                match result {
                    Ok(s) if s.success() => {
                        db_size = path_size_bytes(&dump_path);
                        db_label = format!("mysql:{database}");
                        println!("{}", human_size(db_size).green());
                    }
                    _ => {
                        let _ = std::fs::remove_file(&dump_path);
                        println!("{}", "failed".red());
                        println!("  {} mysqldump failed — check credentials in .env", "warn:".yellow());
                    }
                }
            }
        }

        "sqlite" => {
            let db_database = env.get("DB_DATABASE").map(String::as_str).unwrap_or("");
            let db_path = if db_database.starts_with('/') {
                db_database.to_string()
            } else {
                format!("{project_dir}/{db_database}")
            };

            print!("  {} SQLite... ", "database:".cyan());
            std::io::stdout().flush().ok();

            if Path::new(&db_path).exists() {
                let dest = format!("{stage_inner}/db.sqlite");
                std::fs::copy(&db_path, &dest)?;
                db_size = path_size_bytes(&dest);
                db_label = format!("sqlite:{db_database}");
                println!("{}", human_size(db_size).green());
            } else {
                println!("{} not found at {db_path}", "warn:".yellow(), );
            }
        }

        "none" => {}

        other => {
            println!("  {} DB_CONNECTION={other} not supported — skipping", "skip:".dimmed());
        }
    }

    // --- Files ---
    print!("  {} project files... ", "files:".cyan());
    std::io::stdout().flush().ok();

    let files_archive = format!("{stage_inner}/files.tar.gz");
    let excludes = [
        format!("--exclude={domain}/vendor"),
        format!("--exclude={domain}/node_modules"),
        format!("--exclude={domain}/.git"),
        format!("--exclude={domain}/storage/logs"),
    ];
    let mut tar_args: Vec<&str> = vec!["-czf", &files_archive, "-C", WWW_ROOT];
    for ex in &excludes {
        tar_args.push(ex.as_str());
    }
    tar_args.push(domain);

    let files_size = match std::process::Command::new("tar").args(&tar_args).status() {
        Ok(s) if s.success() => {
            let sz = path_size_bytes(&files_archive);
            println!("{}", human_size(sz).green());
            sz
        }
        _ => {
            println!("{}", "failed".red());
            let _ = std::fs::remove_dir_all(&stage_dir);
            anyhow::bail!("tar failed archiving project files");
        }
    };

    // --- Bundle into final archive ---
    let final_archive = format!("{backup_dir}/{backup_name}.tar.gz");
    run("tar", &["-czf", &final_archive, "-C", &stage_dir, &backup_name])?;
    std::fs::set_permissions(&final_archive, std::fs::Permissions::from_mode(0o600))?;
    let _ = std::fs::remove_dir_all(&stage_dir);

    let archive_size = path_size_bytes(&final_archive);

    // --- Summary ---
    println!();
    println!("{}", "Summary".bold().underline());
    println!("  archive:  {}", final_archive.cyan());
    println!("  total:    {}", human_size(archive_size).bold());
    println!("  files:    {} (excl. vendor, node_modules, .git, logs)", human_size(files_size));
    if !db_label.is_empty() {
        println!("  database: {} ({})", human_size(db_size), db_label);
    }
    // Format 20260626-102147 -> 2026-06-26 10:21:47
    let display_ts = if ts.len() == 15 {
        format!(
            "{}-{}-{} {}:{}:{}",
            &ts[0..4], &ts[4..6], &ts[6..8],
            &ts[9..11], &ts[11..13], &ts[13..15]
        )
    } else {
        ts.clone()
    };
    println!("  created:  {display_ts}");

    Ok(())
}
