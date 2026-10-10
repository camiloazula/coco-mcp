//! How this copy was installed, and the command that upgrades it.
//!
//! The npm shim knows best: it sees its own directory (the npx cache, the
//! global root or a project) and the package manager that started it
//! (`npm_config_user_agent`), and passes both on in `COCO_INSTALL` as
//! `<manager>-<scope>`, e.g. `npm-global` or `pnpm-exec`. Without it, the
//! executable's path decides: Homebrew keeps every version under
//! `Cellar/coco-mcp/`, `cargo install` puts it in `.cargo/bin`, and a binary
//! under `node_modules` came from npm.

use std::path::Path;

/// The variable the npm shim sets.
pub const INSTALL_VAR: &str = "COCO_INSTALL";

/// The package manager behind a JavaScript install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    /// npm, and npx.
    Npm,
    /// pnpm, and `pnpm dlx`.
    Pnpm,
    /// Yarn, and `yarn dlx`.
    Yarn,
    /// Bun, and `bunx`.
    Bun,
}

/// Where a JavaScript install lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Installed globally (`npm i -g`).
    Global,
    /// A dependency of a project (`npm i`).
    Project,
    /// Run without installing (`npx`, `pnpm dlx`, `yarn dlx`, `bunx`).
    Exec,
}

/// How this copy was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// The Homebrew formula.
    Homebrew,
    /// One of the npm packages, through a JavaScript package manager.
    Js(Manager, Scope),
    /// `cargo install` from the repository.
    Cargo,
    /// A release archive, a build from source, or anything not recognised.
    Archive,
}

impl Channel {
    /// The install this process belongs to: `COCO_INSTALL` when it names
    /// one, else the path of the running executable.
    pub fn detect() -> Self {
        let named = std::env::var(INSTALL_VAR).ok();
        let exe = std::env::current_exe().ok();
        // Homebrew's `bin/coco-mcp` is a link into the Cellar.
        let exe = exe.map(|path| std::fs::canonicalize(&path).unwrap_or(path));
        Self::from_parts(named.as_deref(), exe.as_deref())
    }

    /// [`Channel::detect`] over given values.
    pub fn from_parts(named: Option<&str>, exe: Option<&Path>) -> Self {
        if let Some(channel) = named.and_then(Self::parse) {
            return channel;
        }
        let Some(exe) = exe else {
            return Self::Archive;
        };
        let parts: Vec<_> = exe
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect();
        let has = |a: &str, b: &str| parts.windows(2).any(|w| w[0] == a && w[1] == b);
        if has("Cellar", "coco-mcp") {
            Self::Homebrew
        } else if has(".cargo", "bin") {
            Self::Cargo
        } else if parts.iter().any(|p| p == "_npx") {
            Self::Js(Manager::Npm, Scope::Exec)
        } else if parts.iter().any(|p| p == "node_modules") {
            // The shim would have said more; global is the common case.
            Self::Js(Manager::Npm, Scope::Global)
        } else {
            Self::Archive
        }
    }

    /// Read `<manager>-<scope>`, or `homebrew` and `archive`.
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "homebrew" => return Some(Self::Homebrew),
            "archive" => return Some(Self::Archive),
            "cargo" => return Some(Self::Cargo),
            _ => {}
        }
        let (manager, scope) = value.trim().split_once('-')?;
        let manager = match manager {
            "npm" => Manager::Npm,
            "pnpm" => Manager::Pnpm,
            "yarn" => Manager::Yarn,
            "bun" => Manager::Bun,
            _ => return None,
        };
        let scope = match scope {
            "global" => Scope::Global,
            "project" => Scope::Project,
            "exec" => Scope::Exec,
            _ => return None,
        };
        Some(Self::Js(manager, scope))
    }

    /// The command that upgrades this install, or `None` when there is
    /// none to give and the release page is the way.
    pub fn command(self) -> Option<&'static str> {
        use Manager::{Bun, Npm, Pnpm, Yarn};
        use Scope::{Exec, Global, Project};
        Some(match self {
            Self::Homebrew => "brew upgrade coco-mcp",
            Self::Cargo => {
                "cargo install --locked --git https://github.com/camiloazula/coco-mcp coco-mcp"
            }
            Self::Js(Npm, Global) => "npm i -g coco-mcp@latest",
            Self::Js(Npm, Project) => "npm i coco-mcp@latest",
            Self::Js(Npm, Exec) => "npx coco-mcp@latest",
            Self::Js(Pnpm, Global) => "pnpm add -g coco-mcp@latest",
            Self::Js(Pnpm, Project) => "pnpm add coco-mcp@latest",
            Self::Js(Pnpm, Exec) => "pnpm dlx coco-mcp@latest",
            Self::Js(Yarn, Global) => "yarn global add coco-mcp@latest",
            Self::Js(Yarn, Project) => "yarn add coco-mcp@latest",
            Self::Js(Yarn, Exec) => "yarn dlx coco-mcp@latest",
            Self::Js(Bun, Global) => "bun add -g coco-mcp@latest",
            Self::Js(Bun, Project) => "bun add coco-mcp@latest",
            Self::Js(Bun, Exec) => "bunx coco-mcp@latest",
            Self::Archive => return None,
        })
    }

    /// Whether this install follows a release through a package that is
    /// published after it ([`crate::PACKAGE_LAG`]).
    pub fn lags(self) -> bool {
        matches!(self, Self::Homebrew | Self::Js(..))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_path(path: &str) -> Channel {
        Channel::from_parts(None, Some(Path::new(path)))
    }

    #[test]
    fn the_shim_names_the_install() {
        assert_eq!(
            Channel::from_parts(Some("pnpm-global"), None),
            Channel::Js(Manager::Pnpm, Scope::Global)
        );
        assert_eq!(
            Channel::from_parts(
                Some("npm-exec"),
                Some(Path::new(
                    "/opt/homebrew/Cellar/coco-mcp/0.4.0/bin/coco-mcp"
                ))
            ),
            Channel::Js(Manager::Npm, Scope::Exec),
            "the variable outranks the path"
        );
        assert_eq!(
            Channel::from_parts(Some("homebrew"), None),
            Channel::Homebrew
        );
    }

    #[test]
    fn a_value_not_understood_falls_back_to_the_path() {
        for value in ["", "cargo-global", "npm-somewhere", "npm"] {
            assert_eq!(
                Channel::from_parts(
                    Some(value),
                    Some(Path::new("/usr/local/Cellar/coco-mcp/0.4.0/bin/coco-mcp"))
                ),
                Channel::Homebrew,
                "{value:?}"
            );
        }
    }

    #[test]
    fn the_path_tells_homebrew_npm_and_archives_apart() {
        assert_eq!(
            from_path("/opt/homebrew/Cellar/coco-mcp/0.4.0/bin/coco-mcp"),
            Channel::Homebrew
        );
        assert_eq!(
            from_path("/home/linuxbrew/.linuxbrew/Cellar/coco-mcp/0.4.0/bin/coco-mcp"),
            Channel::Homebrew
        );
        assert_eq!(
            from_path("/Users/me/.npm/_npx/1a2b/node_modules/coco-mcp-darwin-arm64/coco-mcp"),
            Channel::Js(Manager::Npm, Scope::Exec)
        );
        assert_eq!(
            from_path(
                "/usr/local/lib/node_modules/coco-mcp/node_modules/coco-mcp-darwin-arm64/coco-mcp"
            ),
            Channel::Js(Manager::Npm, Scope::Global)
        );
        assert_eq!(from_path("/Users/me/.cargo/bin/coco-mcp"), Channel::Cargo);
        assert_eq!(from_path("/Users/me/bin/coco-mcp"), Channel::Archive);
        assert_eq!(
            from_path("/Users/me/src/Cellar/other/coco-mcp"),
            Channel::Archive,
            "a Cellar of something else"
        );
        assert_eq!(Channel::from_parts(None, None), Channel::Archive);
    }

    #[test]
    fn every_install_but_an_archive_has_a_command_and_lags() {
        assert_eq!(Channel::Homebrew.command(), Some("brew upgrade coco-mcp"));
        assert_eq!(
            Channel::Js(Manager::Npm, Scope::Exec).command(),
            Some("npx coco-mcp@latest")
        );
        assert_eq!(Channel::Archive.command(), None);
        assert!(Channel::Homebrew.lags());
        assert!(Channel::Js(Manager::Bun, Scope::Project).lags());
        assert!(!Channel::Archive.lags());
        assert!(!Channel::Cargo.lags(), "it builds from the repository");
    }
}
