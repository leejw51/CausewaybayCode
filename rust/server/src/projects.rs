//! Which folders the server works in: any folder under the user's home folder (also through a
//! link placed directly in the home folder, such as ~/vivid -> /Volumes/disk/vivid), except
//! the home folder itself, credentials and the server's own folder.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub struct Roots {
    /// The home folder, resolved.
    home: PathBuf,
    /// The home folder and the targets of the links directly in it.
    allowed: Vec<PathBuf>,
    /// The server's own folder (~/.cwbcode).
    server_home: PathBuf,
}

const OFF_LIMITS: [&str; 8] = [".ssh", ".aws", ".gnupg", ".kube", ".docker", ".config", "Library", ".Trash"];

impl Roots {
    pub fn new(home: &Path, server_home: &Path) -> Result<Self> {
        let home = home.canonicalize().with_context(|| format!("no home folder {}", home.display()))?;
        let mut allowed = vec![home.clone()];
        if let Ok(entries) = std::fs::read_dir(&home) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_symlink()) {
                    if let Ok(target) = entry.path().canonicalize() {
                        if target.is_dir() && !allowed.contains(&target) && target.parent().is_some() {
                            allowed.push(target);
                        }
                    }
                }
            }
        }
        Ok(Self { home, allowed, server_home: server_home.canonicalize().unwrap_or_else(|_| server_home.to_path_buf()) })
    }

    /// The home folder, as clients are told it (they show paths under it as `~/...`).
    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn from_env(server_home: &Path) -> Result<Self> {
        Self::new(Path::new(&std::env::var("HOME").context("HOME is not set")?), server_home)
    }

    fn expand(&self, path: &str) -> Result<PathBuf> {
        let path = path.trim();
        let expanded = if path == "~" {
            self.home.clone()
        } else if let Some(rest) = path.strip_prefix("~/") {
            self.home.join(rest)
        } else {
            PathBuf::from(path)
        };
        if !expanded.is_absolute() {
            bail!("give the project's folder as an absolute path or as ~/...");
        }
        Ok(expanded)
    }

    /// Open a folder as a project, making it (and the folders above it that are missing)
    /// if it does not exist. Returns the folder and whether it was made. Nothing is made
    /// unless the folder would be one the server may work in.
    pub fn resolve_or_create(&self, path: &str) -> Result<(PathBuf, bool)> {
        let expanded = self.expand(path)?;
        if expanded.exists() {
            return Ok((self.resolve(path)?, false));
        }
        // Where it would be: the deepest folder that exists, resolved, plus the rest as named.
        let mut rest = Vec::new();
        let mut existing = expanded.as_path();
        while !existing.exists() {
            match (existing.parent(), existing.file_name()) {
                (Some(parent), Some(name)) => {
                    rest.push(name.to_os_string());
                    existing = parent;
                }
                // ("..", or nothing left to stand on.)
                _ => bail!("{} is not a folder name cwbcode can create", expanded.display()),
            }
        }
        if rest.iter().any(|part| part == ".." || part == ".") {
            bail!("{} is not a folder name cwbcode can create", expanded.display());
        }
        let would_be = rest.iter().rev().fold(existing.canonicalize()?, |p, part| p.join(part));
        self.check(&would_be)?;
        std::fs::create_dir_all(&would_be).with_context(|| format!("cannot create {}", would_be.display()))?;
        Ok((self.resolve(&would_be.to_string_lossy())?, true))
    }

    /// The folder a client named, resolved, if the server may work in it.
    pub fn resolve(&self, path: &str) -> Result<PathBuf> {
        let expanded = self.expand(path)?;
        let real = expanded.canonicalize().with_context(|| format!("there is no folder {}", expanded.display()))?;
        if !real.is_dir() {
            bail!("{} is not a folder", real.display());
        }
        self.check(&real)?;
        Ok(real)
    }

    /// May the server work in this (resolved) folder?
    fn check(&self, real: &Path) -> Result<()> {
        if !self.allowed.iter().any(|root| real.starts_with(root)) {
            bail!("{} is not under your home folder: cwbcode works only in folders under ~", real.display());
        }
        if real == self.home {
            bail!("the home folder itself cannot be a project; choose a folder inside it");
        }
        if real.starts_with(&self.server_home) || OFF_LIMITS.iter().any(|d| real.starts_with(self.home.join(d))) {
            bail!("{} is off limits (credentials, system data or cwbcode's own folder)", real.display());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_folders_under_home_are_projects() {
        let base = std::env::temp_dir().join(format!("cwbcode-roots-{}", std::process::id())).canonicalize().unwrap_or_else(|_| {
            let b = std::env::temp_dir().join(format!("cwbcode-roots-{}", std::process::id()));
            std::fs::create_dir_all(&b).unwrap();
            b.canonicalize().unwrap()
        });
        let home = base.join("home/user");
        for dir in ["home/user/code/app", "home/user/.ssh", "home/user/.cwbcode", "home/user/Library/x", "elsewhere/secret", "disk/vivid/proj"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        std::fs::write(home.join("file.txt"), "x").unwrap();
        // A link in the home folder to another disk, and a link from inside a project out of it.
        let _ = std::os::unix::fs::symlink(base.join("disk/vivid"), home.join("vivid"));
        let _ = std::os::unix::fs::symlink(base.join("elsewhere"), home.join("code/escape"));
        let roots = Roots::new(&home, &home.join(".cwbcode")).unwrap();
        let ok = |p: &str| roots.resolve(p).unwrap();
        assert_eq!(ok(&home.join("code/app").to_string_lossy()), home.join("code/app"));
        assert_eq!(ok("~/code"), home.join("code"));
        assert_eq!(ok(&home.join("code/app/../app/").to_string_lossy()), home.join("code/app"));
        // Through the home folder's link, and by the linked disk's own path.
        assert_eq!(ok("~/vivid/proj"), base.join("disk/vivid/proj"));
        assert_eq!(ok(&base.join("disk/vivid/proj").to_string_lossy()), base.join("disk/vivid/proj"));
        for refused in ["~", "/", "/etc", "~/.ssh", "~/.cwbcode", "~/Library/x", "~/missing", "~/file.txt", "code/app", "", "~/code/escape/secret", "~/code/../../.."] {
            assert!(roots.resolve(refused).is_err(), "{refused} was accepted");
        }
        assert!(roots.resolve(&base.join("elsewhere/secret").to_string_lossy()).is_err());

        // A folder that does not exist is made, with the folders above it, when opened.
        let (made, created) = roots.resolve_or_create("~/code/new/deep/project").unwrap();
        assert!(created && made == home.join("code/new/deep/project") && made.is_dir());
        assert_eq!(roots.resolve_or_create("~/code/new/deep/project").unwrap(), (made, false));
        assert_eq!(roots.resolve_or_create("~/vivid/fresh").unwrap(), (base.join("disk/vivid/fresh"), true));
        // Nothing is made where the server may not work, however the path is spelled.
        for refused in ["/tmp/cwbcode-roots-nope/x", "~/.ssh/new", "~/.cwbcode/proj", "~/Library/new", "~/code/escape/made", "~/code/missing/../../../outside", "relative/new", "~/file.txt/under-a-file"] {
            assert!(roots.resolve_or_create(refused).is_err(), "{refused} was created");
        }
        assert!(!base.join("elsewhere/made").exists() && !home.join(".ssh/new").exists() && !base.join("outside").exists() && !std::path::Path::new("/tmp/cwbcode-roots-nope").exists());
    }
}
