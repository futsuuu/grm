use std::{
    io::{BufRead, IsTerminal},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::Parser;
use dirs::home_dir;
use git2::Repository;
use url::Url;

const DEFAULT_HOST: &str = "github.com";

/// Git Repository Manager
#[derive(Parser)]
enum CliCommand {
    /// Print repositories' root directory
    Root,

    /// List managed local repositories
    #[command(visible_alias = "l")]
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
        /// Use SSH scheme for the origin URL instead of HTTPS scheme
        #[arg(long, default_value_t = false)]
        ssh: bool,
    },
}

fn main() -> Result<()> {
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
                if Repository::open(path).is_err() {
                    continue;
                }
                let path = if absolute {
                    path
                } else {
                    path.strip_prefix(&root_dir).unwrap_or(path)
                };
                println!("{}", path.display().to_string().replace('\\', "/"));
                walker.skip_current_dir();
            }
        }

        CliCommand::Get { repo, ssh, depth } => {
            let config = open_config(true)?;
            let root_dir = get_root_dir(&config)?;
            let username = get_username(&config)?;

            let origin_url = get_origin_url(&username, ssh, &repo)?;
            println!("origin: {origin_url}");
            let path = &get_repo_path(&root_dir, &origin_url)?;
            println!("path: {}", path.display());

            let mut builder = git2::build::RepoBuilder::new();
            builder.fetch_options({
                let mut opts = git2::FetchOptions::new();
                opts.depth(depth);
                opts
            });
            builder.clone(origin_url.as_str(), path)?;
        }

        CliCommand::New { ssh, repo } => {
            let config = open_config(true)?;
            let root_dir = get_root_dir(&config)?;
            let username = get_username(&config)?;

            let origin_url = get_origin_url(&username, ssh, &repo)?;
            println!("origin: {origin_url}");
            let path = get_repo_path(&root_dir, &origin_url)?;
            println!("path: {}", path.display());

            Repository::init_opts(path, &{
                let mut opts = git2::RepositoryInitOptions::new();
                opts.no_reinit(true);
                opts.origin_url(origin_url.as_str());
                opts
            })?;
        }
    }

    Ok(())
}

fn get_origin_url(username: &str, ssh: bool, repo: &str) -> Result<Url> {
    let slash_count = repo.split('/').count() - 1;
    if slash_count == 0 {
        return get_origin_url(username, ssh, &format!("{username}/{repo}"));
    }
    if slash_count == 1 {
        return get_origin_url(username, ssh, &format!("{DEFAULT_HOST}/{repo}"));
    }
    if slash_count == 2 {
        return get_origin_url(
            username,
            ssh,
            &if ssh && repo.contains('@') {
                format!("ssh://{repo}")
            } else if ssh {
                format!("ssh://git@{repo}")
            } else {
                format!("https://{repo}")
            },
        );
    }
    Ok(Url::parse(repo)?)
}

fn get_repo_path(root_dir: &Path, origin: &Url) -> Result<PathBuf> {
    let domain = origin
        .domain()
        .with_context(|| format!("cannot find a domain name from `{origin}`"))?;
    Ok(root_dir
        .join(domain)
        .join(origin.path().trim_start_matches('/')))
}

fn get_root_dir(config: &git2::Config) -> Result<PathBuf> {
    config
        .get_path(concat!(env!("CARGO_PKG_NAME"), ".root"))
        .ok()
        .or_else(|| home_dir().map(|p| p.join(env!("CARGO_PKG_NAME"))))
        .context("failed to get root dir")
}

fn get_username(config: &git2::Config) -> Result<String> {
    config
        .get_string("user.name")
        .or_else(|_| whoami::fallible::username())
        .context("failed to get username")
}

fn open_config(current_dir: bool) -> Result<git2::Config> {
    if current_dir {
        if let Ok(config) = Repository::discover(".").and_then(|r| r.config()) {
            return Ok(config);
        }
    }
    Ok(git2::Config::open_default()?)
}

#[cfg(test)]
mod test_get_origin_url {
    use super::*;

    #[test]
    fn return_parsed_url() -> Result<()> {
        assert_eq!(
            Url::parse("https://github.com/foo/bar")?,
            get_origin_url("foo", false, "https://github.com/foo/bar")?,
        );
        Ok(())
    }

    #[test]
    fn complete_scheme() -> Result<()> {
        assert_eq!(
            Url::parse("https://github.com/foo/bar")?,
            get_origin_url("foo", false, "github.com/foo/bar")?,
        );
        Ok(())
    }

    #[test]
    fn complete_remote_host() -> Result<()> {
        assert_eq!(
            Url::parse("https://github.com/foo/bar")?,
            get_origin_url("foo", false, "foo/bar")?,
        );
        Ok(())
    }

    #[test]
    fn complete_username() -> Result<()> {
        assert_eq!(
            Url::parse("https://github.com/foo/bar")?,
            get_origin_url("foo", false, "bar")?
        );
        Ok(())
    }
}
