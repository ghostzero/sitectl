use anyhow::Result;
use colored::Colorize;
use std::os::unix::fs::PermissionsExt;
use walkdir::WalkDir;

use crate::system::{owner_name, perms_octal, WWW_ROOT};

const WEBSHELL_NAMES: &[&str] = &[
    "shell", "cmd", "c99", "r57", "alfa", "wso", "b374k", "bypass",
    "exploit", "backdoor", "webshell", "upload", "hack",
];

const WEBSHELL_PATTERNS: &[&str] = &[
    "eval(base64_decode",
    "eval(gzinflate",
    "eval(str_rot13",
    "assert($_POST",
    "assert($_GET",
    "assert($_REQUEST",
    "system($_GET",
    "system($_POST",
    "system($_REQUEST",
    "shell_exec(",
    "passthru(",
    "preg_replace.*\\/e",
    "eval($_REQUEST",
    "eval($_POST",
    "eval($_GET",
];

#[derive(Default)]
struct ProjectReport {
    domain: String,
    issues: Vec<(Severity, String)>,
}

#[derive(Clone, Copy)]
enum Severity {
    Critical,
    Warning,
}

impl ProjectReport {
    fn new(domain: &str) -> Self {
        Self {
            domain: domain.to_string(),
            issues: Vec::new(),
        }
    }

    fn add(&mut self, sev: Severity, msg: String) {
        self.issues.push((sev, msg));
    }

    fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }

    fn print(&self) {
        let status = if self.issues.iter().any(|(s, _)| matches!(s, Severity::Critical)) {
            "COMPROMISED".red().bold()
        } else if !self.issues.is_empty() {
            "SUSPICIOUS".yellow().bold()
        } else {
            "CLEAN".green().bold()
        };

        println!("\n{} [{}]", self.domain.bold(), status);

        for (sev, msg) in &self.issues {
            let prefix = match sev {
                Severity::Critical => "  [CRITICAL]".red(),
                Severity::Warning => "  [WARN]    ".yellow(),
            };
            println!("{} {}", prefix, msg);
        }

        if self.is_clean() {
            println!("  {}", "no issues found".dimmed());
        }
    }
}

fn audit_project(domain: &str) -> ProjectReport {
    let mut report = ProjectReport::new(domain);
    let dir = format!("{WWW_ROOT}/{domain}");
    let dir_path = std::path::Path::new(&dir);

    if !dir_path.exists() {
        report.add(Severity::Warning, format!("directory {dir} does not exist"));
        return report;
    }

    // 1. Directory permissions
    let perms = perms_octal(dir_path);
    let owner = owner_name(dir_path);
    if perms.ends_with("7") || perms.contains("77") {
        report.add(
            Severity::Warning,
            format!("directory permissions {perms} (owned by {owner}) — recommend 750"),
        );
    }

    // 2. .env files
    for entry in WalkDir::new(&dir).max_depth(4).into_iter().flatten() {
        let fname = entry.file_name().to_string_lossy();
        if fname == ".env" || fname.ends_with(".env") {
            let p = entry.path();
            if let Ok(meta) = std::fs::metadata(p) {
                let mode = meta.permissions().mode() & 0o777;
                if mode & 0o044 != 0 {
                    report.add(
                        Severity::Warning,
                        format!("{} is {:o} (group/world readable)", p.display(), mode),
                    );
                }
            }
        }
    }

    // 3. World-writable files
    for entry in WalkDir::new(&dir).into_iter().flatten() {
        let p = entry.path();
        if let Ok(meta) = std::fs::metadata(p) {
            let mode = meta.permissions().mode();
            if mode & 0o002 != 0 && p.is_file() {
                // Skip log files
                let path_str = p.to_string_lossy();
                if !path_str.contains("/storage/logs/") && !path_str.contains("/.git/") {
                    report.add(
                        Severity::Warning,
                        format!("world-writable file: {}", p.display()),
                    );
                }
            }
        }
    }

    // 4. Suspicious file names
    for entry in WalkDir::new(&dir).into_iter().flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("php") {
            continue;
        }
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if WEBSHELL_NAMES.iter().any(|&w| stem.contains(w)) {
            report.add(
                Severity::Critical,
                format!("suspicious filename: {}", p.display()),
            );
        }
    }

    // 5. Webshell code patterns in PHP files
    for entry in WalkDir::new(&dir).into_iter().flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("php") {
            continue;
        }
        // Skip vendor directories
        if p.to_string_lossy().contains("/vendor/") {
            continue;
        }
        let content = match std::fs::read_to_string(p) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for pattern in WEBSHELL_PATTERNS {
            if content.contains(pattern) {
                report.add(
                    Severity::Critical,
                    format!("webshell pattern '{}' in {}", pattern, p.display()),
                );
                break; // one finding per file is enough
            }
        }
        // Check for large base64 blobs (obfuscated payloads)
        if content.contains("base64_decode") && content.len() > 50_000 {
            report.add(
                Severity::Critical,
                format!(
                    "large base64-heavy file ({} bytes): {}",
                    content.len(),
                    p.display()
                ),
            );
        }
    }

    // 6. Unexpected .htaccess files (outside public/)
    for entry in WalkDir::new(&dir).into_iter().flatten() {
        let p = entry.path();
        if p.file_name().and_then(|n| n.to_str()) != Some(".htaccess") {
            continue;
        }
        let rel = p.strip_prefix(&dir).unwrap_or(p).to_string_lossy();
        // Flag .htaccess files outside of the web root / top-level
        if rel.contains('/') && !rel.starts_with("public/") && !rel.starts_with("current/public/")
        {
            report.add(
                Severity::Warning,
                format!(".htaccess outside web root: {}", p.display()),
            );
        }
    }

    report
}

pub fn cmd_run(domain: Option<&str>) -> Result<()> {
    let domains: Vec<String> = match domain {
        Some(d) => vec![d.to_string()],
        None => {
            let mut list = Vec::new();
            for entry in std::fs::read_dir(WWW_ROOT)?.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        list.push(name.to_string());
                    }
                }
            }
            list.sort();
            list
        }
    };

    println!("{}", "Security Audit".bold().underline());
    println!("Scanning {} project(s)...\n", domains.len());

    let mut compromised = Vec::new();
    let mut suspicious = Vec::new();
    let mut clean = 0;

    for d in &domains {
        let report = audit_project(d);
        let has_critical = report.issues.iter().any(|(s, _)| matches!(s, Severity::Critical));
        let has_issues = !report.is_clean();

        report.print();

        if has_critical {
            compromised.push(d.clone());
        } else if has_issues {
            suspicious.push(d.clone());
        } else {
            clean += 1;
        }
    }

    println!("\n{}", "Summary".bold().underline());
    println!(
        "  {}  {}",
        "CLEAN:".green(),
        clean
    );
    if !suspicious.is_empty() {
        println!("  {}  {}: {}", "SUSPICIOUS:".yellow(), suspicious.len(), suspicious.join(", "));
    }
    if !compromised.is_empty() {
        println!(
            "  {}  {}: {}",
            "COMPROMISED:".red().bold(),
            compromised.len(),
            compromised.join(", ")
        );
    }

    Ok(())
}
