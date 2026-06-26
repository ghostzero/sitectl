use anyhow::Result;
use colored::Colorize;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::system::{
    domain_to_user, fpm_pool_path, owner_name, perms_octal, run,
    user_exists, available_php_versions, WWW_ROOT,
};

const NGINX_AVAILABLE: &str = "/etc/nginx/sites-available";
const NGINX_ENABLED: &str = "/etc/nginx/sites-enabled";

struct ProjectStatus {
    domain: String,
    dir_perms: String,
    system_user: String,
    user_exists: bool,
    fpm_pool: Option<(String, String)>, // (php_version, pool_path)
    nginx_enabled: bool,
    env_perms: Option<String>,
}

fn discover_projects() -> Vec<ProjectStatus> {
    let mut projects = Vec::new();

    let entries = match std::fs::read_dir(WWW_ROOT) {
        Ok(e) => e,
        Err(_) => return projects,
    };

    let php_versions = available_php_versions();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let domain = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Skip hidden/internal directories
        if domain.starts_with('.') {
            continue;
        }

        let system_user = domain_to_user(&domain);
        let user_ok = user_exists(&system_user);

        let dir_perms = perms_octal(&path);

        // Find FPM pool
        let fpm_pool = php_versions.iter().find_map(|v| {
            let p = fpm_pool_path(&domain, v);
            if p.exists() {
                Some((v.clone(), p.to_string_lossy().to_string()))
            } else {
                None
            }
        });

        // Check nginx
        let nginx_enabled = Path::new(&format!("{NGINX_ENABLED}/{domain}")).exists();

        // Find .env
        let env_perms = ["", ".env", "current/.env", "current/public/.env"]
            .iter()
            .filter_map(|suffix| {
                let p = if suffix.is_empty() {
                    path.join(".env")
                } else {
                    path.join(suffix)
                };
                if p.exists() {
                    Some(perms_octal(&p))
                } else {
                    None
                }
            })
            .next();

        projects.push(ProjectStatus {
            domain,
            dir_perms,
            system_user,
            user_exists: user_ok,
            fpm_pool,
            nginx_enabled,
            env_perms,
        });
    }

    projects.sort_by(|a, b| a.domain.cmp(&b.domain));
    projects
}

pub fn cmd_list() -> Result<()> {
    let projects = discover_projects();

    println!(
        "{:<30} {:<20} {:<6} {:<12} {:<7} {:<6}",
        "Domain".bold(),
        "User".bold(),
        "Perms".bold(),
        "FPM Pool".bold(),
        "Nginx".bold(),
        ".env".bold(),
    );
    println!("{}", "-".repeat(85));

    for p in &projects {
        let user_col = if p.user_exists {
            p.system_user.green().to_string()
        } else {
            format!("{} (missing)", p.system_user).red().to_string()
        };

        let perms_col = if p.dir_perms == "750" || p.dir_perms == "700" {
            p.dir_perms.green().to_string()
        } else if p.dir_perms == "755" {
            p.dir_perms.yellow().to_string()
        } else {
            p.dir_perms.red().to_string()
        };

        let fpm_col = match &p.fpm_pool {
            Some((v, _)) => format!("php{v}").green().to_string(),
            None => "none".yellow().to_string(),
        };

        let nginx_col = if p.nginx_enabled {
            "yes".green().to_string()
        } else {
            "no".dimmed().to_string()
        };

        let env_col = match &p.env_perms {
            Some(perms) => {
                let mode = u32::from_str_radix(perms, 8).unwrap_or(0);
                if mode & 0o044 != 0 {
                    perms.red().to_string()
                } else {
                    perms.green().to_string()
                }
            }
            None => "-".dimmed().to_string(),
        };

        println!(
            "{:<30} {:<20} {:<6} {:<12} {:<7} {:<6}",
            p.domain, user_col, perms_col, fpm_col, nginx_col, env_col,
        );
    }

    Ok(())
}

pub fn cmd_new(domain: &str, php: &str, project_type: &str) -> Result<()> {
    let user = domain_to_user(domain);
    let dir = format!("{WWW_ROOT}/{domain}");

    println!("{} {domain}", "creating project:".bold());
    println!("  user:    {user}");
    println!("  dir:     {dir}");
    println!("  php:     {php}");
    println!("  type:    {project_type}");
    println!();

    // 1. Create system user
    if user_exists(&user) {
        println!("{} system user '{user}' already exists", "skip:".dimmed());
    } else {
        run(
            "useradd",
            &[
                "--system",
                "--no-create-home",
                "--shell", "/usr/sbin/nologin",
                "--home-dir", &dir,
                &user,
            ],
        )?;
        println!("{} system user '{user}'", "created:".green());
    }

    // 2. Create directory
    std::fs::create_dir_all(&dir)?;
    println!("{} {dir}", "created:".green());

    // 3. Set ownership + permissions
    run("chown", &[&format!("{user}:{user}"), &dir])?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o750))?;
    println!("{} {dir} -> {user}:{user} 750", "chown/chmod:".green());

    // 4. PHP-FPM pool
    let pool_path = fpm_pool_path(domain, php);
    if pool_path.exists() {
        println!("{} FPM pool already exists", "skip:".dimmed());
    } else {
        crate::commands::fpm::cmd_add(domain, php)?;
    }

    // 5. Nginx vhost
    let vhost_available = format!("{NGINX_AVAILABLE}/{domain}");
    if Path::new(&vhost_available).exists() {
        println!("{} nginx vhost already exists", "skip:".dimmed());
    } else {
        let web_root = if project_type == "static" { "." } else { "public" };
        crate::commands::nginx::cmd_add(domain, php, web_root)?;
    }

    println!("\n{} {domain}", "done:".green().bold());
    println!(
        "  Deploy your code to {dir}, then run: {}",
        "certbot --nginx -d <domain>".dimmed()
    );

    Ok(())
}

pub fn cmd_fix(domain: &str) -> Result<()> {
    let dir = format!("{WWW_ROOT}/{domain}");
    let dir_path = Path::new(&dir);

    if !dir_path.exists() {
        anyhow::bail!("directory {dir} does not exist");
    }

    let user = domain_to_user(domain);
    let mut fixed = 0;

    // Create system user if missing
    if !user_exists(&user) {
        run(
            "useradd",
            &[
                "--system",
                "--no-create-home",
                "--shell", "/usr/sbin/nologin",
                "--home-dir", &dir,
                &user,
            ],
        )?;
        println!("{} system user '{user}'", "created:".green());
        fixed += 1;
    }

    // Recursively chown the project directory if not already owned by the site user
    let current_owner = owner_name(dir_path);
    if current_owner != user {
        run("chown", &["-R", &format!("{user}:{user}"), &dir])?;
        println!("{} {dir} (recursive) -> {user}:{user}", "chown:".green());
        fixed += 1;
    }

    // Ensure top-level directory is 750 (group-traversable for nginx)
    let perms = perms_octal(dir_path);
    if perms != "750" {
        std::fs::set_permissions(dir_path, std::fs::Permissions::from_mode(0o750))?;
        println!("{} {dir} -> 750 (was {perms})", "chmod:".green());
        fixed += 1;
    }

    // Add group read+traverse on all files/dirs so nginx (www-data) can serve them
    run("chmod", &["-R", "g+rX", &dir])?;
    println!("{} {dir} (recursive) g+rX", "chmod:".green());
    fixed += 1;

    // Add www-data to the site's group so nginx can traverse the directory
    let groups_out = run("id", &["-Gn", "www-data"]).unwrap_or_default();
    if !groups_out.split_whitespace().any(|g| g == user) {
        run("usermod", &["-aG", &user, "www-data"])?;
        println!("{} www-data -> group '{user}'", "usermod:".green());
        fixed += 1;
    }

    // Fix .env files
    let env_candidates = [
        format!("{dir}/.env"),
        format!("{dir}/current/.env"),
    ];
    for env_path in &env_candidates {
        let ep = Path::new(env_path);
        if !ep.exists() {
            continue;
        }
        let mode = std::fs::metadata(ep)?.permissions().mode() & 0o777;
        if mode & 0o044 != 0 {
            std::fs::set_permissions(ep, std::fs::Permissions::from_mode(0o600))?;
            println!("{} {env_path} -> 600 (was {:o})", "chmod:".green(), mode);
            fixed += 1;
        }
    }

    if fixed == 0 {
        println!("{} {domain} — nothing to fix", "ok:".green());
    } else {
        println!("\n{} {fixed} issue(s) fixed for {domain}", "done:".green().bold());
    }

    Ok(())
}
