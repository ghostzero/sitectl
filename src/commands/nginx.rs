use anyhow::Result;
use colored::Colorize;

use crate::system::{fpm_socket_path, reload_nginx};

const AVAILABLE: &str = "/etc/nginx/sites-available";
const ENABLED: &str = "/etc/nginx/sites-enabled";

fn laravel_vhost(domain: &str, php: &str, web_root: &str) -> String {
    create_vhost_config(&[domain], domain, php, web_root)
}

fn create_vhost_config(server_names: &[&str], primary: &str, php: &str, web_root: &str) -> String {
    let socket = fpm_socket_path(primary, php);
    let names = server_names.join(" ");
    format!(
        "server {{
    server_name {names};
    root /var/www/{primary}/{web_root};

    add_header X-Content-Type-Options \"nosniff\";
    add_header X-Frame-Options \"SAMEORIGIN\";

    index index.php;
    charset utf-8;

    location / {{
        try_files $uri $uri/ /index.php?$query_string;
    }}

    location = /favicon.ico {{ access_log off; log_not_found off; }}
    location = /robots.txt  {{ access_log off; log_not_found off; }}

    error_page 404 /index.php;

    location ~ \\.php$ {{
        fastcgi_pass unix:{socket};
        fastcgi_param SCRIPT_FILENAME $realpath_root$fastcgi_script_name;
        include fastcgi_params;
    }}

    location ~ /\\.(?!well-known).* {{
        deny all;
    }}

    listen 80;
}}
"
    )
}

/// Create a vhost config for multiple domains (first is primary/directory name).
pub fn create_vhost_multi(domains: &[&str], php: &str, web_root: &str) -> Result<()> {
    let primary = domains.first().ok_or_else(|| anyhow::anyhow!("no domains provided"))?;
    let available = format!("{AVAILABLE}/{primary}");

    if std::path::Path::new(&available).exists() {
        anyhow::bail!("vhost already exists at {available}");
    }

    let config = create_vhost_config(domains, primary, php, web_root);
    std::fs::write(&available, config)?;
    println!("{} {} (domains: {})", "created:".green(), available, domains.join(", "));
    Ok(())
}

pub fn cmd_add(domain: &str, php: &str, web_root: &str) -> Result<()> {
    let available = format!("{AVAILABLE}/{domain}");

    if std::path::Path::new(&available).exists() {
        anyhow::bail!("vhost already exists at {available}");
    }

    // Detect static site if web_root is empty or no PHP needed
    let config = laravel_vhost(domain, php, web_root);
    std::fs::write(&available, config)?;
    println!("{} {}", "created:".green(), available);

    cmd_enable(domain)?;
    Ok(())
}

pub fn cmd_remove(domain: &str) -> Result<()> {
    let available = format!("{AVAILABLE}/{domain}");
    let enabled = format!("{ENABLED}/{domain}");

    if std::path::Path::new(&enabled).exists() {
        std::fs::remove_file(&enabled)?;
        println!("{} {}", "unlinked:".yellow(), enabled);
    }

    if std::path::Path::new(&available).exists() {
        std::fs::remove_file(&available)?;
        println!("{} {}", "removed:".green(), available);
    } else {
        anyhow::bail!("vhost not found at {available}");
    }

    reload_nginx()?;
    println!("{} nginx", "reloaded:".green());
    Ok(())
}

pub fn cmd_enable(domain: &str) -> Result<()> {
    let available = format!("{AVAILABLE}/{domain}");
    let enabled = format!("{ENABLED}/{domain}");

    if !std::path::Path::new(&available).exists() {
        anyhow::bail!("vhost not found at {available}");
    }

    if std::path::Path::new(&enabled).exists() {
        println!("{} already enabled", "warn:".yellow());
        return Ok(());
    }

    std::os::unix::fs::symlink(&available, &enabled)?;
    println!("{} {}", "enabled:".green(), enabled);

    reload_nginx()?;
    println!("{} nginx", "reloaded:".green());
    Ok(())
}

/// Detect the PHP version in use from an existing nginx vhost config.
pub fn detect_php_version(domain: &str) -> Option<String> {
    let path = format!("{AVAILABLE}/{domain}");
    let content = std::fs::read_to_string(&path).ok()?;
    // Match: fastcgi_pass unix:/run/php/php8.3-fpm...
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("fastcgi_pass") {
            // Extract version number like "8.3" from "php8.3-fpm"
            if let Some(start) = line.find("php") {
                let rest = &line[start + 3..];
                let version: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                if !version.is_empty() {
                    return Some(version);
                }
            }
        }
    }
    None
}

/// Replace the fastcgi_pass socket in an existing nginx vhost with the per-site socket.
pub fn cmd_set_socket(domain: &str, php: &str, socket_override: Option<&str>) -> Result<()> {
    let path = format!("{AVAILABLE}/{domain}");

    if !std::path::Path::new(&path).exists() {
        anyhow::bail!("vhost not found at {path}");
    }

    let content = std::fs::read_to_string(&path)?;
    let new_socket = match socket_override {
        Some(s) => s.to_string(),
        None => crate::system::fpm_socket_path(domain, php),
    };

    // Replace any existing fastcgi_pass unix:... line
    let updated: String = content
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with("fastcgi_pass") && trimmed.contains("unix:") {
                let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                format!("{indent}fastcgi_pass unix:{new_socket};")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    if updated == content {
        println!("{} no fastcgi_pass line found in {path}", "warn:".yellow());
        return Ok(());
    }

    // Write a backup first
    std::fs::write(format!("{path}.bak"), &content)?;
    std::fs::write(&path, updated)?;
    println!("{} {} -> {}", "updated:".green(), path, new_socket);

    reload_nginx()?;
    println!("{} nginx", "reloaded:".green());
    Ok(())
}

pub fn cmd_audit() -> Result<()> {
    use colored::Colorize;
    use std::collections::HashSet;

    // Parse all vhosts: extract config name, server_names, root paths, php socket
    struct Vhost {
        config: String,
        enabled: bool,
        server_names: Vec<String>,
        roots: Vec<String>,
        php_socket: Option<String>,
    }

    let mut vhosts: Vec<Vhost> = Vec::new();

    for entry in std::fs::read_dir(AVAILABLE)?.flatten() {
        let path = entry.path();
        let config = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let enabled = std::path::Path::new(&format!("{ENABLED}/{config}")).exists();
        let content = std::fs::read_to_string(&path).unwrap_or_default();

        let server_names: Vec<String> = content
            .lines()
            .filter(|l| l.trim().starts_with("server_name"))
            .flat_map(|l| {
                l.trim()
                    .trim_start_matches("server_name")
                    .trim_end_matches(';')
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|s| s != "_")
            .collect();

        let roots: Vec<String> = content
            .lines()
            .filter(|l| l.trim().starts_with("root "))
            .map(|l| {
                l.trim()
                    .trim_start_matches("root ")
                    .trim_end_matches(';')
                    .trim()
                    .to_string()
            })
            .collect();

        let php_socket = content.lines().find_map(|l| {
            let t = l.trim();
            if t.starts_with("fastcgi_pass") && t.contains("unix:") {
                Some(t.trim_start_matches("fastcgi_pass").trim().trim_end_matches(';').trim_start_matches("unix:").to_string())
            } else {
                None
            }
        });

        vhosts.push(Vhost { config, enabled, server_names, roots, php_socket });
    }

    // Collect all /var/www project directories
    let www_dirs: HashSet<String> = std::fs::read_dir(crate::system::WWW_ROOT)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| !n.starts_with('.'))
        .collect();

    println!("{}", "Nginx vhost audit".bold().underline());
    println!();

    // Check each vhost
    let shared_sock_pattern = |s: &str| {
        // matches php8.x-fpm.sock but NOT php8.x-fpm-something.sock
        s.contains("-fpm.sock") && !s.contains("-fpm-")
    };

    for v in &vhosts {
        let enabled_label = if v.enabled {
            "enabled".green()
        } else {
            "disabled".dimmed()
        };

        println!("{} [{}]", v.config.bold(), enabled_label);

        // server_names
        if v.server_names.is_empty() {
            println!("  {} no server_name found", "warn:".yellow());
        } else {
            println!("  server_name: {}", v.server_names.join(", "));
        }

        // root paths and whether they exist
        for root in &v.roots {
            let root_path = std::path::Path::new(root);
            // Check whether root is under /var/www and if that project dir exists
            let project_dir = root
                .strip_prefix(&format!("{}/", crate::system::WWW_ROOT))
                .and_then(|rest| rest.split('/').next())
                .map(str::to_string);

            if root_path.exists() {
                println!("  root: {} {}", root, "exists".green());
            } else {
                println!("  root: {} {}", root, "MISSING".red().bold());
            }

            if let Some(ref proj) = project_dir {
                if !www_dirs.contains(proj.as_str()) {
                    println!("  {} /var/www/{proj} does not exist", "warn:".yellow());
                }
            }
        }

        // PHP socket
        if let Some(ref sock) = v.php_socket {
            if shared_sock_pattern(sock) {
                println!("  socket: {} {}", sock, "shared pool (not isolated)".yellow());
            } else {
                let sock_exists = std::path::Path::new(sock).exists();
                let sock_label = if sock_exists { "ok".green() } else { "not yet active".dimmed() };
                println!("  socket: {} [{}]", sock, sock_label);
            }
        }

        println!();
    }

    // Check which /var/www dirs have NO nginx vhost at all
    let all_server_names: HashSet<String> = vhosts
        .iter()
        .flat_map(|v| v.server_names.clone())
        .collect();

    // Also collect root-referenced project dirs
    let all_root_projects: HashSet<String> = vhosts
        .iter()
        .flat_map(|v| v.roots.iter().filter_map(|r| {
            r.strip_prefix(&format!("{}/", crate::system::WWW_ROOT))
                .and_then(|rest| rest.split('/').next())
                .map(str::to_string)
        }))
        .collect();

    let unserved: Vec<&String> = www_dirs
        .iter()
        .filter(|d| !all_server_names.contains(*d) && !all_root_projects.contains(*d))
        .collect();

    if !unserved.is_empty() {
        let mut sorted = unserved;
        sorted.sort();
        println!("{}", "Directories with no nginx vhost:".bold());
        for d in sorted {
            println!("  {} /var/www/{d}", "—".dimmed());
        }
    }

    Ok(())
}

pub fn cmd_disable(domain: &str) -> Result<()> {
    let enabled = format!("{ENABLED}/{domain}");

    if !std::path::Path::new(&enabled).exists() {
        anyhow::bail!("vhost not enabled at {enabled}");
    }

    std::fs::remove_file(&enabled)?;
    println!("{} {}", "disabled:".yellow(), enabled);

    reload_nginx()?;
    println!("{} nginx", "reloaded:".green());
    Ok(())
}
