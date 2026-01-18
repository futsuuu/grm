use std::io::{BufRead as _, IsTerminal as _};

use anyhow::Context as _;
use clap::Parser as _;

const DEFAULT_HOST: &str = "github.com";

/// Git Repository Manager
#[derive(clap::Parser)]
enum CliCommand {
    /// Print repositories' root directory
    Root,

    /// List managed local repositories
    #[command(visible_alias = "ls")]
    List {
        /// Print absolute paths
        #[arg(long, short = 'l', default_value_t = false)]
        absolute: bool,
    },

    /// Clone a remote repository
    #[command(visible_alias = "g", alias = "clone")]
    Get {
        repo: String,
        /// Clone with SSH instead of HTTPS
        #[arg(long, default_value_t = false)]
        ssh: bool,
        /// Set fetch depth, 0 means to pull everything
        #[arg(long, default_value_t = 0)]
        depth: i32,
    },

    /// Create a new local repository
    #[command(visible_alias = "n")]
    New {
        repo: String,
        /// Don't infer the origin URL
        #[arg(long, short, default_value_t = false)]
        raw: bool,
        /// Use SSH scheme for the origin URL instead of HTTPS scheme
        #[arg(long, default_value_t = false)]
        ssh: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let command = {
        let stdin = std::io::stdin().lock();
        if stdin.is_terminal() {
            CliCommand::parse()
        } else {
            CliCommand::parse_from(std::env::args().chain(stdin.lines().map_while(Result::ok)))
        }
    };

    match command {
        CliCommand::Root => {
            let config = open_config(false)?;
            println!("{}", get_root_dir(&config)?.display());
        }

        CliCommand::List { absolute } => {
            let config = open_config(false)?;
            let root_dir = get_root_dir(&config)?;

            let mut walker = walkdir::WalkDir::new(&root_dir).min_depth(1).into_iter();
            while let Some(Ok(entry)) = walker.next() {
                let path = entry.path();
                if git2::Repository::open(path).is_err() {
                    continue;
                }
                let path = if absolute {
                    path
                } else {
                    path.strip_prefix(&root_dir).unwrap_or(path)
                };
                println!("{}", path.to_string_lossy().to_string().replace('\\', "/"));
                walker.skip_current_dir();
            }
        }

        CliCommand::Get { repo, ssh, depth } => {
            let config = open_config(true)?;
            let root_dir = get_root_dir(&config)?;
            let username = get_username(&config)?;

            let origin_url = get_origin_url(&username, ssh, &repo)?;
            println!("origin: {origin_url}");
            let path = get_repo_path(&root_dir, &origin_url)?;
            let path = std::path::absolute(&path).unwrap_or(path);
            println!("path: {}", path.display());

            let mut command = std::process::Command::new("git");
            command.arg("clone");

            if depth > 0 {
                command.arg("--depth").arg(depth.to_string());
            }

            command.arg(origin_url.as_str()).arg(path);

            let status = command
                .status()
                .with_context(|| format!("failed to execute {command:?}"))?;

            if !status.success() {
                anyhow::bail!("{command:?} failed with {status}");
            }
        }

        CliCommand::New { repo, ssh, raw } => {
            let config = open_config(true)?;
            let root_dir = get_root_dir(&config)?;
            let username = get_username(&config)?;

            let mut opts = git2::RepositoryInitOptions::new();
            opts.no_reinit(true);

            let path = if raw {
                root_dir.join(repo)
            } else {
                let origin_url = get_origin_url(&username, ssh, &repo)?;
                opts.origin_url(origin_url.as_str());
                println!("origin: {origin_url}");
                get_repo_path(&root_dir, &origin_url)?
            };
            let path = std::path::absolute(&path).unwrap_or(path);
            println!("path: {}", path.display());

            let repo = git2::Repository::init_opts(path, &opts)?;
            if !raw {
                let mut config = repo.config()?;
                let branch = get_default_branch(&config);
                config.set_str(&format!("branch.{branch}.remote"), "origin")?;
                config.set_str(
                    &format!("branch.{branch}.merge"),
                    &format!("refs/heads/{branch}"),
                )?;
            }
        }
    }

    Ok(())
}

fn get_origin_url(username: &str, ssh: bool, repo: &str) -> anyhow::Result<url::Url> {
    match repo.split('/').count() {
        1 => get_origin_url(username, ssh, &format!("{username}/{repo}")),
        2 => get_origin_url(username, ssh, &format!("{DEFAULT_HOST}/{repo}")),
        3 => get_origin_url(
            username,
            ssh,
            &if ssh && repo.contains('@') {
                format!("ssh://{repo}")
            } else if ssh {
                format!("ssh://git@{repo}")
            } else {
                format!("https://{repo}")
            },
        ),
        _ => Ok(url::Url::parse(repo)?),
    }
}

fn get_repo_path(
    root_dir: &std::path::Path,
    origin: &url::Url,
) -> anyhow::Result<std::path::PathBuf> {
    let domain = origin
        .domain()
        .with_context(|| format!("cannot find a domain name from `{origin}`"))?;
    Ok(root_dir
        .join(domain)
        .join(origin.path().trim_start_matches('/')))
}

fn get_root_dir(config: &git2::Config) -> anyhow::Result<std::path::PathBuf> {
    config
        .get_path(concat!(env!("CARGO_PKG_NAME"), ".root"))
        .ok()
        .or_else(|| dirs::home_dir().map(|p| p.join(env!("CARGO_PKG_NAME"))))
        .context("failed to get root dir")
}

fn get_default_branch(config: &git2::Config) -> String {
    config
        .get_string("init.defaultBranch")
        .unwrap_or("master".into())
}

fn get_username(config: &git2::Config) -> anyhow::Result<String> {
    config
        .get_string("user.name")
        .or_else(|_| whoami::fallible::username())
        .context("failed to get username")
}

fn open_config(current_dir: bool) -> anyhow::Result<git2::Config> {
    if current_dir {
        if let Ok(config) = git2::Repository::discover(".").and_then(|r| r.config()) {
            return Ok(config);
        }
    }
    Ok(git2::Config::open_default()?)
}

#[cfg(test)]
mod test_get_origin_url {
    use super::*;

    #[test]
    fn return_parsed_url() -> anyhow::Result<()> {
        assert_eq!(
            url::Url::parse("https://github.com/foo/bar")?,
            get_origin_url("foo", false, "https://github.com/foo/bar")?,
        );
        Ok(())
    }

    #[test]
    fn complete_scheme() -> anyhow::Result<()> {
        assert_eq!(
            url::Url::parse("https://github.com/foo/bar")?,
            get_origin_url("foo", false, "github.com/foo/bar")?,
        );
        Ok(())
    }

    #[test]
    fn complete_remote_host() -> anyhow::Result<()> {
        assert_eq!(
            url::Url::parse("https://github.com/foo/bar")?,
            get_origin_url("foo", false, "foo/bar")?,
        );
        Ok(())
    }

    #[test]
    fn complete_username() -> anyhow::Result<()> {
        assert_eq!(
            url::Url::parse("https://github.com/foo/bar")?,
            get_origin_url("foo", false, "bar")?
        );
        Ok(())
    }
}
