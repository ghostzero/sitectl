use anyhow::Result;
use colored::Colorize;
use std::os::unix::fs::PermissionsExt;

use crate::system::{check_dns, domain_to_user, fpm_pool_path, run, run_status, user_exists, DnsStatus, WWW_ROOT};

pub fn cmd_init(repo: &str, domains: &[String], php: &str, branch: Option<&str>, skip_dns: bool) -> Result<()> {
    if domains.is_empty() {
        anyhow::bail!("at least one domain is required");
    }

    let primary = &domains[0];
    let user = domain_to_user(primary);
    let dir = format!("{WWW_ROOT}/{primary}");
    let git_url = format!("git@github.com:{repo}.git");

    println!("{}", format!("Initializing {repo}").bold());
    println!("  primary domain: {primary}");
    println!("  all domains:    {}", domains.join(", "));
    println!("  git:            {git_url}");
    println!("  directory:      {dir}");
    println!("  php:            {php}");
    // --- DNS check ---
    if skip_dns {
        println!("{} DNS check skipped", "warn:".yellow());
    } else {
        println!("{}", "Checking DNS...".bold());
        let mut dns_ok = true;
        for domain in domains {
            match check_dns(domain) {
                DnsStatus::Ok(ips) => {
                    let ip_str = ips.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
                    println!("  {} {} -> {}", "ok:".green(), domain, ip_str);
                }
                DnsStatus::WrongServer { resolved, server } => {
                    let resolved_str = resolved.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
                    let server_str = server.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
                    println!("  {} {} resolves to {} (server is {})", "fail:".red(), domain, resolved_str, server_str);
                    dns_ok = false;
                }
                DnsStatus::Unresolved => {
                    println!("  {} {} does not resolve", "fail:".red(), domain);
                    dns_ok = false;
                }
            }
        }
        println!();
        if !dns_ok {
            anyhow::bail!(
                "one or more domains do not point to this server\n  \
                 Use --skip-dns-check to bypass (e.g. Cloudflare proxy, split-horizon DNS)"
            );
        }
    }

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

    // 2. Clone repo (or skip if already cloned)
    let git_dir = format!("{dir}/.git");
    if std::path::Path::new(&git_dir).exists() {
        println!("{} already cloned — pulling latest", "skip:".dimmed());
        run_status("git", &["-C", &dir, "pull"])?;
    } else {
        // Ensure parent exists but let git create the target dir
        let _ = std::fs::remove_dir(&dir); // remove if empty
        let mut args = vec!["clone"];
        let branch_owned;
        if let Some(b) = branch {
            branch_owned = b.to_string();
            args.extend_from_slice(&["--branch", &branch_owned]);
        }
        args.extend_from_slice(&[&git_url, &dir]);
        run_status("git", &args)?;
        println!("{} {} -> {}", "cloned:".green(), git_url, dir);
    }

    // 3. Ownership + top-level permissions
    run("chown", &["-R", &format!("{user}:{user}"), &dir])?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o750))?;
    println!("{} {dir} -> {user}:{user} 750", "chown:".green());

    // 4. PHP-FPM pool
    let pool_path = fpm_pool_path(primary, php);
    if pool_path.exists() {
        println!("{} FPM pool already exists", "skip:".dimmed());
    } else {
        super::fpm::cmd_add(primary, php)?;
    }

    // 5. Nginx vhost (multi-domain)
    let vhost_path = format!("/etc/nginx/sites-available/{primary}");
    if std::path::Path::new(&vhost_path).exists() {
        println!("{} nginx vhost already exists", "skip:".dimmed());
    } else {
        let domain_refs: Vec<&str> = domains.iter().map(String::as_str).collect();
        super::nginx::create_vhost_multi(&domain_refs, php, "public")?;
        super::nginx::cmd_enable(primary)?;
    }

    // 6. Fix all permissions (chown -R, g+rX, www-data group, .env 600)
    println!();
    super::project::cmd_fix(primary)?;

    // 7. Certbot SSL
    println!("\n{}", "Running certbot for SSL...".bold());
    let mut certbot_args = vec!["--nginx", "--non-interactive", "--agree-tos"];
    let d_args: Vec<String> = domains.iter().flat_map(|d| vec!["-d".to_string(), d.clone()]).collect();
    let d_refs: Vec<&str> = d_args.iter().map(String::as_str).collect();
    certbot_args.extend_from_slice(&d_refs);

    match run_status("certbot", &certbot_args) {
        Ok(_) => println!("{} SSL certificate issued", "certbot:".green()),
        Err(e) => println!("{} certbot failed: {e} — configure SSL manually", "warn:".yellow()),
    }

    println!("\n{} {primary}", "done:".green().bold());
    println!("  Deploy .env to {dir}/.env and run: php artisan migrate");

    Ok(())
}
