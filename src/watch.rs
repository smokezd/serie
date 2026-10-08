//! Background refreshes: a cheap check of the local repository every [`LOCAL_INTERVAL`], and a
//! fetch of the remote branches the view is scoped to every [`FETCH_INTERVAL`].
//!
//! Neither loop reloads anything itself. Both compare the repository against the fingerprint taken
//! when it was last loaded and, when it differs, ask the app for an ordinary refresh, so a refresh
//! from here keeps the selection and pane state exactly like pressing the refresh key does.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use crate::event::{AppEvent, Sender};

pub const LOCAL_INTERVAL: Duration = Duration::from_secs(20);
pub const FETCH_INTERVAL: Duration = Duration::from_secs(60);

/// What the app last loaded, as the watcher threads see it. Replaced on every load, so a fetch
/// started before a reload is compared against the reloaded state rather than the stale one.
#[derive(Debug, Default)]
struct Loaded {
    fingerprint: String,
    /// Remote branches to fetch, as `<remote>/<branch>` names.
    remote_targets: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Watcher {
    path: PathBuf,
    include_uncommitted: bool,
    loaded: Arc<Mutex<Loaded>>,
}

impl Watcher {
    /// Starts the loops that are enabled. `auto_fetch` only matters when there are remote targets,
    /// which there never are without a revspec, so the default view never touches the network.
    pub fn start(
        path: &Path,
        include_uncommitted: bool,
        auto_refresh: bool,
        auto_fetch: bool,
        tx: Sender,
    ) -> Self {
        let watcher = Self {
            path: path.to_path_buf(),
            include_uncommitted,
            loaded: Arc::default(),
        };
        if auto_refresh {
            let watcher = watcher.clone();
            let tx = tx.clone();
            thread::spawn(move || loop {
                thread::sleep(LOCAL_INTERVAL);
                watcher.refresh_if_changed(&tx);
            });
        }
        if auto_fetch {
            let watcher = watcher.clone();
            thread::spawn(move || loop {
                thread::sleep(FETCH_INTERVAL);
                let targets = watcher.loaded.lock().unwrap().remote_targets.clone();
                if !targets.is_empty() {
                    fetch(&watcher.path, &targets);
                    watcher.refresh_if_changed(&tx);
                }
            });
        }
        watcher
    }

    /// The state the app is about to load. Taken before loading rather than after, so a change
    /// landing in between is seen as a difference and refreshed again, rather than missed.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.path, self.include_uncommitted)
    }

    pub fn loaded(&self, fingerprint: String, remote_targets: Vec<String>) {
        *self.loaded.lock().unwrap() = Loaded {
            fingerprint,
            remote_targets,
        };
    }

    fn refresh_if_changed(&self, tx: &Sender) {
        let current = self.fingerprint();
        // A request the app turns down (in the middle of typing a search, say) is not remembered
        // here: the fingerprint still differs next time round, so it is simply asked again.
        if current != self.loaded.lock().unwrap().fingerprint {
            tx.send(AppEvent::AutoRefresh);
        }
    }
}

/// Everything a reload would render differently: every ref and where HEAD points, plus the
/// worktree status when the uncommitted rows are shown. Each part is the raw output of a git
/// command, which is all a comparison needs.
fn fingerprint(path: &Path, include_uncommitted: bool) -> String {
    let mut fingerprint = git_output(path, &["show-ref", "--head", "--dereference"]);
    // a checkout between two branches on the same commit moves no ref, only HEAD itself
    fingerprint.push_str(&git_output(path, &["symbolic-ref", "--quiet", "HEAD"]));
    if include_uncommitted {
        fingerprint.push_str(&git_output(path, &["status", "--porcelain", "-z"]));
    }
    fingerprint
}

fn git_output(path: &Path, args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
}

/// Fetches each target from its remote, one `git fetch` per remote. git updates the
/// remote-tracking ref of a branch fetched by name as long as the remote's fetch refspec covers
/// it, which the default one does.
///
/// Runs while the TUI owns the terminal, so nothing may prompt: credentials are refused rather
/// than asked for, and ssh is put in batch mode unless the user configured their own ssh command,
/// which may rely on an agent that batch mode would not disturb anyway. A failed fetch is silent;
/// the next one simply tries again.
pub fn fetch(path: &Path, targets: &[String]) {
    for (remote, branches) in group_by_remote(targets, &remotes(path)) {
        let mut cmd = Command::new("git");
        cmd.args(["fetch", "--quiet", "--no-tags", &remote])
            .args(&branches)
            .current_dir(path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if std::env::var_os("GIT_SSH_COMMAND").is_none()
            && git_output(path, &["config", "core.sshCommand"]).is_empty()
        {
            cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
        let _ = cmd.status();
    }
}

fn remotes(path: &Path) -> Vec<String> {
    git_output(path, &["remote"])
        .lines()
        .map(str::to_string)
        .collect()
}

/// Splits `<remote>/<branch>` names by remote, keeping the order they were given in. A remote name
/// may itself contain `/`, so the longest remote that prefixes the name wins; a name no remote
/// prefixes is not fetchable and is dropped.
fn group_by_remote(targets: &[String], remotes: &[String]) -> Vec<(String, Vec<String>)> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for target in targets {
        let Some((remote, branch)) = remotes
            .iter()
            .filter_map(|remote| {
                let branch = target.strip_prefix(remote.as_str())?.strip_prefix('/')?;
                Some((remote, branch))
            })
            .max_by_key(|(remote, _)| remote.len())
        else {
            continue;
        };
        match groups.iter_mut().find(|(r, _)| r == remote) {
            Some((_, branches)) => branches.push(branch.to_string()),
            None => groups.push((remote.clone(), vec![branch.to_string()])),
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_git::GitRepository;

    #[test]
    fn test_fingerprint_moves_with_refs_and_with_the_worktree_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let git = GitRepository::new(dir.path());
        git.init();
        git.commit("c1", "2024-01-01");

        let before = (fingerprint(dir.path(), false), fingerprint(dir.path(), true));
        git.write_file("new.txt", "x");
        assert_eq!(fingerprint(dir.path(), false), before.0);
        assert_ne!(fingerprint(dir.path(), true), before.1);

        git.commit("c2", "2024-01-02");
        assert_ne!(fingerprint(dir.path(), false), before.0);
    }

    #[test]
    fn test_fingerprint_sees_a_checkout_to_a_branch_on_the_same_commit() {
        let dir = tempfile::tempdir().unwrap();
        let git = GitRepository::new(dir.path());
        git.init();
        git.commit("c1", "2024-01-01");
        git.checkout_b("other");

        let before = fingerprint(dir.path(), false);
        git.checkout("master");
        assert_ne!(fingerprint(dir.path(), false), before);
    }

    #[test]
    fn test_fetch_moves_the_remote_tracking_branch() {
        let origin_dir = tempfile::tempdir().unwrap();
        let origin = GitRepository::new(origin_dir.path());
        origin.init();
        origin.commit("c1", "2024-01-01");

        let clone_dir = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .args(["clone", "--quiet"])
            .arg(origin_dir.path())
            .arg(clone_dir.path())
            .status()
            .unwrap();
        assert!(status.success());

        origin.commit("c2", "2024-01-02");
        let tracking = |path: &Path| git_output(path, &["rev-parse", "origin/master"]);
        assert_ne!(tracking(clone_dir.path()).trim(), origin.rev_parse_head());

        fetch(clone_dir.path(), &strings(&["origin/master"]));
        assert_eq!(tracking(clone_dir.path()).trim(), origin.rev_parse_head());
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn test_group_by_remote_keeps_order_and_drops_unknown_remotes() {
        assert_eq!(
            group_by_remote(
                &strings(&["origin/main", "fork/topic", "origin/feature", "gone/x"]),
                &strings(&["origin", "fork"]),
            ),
            [
                ("origin".to_string(), strings(&["main", "feature"])),
                ("fork".to_string(), strings(&["topic"])),
            ]
        );
    }

    #[test]
    fn test_group_by_remote_prefers_the_longest_remote_name() {
        assert_eq!(
            group_by_remote(&strings(&["team/a/main"]), &strings(&["team", "team/a"])),
            [("team/a".to_string(), strings(&["main"]))]
        );
    }
}
