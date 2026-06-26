use anyhow::Result;
use colored::Colorize;

use crate::system::{available_php_versions, domain_to_user, fpm_pool_path, reload_nginx, run, run_status, user_exists, WWW_ROOT};

pub fn cmd_rm(domain: &str, yes: bool) -> Result<()> {
    let user = domain_to_user(domain);
    let dir = format!("{WWW_ROOT}/{domain}");

    // --- Inventory what exists ---

    let dir_exists = std::path::Path::new(&dir).exists();
    let user_exists = user_exists(&user);

    let nginx_available = format!("/etc/nginx/sites-available/{domain}");
    let nginx_enabled = format!("/etc/nginx/sites-enabled/{domain}");
    let nginx_vhost_exists = std::path::Path::new(&nginx_available).exists();
    let nginx_vhost_enabled = std::path::Path::new(&nginx_enabled).exists();

    let php_versions = available_php_versions();
    let fpm_pools: Vec<(String, std::path::PathBuf)> = php_versions
        .iter()
        .filter_map(|v| {
            let p = fpm_pool_path(domain, v);
            if p.exists() { Some((v.clone(), p)) } else { None }
        })
        .collect();

    // Certbot cert-name may differ — check common locations
    let cert_names: Vec<String> = [domain.to_string(), format!("www.{domain}")]
        .iter()
        .filter(|n| std::path::Path::new(&format!("/etc/letsencrypt/live/{n}")).exists())
        .cloned()
        .collect();

    let nothing = !dir_exists && !user_exists && !nginx_vhost_exists && fpm_pools.is_empty() && cert_names.is_empty();

    // --- Print summary ---
    println!("{} {domain}", "Removing project:".bold());
    println!();

    if nothing {
        println!("  nothing found to remove");
        return Ok(());
    }

    println!("  The following will be permanently deleted:\n");

    if dir_exists {
        let size = run("du", &["-sh", &dir]).unwrap_or_else(|_| "?".into());
        println!("  {} {} ({})", "dir:".red(), dir, size.split_whitespace().next().unwrap_or("?"));
    }
    if user_exists {
        println!("  {} {user}", "user:".red());
    }
    if nginx_vhost_exists {
        println!("  {} {nginx_available}", "nginx vhost:".red());
    }
    for (v, path) in &fpm_pools {
        println!("  {} {}", format!("php{v} pool:").red(), path.display());
    }
    for cert in &cert_names {
        println!("  {} /etc/letsencrypt/live/{cert}", "certbot cert:".red());
    }

    if !yes {
        println!();
        println!("{}", "  Irreversible. Run with --yes to confirm.".yellow());
        return Ok(());
    }

    println!();

    // --- Execute ---

    // 1. Disable + remove nginx vhost
    if nginx_vhost_enabled {
        std::fs::remove_file(&nginx_enabled)?;
        println!("{} {nginx_enabled}", "removed:".green());
    }
    if nginx_vhost_exists {
        std::fs::remove_file(&nginx_available)?;
        println!("{} {nginx_available}", "removed:".green());
        reload_nginx()?;
        println!("{} nginx", "reloaded:".green());
    }

    // 2. Remove FPM pools + reload
    let mut fpm_versions_reloaded: Vec<String> = Vec::new();
    for (v, path) in &fpm_pools {
        std::fs::remove_file(path)?;
        println!("{} {}", "removed:".green(), path.display());
        if !fpm_versions_reloaded.contains(v) {
            run_status("systemctl", &["reload", &format!("php{v}-fpm")])?;
            println!("{} php{v}-fpm", "reloaded:".green());
            fpm_versions_reloaded.push(v.clone());
        }
    }

    // 3. Delete certbot certificates
    for cert in &cert_names {
        match run_status("certbot", &["delete", "--cert-name", cert, "--non-interactive"]) {
            Ok(_) => println!("{} certbot cert '{cert}'", "deleted:".green()),
            Err(e) => println!("{} certbot cert '{cert}': {e}", "warn:".yellow()),
        }
    }

    // 4. Delete project directory
    if dir_exists {
        std::fs::remove_dir_all(&dir)?;
        println!("{} {dir}", "deleted:".green());
    }

    // 5. Remove system user and its group
    if user_exists {
        // Remove www-data from the site group first so userdel doesn't warn about members
        let _ = run_status("gpasswd", &["-d", "www-data", &user]);

        match run_status("userdel", &[&user]) {
            Ok(_) => println!("{} user '{user}'", "deleted:".green()),
            Err(e) => println!("{} userdel '{user}': {e}", "warn:".yellow()),
        }
        // groupdel in case the group outlived the user
        if run("getent", &["group", &user]).is_ok() {
            let _ = run_status("groupdel", &[&user]);
        }
    }

    println!("\n{} {domain}", "done:".green().bold());
    Ok(())
}
