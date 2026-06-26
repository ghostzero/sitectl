use clap::{Parser, Subcommand};

mod commands;
mod system;

use commands::{audit, env, fpm, init, nginx, project, rm, ssl};

#[derive(Parser)]
#[command(name = "sitectl", about = "Web server project administration", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage web projects
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Manage PHP-FPM pools
    Fpm {
        #[command(subcommand)]
        action: FpmAction,
    },
    /// Manage nginx vhosts
    Nginx {
        #[command(subcommand)]
        action: NginxAction,
    },
    /// Manage .env file security
    Env {
        #[command(subcommand)]
        action: EnvAction,
    },
    /// Manage SSL certificates
    Ssl {
        #[command(subcommand)]
        action: SslAction,
    },
    /// Run security audit across all projects (or a single domain)
    Audit {
        domain: Option<String>,
    },
    /// Remove a project: nginx vhost, FPM pool, certbot cert, directory, system user
    Rm {
        domain: String,
        /// Actually perform the removal (dry-run without this flag)
        #[arg(long)]
        yes: bool,
    },
    /// Clone a GitHub repo and fully provision a new project
    Init {
        /// Primary domain — determines /var/www/<domain> and nginx server_name
        domain: String,
        /// GitHub repo slug, e.g. anikeen-com/signed-cards
        repo: String,
        /// Additional domains to include in the nginx server_name and certbot cert
        #[arg(short = 'd', long = "domain")]
        extra_domains: Vec<String>,
        #[arg(long, default_value = "8.3")]
        php: String,
        #[arg(long, help = "Git branch to clone")]
        branch: Option<String>,
        /// Skip DNS verification (use for Cloudflare-proxied or split-horizon DNS)
        #[arg(long)]
        skip_dns_check: bool,
        /// Skip certbot — leave the site on HTTP only
        #[arg(long)]
        no_ssl: bool,
    },
}

#[derive(Subcommand)]
enum ProjectAction {
    /// Scaffold a new project: system user + directory + FPM pool + nginx vhost
    New {
        domain: String,
        #[arg(long, default_value = "8.3", help = "PHP version")]
        php: String,
        #[arg(long, default_value = "laravel", help = "Project type: laravel, static")]
        r#type: String,
    },
    /// List all projects with ownership, permissions and service status
    List,
    /// Fix directory and .env permissions for a project
    Fix { domain: String },
}

#[derive(Subcommand)]
enum FpmAction {
    /// Create a PHP-FPM pool for a domain
    Add {
        domain: String,
        #[arg(long, default_value = "8.3")]
        php: String,
    },
    /// List all PHP-FPM pools
    List,
    /// Remove a PHP-FPM pool
    Remove {
        domain: String,
        #[arg(long, default_value = "8.3")]
        php: String,
    },
}

#[derive(Subcommand)]
enum NginxAction {
    /// Scaffold a nginx vhost config
    Add {
        domain: String,
        #[arg(long, default_value = "8.3")]
        php: String,
        #[arg(long, default_value = "public", help = "Web root subdirectory")]
        root: String,
    },
    /// Remove a nginx vhost
    Remove { domain: String },
    /// Enable a vhost (create sites-enabled symlink)
    Enable { domain: String },
    /// Disable a vhost (remove sites-enabled symlink)
    Disable { domain: String },
    /// Update fastcgi_pass in an existing vhost to the per-site FPM socket
    SetSocket {
        /// Nginx config filename (in sites-available)
        domain: String,
        #[arg(long, default_value = "8.3")]
        php: String,
        /// Override the socket path instead of deriving it from the domain
        #[arg(long)]
        socket: Option<String>,
    },
    /// Cross-reference nginx vhosts with /var/www project directories
    Audit,
}

#[derive(Subcommand)]
enum SslAction {
    /// Issue or renew an SSL certificate via certbot (reads domains from the nginx vhost)
    Enable { domain: String },
}

#[derive(Subcommand)]
enum EnvAction {
    /// List all .env files with their permissions
    List,
    /// Fix .env permissions to 600 (all projects or a specific domain)
    Secure { domain: Option<String> },
}

fn main() -> anyhow::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("error: sitectl must be run as root");
        std::process::exit(1);
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Project { action } => match action {
            ProjectAction::New { domain, php, r#type } => project::cmd_new(&domain, &php, &r#type),
            ProjectAction::List => project::cmd_list(),
            ProjectAction::Fix { domain } => project::cmd_fix(&domain),
        },
        Commands::Fpm { action } => match action {
            FpmAction::Add { domain, php } => fpm::cmd_add(&domain, &php),
            FpmAction::List => fpm::cmd_list(),
            FpmAction::Remove { domain, php } => fpm::cmd_remove(&domain, &php),
        },
        Commands::Nginx { action } => match action {
            NginxAction::Add { domain, php, root } => nginx::cmd_add(&domain, &php, &root),
            NginxAction::Remove { domain } => nginx::cmd_remove(&domain),
            NginxAction::Enable { domain } => nginx::cmd_enable(&domain),
            NginxAction::Disable { domain } => nginx::cmd_disable(&domain),
            NginxAction::SetSocket { domain, php, socket } => nginx::cmd_set_socket(&domain, &php, socket.as_deref()),
            NginxAction::Audit => nginx::cmd_audit(),
        },
        Commands::Env { action } => match action {
            EnvAction::List => env::cmd_list(),
            EnvAction::Secure { domain } => env::cmd_secure(domain.as_deref()),
        },
        Commands::Ssl { action } => match action {
            SslAction::Enable { domain } => ssl::cmd_enable(&domain),
        },
        Commands::Audit { domain } => audit::cmd_run(domain.as_deref()),
        Commands::Init { domain, repo, extra_domains, php, branch, skip_dns_check, no_ssl } => {
            let mut domains = vec![domain];
            domains.extend(extra_domains);
            init::cmd_init(&repo, &domains, &php, branch.as_deref(), skip_dns_check, no_ssl)
        }
        Commands::Rm { domain, yes } => rm::cmd_rm(&domain, yes),
    }
}
