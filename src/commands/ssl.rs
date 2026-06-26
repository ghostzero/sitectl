use anyhow::Result;
use colored::Colorize;

use crate::system::{check_dns, run_status, DnsStatus};

const AVAILABLE: &str = "/etc/nginx/sites-available";

/// Parse server_name values from an existing nginx vhost config.
fn vhost_server_names(domain: &str) -> Vec<String> {
    let path = format!("{AVAILABLE}/{domain}");
    let content = std::fs::read_to_string(path).unwrap_or_default();
    content
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
        .collect()
}

pub fn cmd_enable(domain: &str) -> Result<()> {
    let vhost = format!("{AVAILABLE}/{domain}");
    if !std::path::Path::new(&vhost).exists() {
        anyhow::bail!("no nginx vhost found at {vhost} — run sitectl nginx add first");
    }

    let names = vhost_server_names(domain);
    if names.is_empty() {
        anyhow::bail!("could not read server_name from {vhost}");
    }

    println!("{}", "Checking DNS...".bold());
    let mut dns_ok = true;
    for name in &names {
        match check_dns(name) {
            DnsStatus::Ok(ips) => {
                let ip_str = ips.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
                println!("  {} {} -> {}", "ok:".green(), name, ip_str);
            }
            DnsStatus::WrongServer { resolved, server } => {
                let resolved_str = resolved.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
                let server_str = server.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
                println!("  {} {} resolves to {} (server is {})", "fail:".red(), name, resolved_str, server_str);
                dns_ok = false;
            }
            DnsStatus::Unresolved => {
                println!("  {} {} does not resolve", "fail:".red(), name);
                dns_ok = false;
            }
        }
    }
    println!();

    if !dns_ok {
        anyhow::bail!(
            "one or more domains do not point to this server\n  \
             Fix DNS or use --skip-dns-check to bypass"
        );
    }

    let mut certbot_args = vec!["--nginx", "--non-interactive", "--agree-tos"];
    let d_args: Vec<String> = names.iter().flat_map(|d| vec!["-d".to_string(), d.clone()]).collect();
    let d_refs: Vec<&str> = d_args.iter().map(String::as_str).collect();
    certbot_args.extend_from_slice(&d_refs);

    run_status("certbot", &certbot_args)?;
    println!("{} SSL enabled for {}", "done:".green().bold(), names.join(", "));
    Ok(())
}
