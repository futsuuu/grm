use std::io::{BufRead as _, IsTerminal as _, Write as _};

use anyhow::Context as _;

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
        depth: u64,
    },

    /// Create a new local repository
    #[command(visible_alias = "n")]
    New {
        name: String,
        /// Create a new linked worktree of the current repository
        #[arg(long, short = 'w', default_value_t = false)]
        worktree: bool,
        /// Use SSH scheme for the origin URL instead of HTTPS scheme
        #[arg(long, default_value_t = false)]
        ssh: bool,
    },
}

impl CliCommand {
    fn parse() -> Self {
        let stdin = std::io::stdin().lock();
        if stdin.is_terminal() {
            <CliCommand as clap::Parser>::parse()
        } else {
            <CliCommand as clap::Parser>::parse_from(
                std::env::args().chain(stdin.lines().map_while(Result::ok)),
            )
        }
    }
}

fn main() -> anyhow::Result<()> {
    match CliCommand::parse() {
        CliCommand::Root => {
            let app = App::open_default()?;
            println!("{}", DisplayPath(app.root_dir()?));
        }

        CliCommand::List { absolute } => {
            let app = App::open_default()?;
            let root_dir = app.root_dir()?;
            let mut walker = walkdir::WalkDir::new(&root_dir).min_depth(1).into_iter();
            let mut stdout = std::io::stdout().lock();
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
                stdout.write_fmt(format_args!("{}\n", DisplayPath(path)))?;
                walker.skip_current_dir();
            }
        }

        CliCommand::Get { repo, ssh, depth } => {
            let app = App::open_current()?;
            let origin_url = {
                let scheme = if ssh {
                    OriginUrlScheme::Ssh
                } else {
                    OriginUrlScheme::Https
                };
                scheme.get_url(&repo, &app.user_name()?)?
            };
            println!("origin: {origin_url}");
            let path = app.get_repo_path(&origin_url)?;
            println!("path: {}", DisplayPath(&path));

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

        CliCommand::New {
            name,
            worktree: false,
            ssh,
        } => {
            let app = App::open_current()?;
            let origin_url = {
                let scheme = if ssh {
                    OriginUrlScheme::Ssh
                } else {
                    OriginUrlScheme::Https
                };
                scheme.get_url(&name, &app.user_name()?)?
            };
            let mut opts = git2::RepositoryInitOptions::new();
            opts.no_reinit(true);
            opts.origin_url(origin_url.as_str());
            println!("origin: {origin_url}");
            let path = app.get_repo_path(&origin_url)?;
            println!("path: {}", DisplayPath(&path));

            let repo = git2::Repository::init_opts(path, &opts)?;
            let mut config = repo.config()?;
            let branch = config
                .get_string("init.defaultBranch")
                .unwrap_or("master".into());
            config.set_str(&format!("branch.{branch}.remote"), "origin")?;
            config.set_str(
                &format!("branch.{branch}.merge"),
                &format!("refs/heads/{branch}"),
            )?;
        }

        CliCommand::New {
            name,
            worktree: true,
            ..
        } => {
            let app = App::open_current()?;
            let repo = app.current_repo()?;
            let mut branches = Vec::new();
            for entry in repo.branches(Some(git2::BranchType::Local))? {
                let (branch, _) = entry?;
                let Some(branch_name) = branch.name()? else {
                    continue;
                };
                if branch_name.contains(&name) {
                    let branch_name = branch_name.to_string();
                    branches.push((branch, branch_name));
                }
            }
            branches.sort_unstable_by(|(_, lhs), (_, rhs)| {
                lhs.split('/')
                    .count()
                    .cmp(&rhs.split('/').count())
                    .then_with(|| lhs.len().cmp(&rhs.len()))
            });
            let (branch, branch_name) = branches
                .last()
                .with_context(|| format!("'{name}' does not match with any branches"))?;
            println!("branch: {}", branch_name);
            let path = app.get_worktree_path(branch_name)?;
            println!("worktree: {}", DisplayPath(&path));
            _ = std::fs::remove_dir(&path); // remove directory if empty
            anyhow::ensure!(!std::fs::exists(&path)?, "already exists");
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut opts = git2::WorktreeAddOptions::new();
            opts.reference(Some(branch.get()));
            opts.checkout_existing(true);
            repo.worktree(&branch_name.replace('/', "__"), &path, Some(&opts))?;
        }
    }

    Ok(())
}

struct App {
    current: Option<git2::Repository>,
    config: git2::Config,

    root_dir_cache: std::cell::RefCell<Option<std::path::PathBuf>>,
    user_name_cache: std::cell::RefCell<Option<String>>,
}

impl App {
    fn open_current() -> anyhow::Result<Self> {
        let repo = git2::Repository::discover(".").ok();
        let config = if let Some(repo) = &repo {
            repo.config()?
        } else {
            git2::Config::open_default()?
        };
        Ok(Self {
            current: repo,
            config,
            root_dir_cache: None.into(),
            user_name_cache: None.into(),
        })
    }

    fn open_default() -> anyhow::Result<Self> {
        Ok(Self {
            current: None,
            config: git2::Config::open_default()?,
            root_dir_cache: None.into(),
            user_name_cache: None.into(),
        })
    }

    fn current_repo(&self) -> anyhow::Result<&git2::Repository> {
        self.current
            .as_ref()
            .context("current directory is not a git repository")
    }

    fn root_dir(&self) -> anyhow::Result<std::path::PathBuf> {
        if let Some(path) = &*self.root_dir_cache.borrow() {
            return Ok(path.clone());
        };
        let path = self
            .config
            .get_path(concat!(env!("CARGO_PKG_NAME"), ".root"))
            .ok()
            .or_else(|| std::env::home_dir().map(|p| p.join(env!("CARGO_PKG_NAME"))))
            .context("failed to get root directory")?;
        self.root_dir_cache.replace(Some(path.clone()));
        Ok(path)
    }

    fn get_repo_path(&self, origin: &url::Url) -> anyhow::Result<std::path::PathBuf> {
        let domain = origin
            .domain()
            .with_context(|| format!("`{origin}` does not have a domain name"))?;
        Ok(self
            .root_dir()?
            .join(domain)
            .join(origin.path().trim_start_matches('/')))
    }

    fn worktree_root_dir(&self) -> anyhow::Result<std::path::PathBuf> {
        self.root_dir().map(|p| p.join("worktrees"))
    }

    fn get_worktree_path(&self, branch: &str) -> anyhow::Result<std::path::PathBuf> {
        let current = self.current_repo()?;
        let main_worktree_path = if current.is_worktree() {
            let main_worktree = git2::Repository::open_ext(
                current.commondir(),
                git2::RepositoryOpenFlags::NO_SEARCH | git2::RepositoryOpenFlags::NO_DOTGIT,
                &[] as &[&std::ffi::OsStr],
            )?;
            main_worktree
                .workdir()
                .unwrap_or_else(|| main_worktree.path())
                .to_path_buf()
        } else {
            current
                .workdir()
                .unwrap_or_else(|| current.path())
                .to_path_buf()
        };
        let relative = main_worktree_path
            .strip_prefix(self.root_dir()?)
            .context("cannot create a worktree of an unmanaged repository")?;
        Ok(self.worktree_root_dir()?.join(relative).join(branch))
    }

    fn user_name(&self) -> anyhow::Result<String> {
        if let Some(name) = &*self.user_name_cache.borrow() {
            return Ok(name.clone());
        }
        let name = self
            .config
            .get_string("user.name")
            .or_else(|_| whoami::fallible::username())
            .context("failed to get user name")?;
        self.user_name_cache.replace(Some(name.clone()));
        Ok(name)
    }
}

struct DisplayPath<P>(P);

impl<P: AsRef<std::ffi::OsStr>> std::fmt::Display for DisplayPath<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            self.0
                .as_ref()
                .to_string_lossy()
                .replace('\\', "/")
                .as_str(),
        )
    }
}

enum OriginUrlScheme {
    Https,
    Ssh,
}

impl OriginUrlScheme {
    fn get_url(&self, repo: &str, default_username: &str) -> anyhow::Result<url::Url> {
        match repo.split('/').count() {
            1 => self.get_url(&format!("{default_username}/{repo}"), default_username),
            2 => self.get_url(&format!("{DEFAULT_HOST}/{repo}"), default_username),
            3 => self.get_url(
                &match self {
                    OriginUrlScheme::Https => format!("https://{repo}"),
                    OriginUrlScheme::Ssh => {
                        if repo.contains('@') {
                            format!("ssh://{repo}")
                        } else {
                            format!("ssh://git@{repo}")
                        }
                    }
                },
                default_username,
            ),
            _ => Ok(url::Url::parse(repo)?),
        }
    }
}

#[cfg(test)]
mod test_get_origin_url {
    use super::*;

    #[test]
    fn return_parsed_url() -> anyhow::Result<()> {
        assert_eq!(
            url::Url::parse("https://github.com/foo/bar")?,
            OriginUrlScheme::Https.get_url("https://github.com/foo/bar", "foo")?,
        );
        Ok(())
    }

    #[test]
    fn complete_scheme() -> anyhow::Result<()> {
        assert_eq!(
            url::Url::parse("https://github.com/foo/bar")?,
            OriginUrlScheme::Https.get_url("github.com/foo/bar", "foo")?,
        );
        Ok(())
    }

    #[test]
    fn complete_remote_host() -> anyhow::Result<()> {
        assert_eq!(
            url::Url::parse("https://github.com/foo/bar")?,
            OriginUrlScheme::Https.get_url("foo/bar", "foo")?,
        );
        Ok(())
    }

    #[test]
    fn complete_username() -> anyhow::Result<()> {
        assert_eq!(
            url::Url::parse("https://github.com/foo/bar")?,
            OriginUrlScheme::Https.get_url("bar", "foo")?
        );
        Ok(())
    }
}
