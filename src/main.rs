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

    /// Clone a remote repository and print its path
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

    /// Create a new local repository and print its path
    #[command(visible_alias = "n")]
    New {
        name: String,
        /// Use SSH scheme for the origin URL instead of HTTPS scheme
        #[arg(long, default_value_t = false)]
        ssh: bool,
    },

    /// Manage linked worktrees
    #[command(subcommand, visible_alias = "wt")]
    Worktree(WorktreeAction),
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

#[derive(clap::Parser)]
enum WorktreeAction {
    /// Create a new linked worktree and print its path
    #[command(visible_alias = "n")]
    New { name: String },
}

fn main() -> std::process::ExitCode {
    let args = CliCommand::parse();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .format_module_path(false)
        .init();
    match main_inner(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn main_inner(cli_args: CliCommand) -> anyhow::Result<()> {
    match cli_args {
        CliCommand::Root => {
            println!("{}", DisplayPath(App::<Git2, GitCommand>::get_root_dir()?));
        }

        CliCommand::List { absolute } => {
            let root_dir = App::<Git2, GitCommand>::get_root_dir()?;
            let mut walker = walkdir::WalkDir::new(root_dir).min_depth(1).into_iter();
            let mut stdout = std::io::stdout().lock();
            while let Some(Ok(entry)) = walker.next() {
                let path = entry.path();
                if git2::Repository::open(path).is_err() {
                    continue;
                }
                let path = if absolute {
                    path
                } else {
                    path.strip_prefix(root_dir).unwrap_or(path)
                };
                writeln!(stdout, "{}", DisplayPath(path))?;
                walker.skip_current_dir();
            }
        }

        CliCommand::Get { repo, ssh, depth } => {
            let app = App::new();
            let origin_url = {
                let scheme = if ssh {
                    OriginUrlScheme::Ssh
                } else {
                    OriginUrlScheme::Https
                };
                scheme.get_url(&repo, &app.user_name()?)?
            };
            log::info!("origin: {origin_url}");
            let path = app.get_repo_path(&origin_url)?;
            log::info!("path:   {}", DisplayPath(&path));

            app.remote
                .clone_repo(origin_url, &path, CloneOpts { depth })?;

            log::info!("repository cloned");
            println!("{}", DisplayPath(path));
        }

        CliCommand::New { name, ssh } => {
            let app = App::new();
            let origin_url = {
                let scheme = if ssh {
                    OriginUrlScheme::Ssh
                } else {
                    OriginUrlScheme::Https
                };
                scheme.get_url(&name, &app.user_name()?)?
            };
            let path = app.get_repo_path(&origin_url)?;
            log::info!("origin: {origin_url}");
            log::info!("path:   {}", DisplayPath(&path));

            app.local.init_repo(&path, &origin_url)?;
            log::info!("repository initialized");
            println!("{}", DisplayPath(path));
        }

        CliCommand::Worktree(WorktreeAction::New { name }) => {
            let app = App::new();
            let mut branches = Vec::new();
            for branch_name in app.local.iter_local_branches()? {
                let branch_name = branch_name?;
                if branch_name.contains(&name) {
                    log::debug!("branch matched: {branch_name}");
                    let branch_name = branch_name.to_string();
                    branches.push(branch_name);
                } else {
                    log::debug!("branch did not match: {branch_name}");
                }
            }
            log::debug!("matched {} branches", branches.len());
            branches.sort_unstable_by(|lhs, rhs| {
                lhs.split('/')
                    .count()
                    .cmp(&rhs.split('/').count())
                    .then_with(|| lhs.len().cmp(&rhs.len()))
            });
            let branch_name = branches
                .last()
                .with_context(|| format!("'{name}' does not match with any branches"))?;
            log::info!("branch:   {branch_name}");
            let path = app.get_linked_worktree_path(branch_name)?;
            log::info!("worktree: {}", DisplayPath(&path));
            _ = std::fs::remove_dir(&path); // remove directory if empty
            anyhow::ensure!(!std::fs::exists(&path)?, "already exists");
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            app.local.create_linked_worktree(branch_name, &path)?;
            log::info!("worktree created");
            println!("{}", DisplayPath(path));
        }
    }

    Ok(())
}

/// Application context holding local/remote git operations and cached user name
struct App<Local: LocalGitOp, Remote: RemoteGitOp> {
    local: Local,
    remote: Remote,
    user_name_cache: std::cell::RefCell<Option<String>>,
}

impl App<Git2, GitCommand> {
    fn new() -> Self {
        Self {
            local: Git2::new(),
            remote: GitCommand::new(),
            user_name_cache: std::cell::RefCell::new(None),
        }
    }
}

impl<LocalOp: LocalGitOp, RemoteOp: RemoteGitOp> App<LocalOp, RemoteOp> {
    /// Resolve the local path for the given remote origin URL
    fn get_repo_path(&self, origin: &url::Url) -> anyhow::Result<std::path::PathBuf> {
        let domain = origin
            .domain()
            .with_context(|| format!("`{origin}` does not have a domain name"))?;
        Ok(Self::get_root_dir()?
            .join(domain)
            .join(origin.path().trim_start_matches('/')))
    }

    /// Resolve the worktree path for the given branch name
    fn get_linked_worktree_path(&self, branch: &str) -> anyhow::Result<std::path::PathBuf> {
        let main_worktree_path = self.local.get_main_worktree_path()?;
        let relative = main_worktree_path
            .strip_prefix(Self::get_root_dir()?)
            .context("cannot create a worktree of an unmanaged repository")?;
        Ok(Self::get_worktree_root_dir()?.join(relative).join(branch))
    }

    /// Get the Git user name, falling back to the system username
    fn user_name(&self) -> anyhow::Result<String> {
        if let Some(name) = &*self.user_name_cache.borrow() {
            return Ok(name.clone());
        }
        let name = self
            .local
            .read_current_user_name_config()
            .or_else(|_| whoami::username())
            .context("failed to get user name")?;
        self.user_name_cache.replace(Some(name.clone()));
        Ok(name)
    }

    /// Get the root directory for managed repositories
    fn get_root_dir() -> anyhow::Result<&'static std::path::Path> {
        static CACHE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        if let Some(path) = CACHE.get() {
            return Ok(path);
        }
        let path = LocalOp::read_root_dir_config()
            .ok()
            .or_else(|| std::env::home_dir().map(|p| p.join(env!("CARGO_PKG_NAME"))))
            .context("failed to get root directory")?;
        _ = CACHE.set(path);
        Ok(CACHE.get().unwrap())
    }

    /// Get the root directory for linked worktrees (under root/worktrees)
    fn get_worktree_root_dir() -> anyhow::Result<std::path::PathBuf> {
        Self::get_root_dir().map(|p| p.join("worktrees"))
    }
}

/// Local Git operations (read config, branches, worktrees, init)
trait LocalGitOp {
    /// Read the root directory config from global git config
    fn read_root_dir_config() -> anyhow::Result<std::path::PathBuf>;
    fn read_current_user_name_config(&self) -> anyhow::Result<String>;
    fn iter_local_branches(&self) -> anyhow::Result<impl Iterator<Item = anyhow::Result<String>>>;
    fn get_main_worktree_path(&self) -> anyhow::Result<std::path::PathBuf>;
    fn create_linked_worktree(&self, branch: &str, path: &std::path::Path) -> anyhow::Result<()>;
    fn init_repo(&self, path: &std::path::Path, origin: &url::Url) -> anyhow::Result<()>;
}

/// Local Git operations backed by libgit2
struct Git2 {
    current: Option<git2::Repository>,
}

impl Git2 {
    fn new() -> Self {
        Self {
            current: git2::Repository::discover(".").ok(),
        }
    }

    fn current(&self) -> anyhow::Result<&git2::Repository> {
        self.current
            .as_ref()
            .context("current directory is not in a git repository")
    }

    fn config(&self) -> anyhow::Result<git2::Config> {
        match &self.current {
            Some(repo) => Ok(repo.config()?),
            None => Ok(git2::Config::open_default()?),
        }
    }
}

impl LocalGitOp for Git2 {
    fn read_root_dir_config() -> anyhow::Result<std::path::PathBuf> {
        let config = git2::Config::open_default()?;
        Ok(config.get_path(concat!(env!("CARGO_PKG_NAME"), ".root"))?)
    }

    fn get_main_worktree_path(&self) -> anyhow::Result<std::path::PathBuf> {
        let current = self.current()?;
        if current.is_worktree() {
            let main_worktree = git2::Repository::open_ext(
                current.commondir(),
                git2::RepositoryOpenFlags::NO_SEARCH | git2::RepositoryOpenFlags::NO_DOTGIT,
                &[] as &[&std::ffi::OsStr],
            )?;
            Ok(main_worktree
                .workdir()
                .unwrap_or_else(|| main_worktree.path())
                .to_path_buf())
        } else {
            Ok(current
                .workdir()
                .unwrap_or_else(|| current.path())
                .to_path_buf())
        }
    }

    fn read_current_user_name_config(&self) -> anyhow::Result<String> {
        Ok(self.config()?.get_string("user.name")?)
    }

    fn iter_local_branches(&self) -> anyhow::Result<impl Iterator<Item = anyhow::Result<String>>> {
        Ok(self
            .current()?
            .branches(Some(git2::BranchType::Local))?
            .map(|entry| {
                let (branch, _type) = entry?;
                Ok(branch.name()?.map(|s| s.to_string()))
            })
            .filter_map(Result::transpose))
    }

    fn create_linked_worktree(
        &self,
        branch_name: &str,
        path: &std::path::Path,
    ) -> anyhow::Result<()> {
        let current = self.current()?;
        let branch = current.find_branch(branch_name, git2::BranchType::Local)?;
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(branch.get()));
        opts.checkout_existing(true);
        // Directory name within `.git/worktrees`. Branch names are relative paths and
        // cannot contain backslashes, so replace '/' with '__' to avoid nesting.
        let worktree_name = branch_name.replace('/', "__");
        current.worktree(&worktree_name, path, Some(&opts))?;
        Ok(())
    }

    fn init_repo(&self, path: &std::path::Path, origin: &url::Url) -> anyhow::Result<()> {
        let mut opts = git2::RepositoryInitOptions::new();
        opts.no_reinit(true);
        opts.origin_url(origin.as_str());
        let repo = git2::Repository::init_opts(path, &opts)?;
        let mut config = repo.config()?;
        let branch = config
            .get_string("init.defaultBranch")
            .unwrap_or_else(|_| "master".into());
        config.set_str(&format!("branch.{branch}.remote"), "origin")?;
        config.set_str(
            &format!("branch.{branch}.merge"),
            &format!("refs/heads/{branch}"),
        )?;
        Ok(())
    }
}

/// Options for cloning a remote repository
struct CloneOpts {
    depth: u64,
}

/// Remote Git operations (clone, etc.)
trait RemoteGitOp: Sized {
    fn clone_repo(
        &self,
        src: url::Url,
        dest: &std::path::Path,
        opts: CloneOpts,
    ) -> anyhow::Result<()>;
}

/// Remote Git operations backed by the `git` CLI
struct GitCommand;

impl GitCommand {
    fn new() -> Self {
        Self
    }
}

impl RemoteGitOp for GitCommand {
    fn clone_repo(
        &self,
        src: url::Url,
        dest: &std::path::Path,
        opts: CloneOpts,
    ) -> anyhow::Result<()> {
        let mut command = std::process::Command::new("git");
        command.arg("clone");
        if opts.depth > 0 {
            command.arg("--depth").arg(opts.depth.to_string());
        }
        command.arg(src.as_str()).arg(dest);

        let status = command
            .status()
            .with_context(|| format!("failed to execute {command:?}"))?;
        anyhow::ensure!(status.success(), "{command:?} failed with {status}");
        Ok(())
    }
}

/// Wrapper to display paths with forward slashes on Windows
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

/// URL scheme for constructing remote origin URLs
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
