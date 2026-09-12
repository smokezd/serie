use std::{
    hash::Hash,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use chrono::{DateTime, FixedOffset};
use rustc_hash::FxHashMap;

use crate::Result;

#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitHash(String);

impl CommitHash {
    pub fn as_short_hash(&self) -> &str {
        &self.0[0..7]
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for CommitHash {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[derive(Debug, Default, Clone)]
pub struct Commit {
    pub commit_hash: CommitHash,
    pub author_name: String,
    pub author_email: String,
    pub author_date: DateTime<FixedOffset>,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: DateTime<FixedOffset>,
    pub subject: String,
    pub body: String,
    pub parent_commit_hashes: Vec<CommitHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ref {
    Tag {
        name: String,
        target: CommitHash,
    },
    Branch {
        name: String,
        target: CommitHash,
    },
    RemoteBranch {
        name: String,
        target: CommitHash,
    },
    Stash {
        name: String,
        message: String,
        target: CommitHash,
    },
}

impl Ref {
    pub fn name(&self) -> &str {
        match self {
            Ref::Tag { name, .. } => name,
            Ref::Branch { name, .. } => name,
            Ref::RemoteBranch { name, .. } => name,
            Ref::Stash { name, .. } => name,
        }
    }

    pub fn target(&self) -> &CommitHash {
        match self {
            Ref::Tag { target, .. } => target,
            Ref::Branch { target, .. } => target,
            Ref::RemoteBranch { target, .. } => target,
            Ref::Stash { target, .. } => target,
        }
    }
}

/// What `git merge-base` had to say about the revspec.
///
/// Three states, because two of them used to be one: an absent base meant both "the revspec did
/// not name two revisions" and "the two it named share no history", so unrelated branches were
/// reported as a malformed revspec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeBase {
    /// The revspec named something other than exactly two revisions.
    NotScoped,
    /// Exactly two revisions, with no common ancestor between them.
    UnrelatedHistories,
    Found(CommitHash),
}

#[derive(Debug, Clone)]
pub enum Head {
    Branch { name: String },
    Detached { target: CommitHash },
    None,
}

#[derive(Debug, Clone, Copy)]
pub enum SortCommit {
    Chronological,
    Topological,
}

type CommitMap = FxHashMap<CommitHash, Commit>;
type CommitsMap = FxHashMap<CommitHash, Vec<CommitHash>>;

type RefMap = FxHashMap<CommitHash, Vec<Ref>>;

#[derive(Debug)]
pub struct Repository {
    path: PathBuf,
    commit_map: CommitMap,

    parents_map: CommitsMap,
    children_map: CommitsMap,

    ref_map: RefMap,
    head: Head,
    // to preserve order of the original commits from `git log`, we store the commit hashes
    commit_hashes: Vec<CommitHash>,
    // `Some` only when the revspec named exactly two revisions; see `two_revisions`
    merge_base: MergeBase,
    // The commit each plain revision in the revspec resolves to, in the order given. Empty
    // unless the revspec named two or more of them; see `plain_revisions`.
    revspec_tips: Vec<CommitHash>,
}

impl Repository {
    /// Loads the commits reachable from `revspec`, or from every branch, remote branch, tag and
    /// stash when `revspec` is empty.
    pub fn load(
        path: &Path,
        sort: SortCommit,
        max_count: Option<usize>,
        mailmap: bool,
        revspec: &[String],
    ) -> Result<Self> {
        check_git_repository(path)?;

        let (mut ref_map, head) = load_refs(path);

        // Stashes are reachable only through their own refs, so a scoped log would otherwise
        // include them or not depending on where each stash happens to be based.
        let stashes = if revspec.is_empty() {
            load_all_stashes(path, mailmap)
        } else {
            Vec::new()
        };
        let commits = load_all_commits(path, sort, &head, &stashes, max_count, mailmap, revspec)?;
        if commits.is_empty() {
            return Err(no_commits_error(revspec));
        }

        let commits = merge_stashes_to_commits(commits, stashes);
        let commit_hashes = commits.iter().map(|c| c.commit_hash.clone()).collect();

        let (parents_map, children_map) = build_commits_maps(&commits);
        let commit_map = to_commit_map(commits);

        if revspec.is_empty() {
            let stash_ref_map = load_stashes_as_refs(path);
            merge_ref_maps(&mut ref_map, stash_ref_map);
        } else {
            // `git show-ref` ignores the revspec, so drop the refs that point outside of it to
            // keep the ref list and ref jumps consistent with what is rendered.
            ref_map.retain(|hash, _| commit_map.contains_key(hash));
        }

        let merge_base = match two_revisions(revspec) {
            Some((a, b)) => match load_merge_base(path, a, b) {
                Some(hash) => MergeBase::Found(hash),
                None => MergeBase::UnrelatedHistories,
            },
            None => MergeBase::NotScoped,
        };
        let revspec_tips = load_revspec_tips(path, revspec);

        Ok(Self::new(
            path.to_path_buf(),
            commit_map,
            parents_map,
            children_map,
            ref_map,
            head,
            commit_hashes,
            merge_base,
            revspec_tips,
        ))
    }

    pub fn new(
        path: PathBuf,
        commit_map: CommitMap,
        parents_map: CommitsMap,
        children_map: CommitsMap,
        ref_map: RefMap,
        head: Head,
        commit_hashes: Vec<CommitHash>,
        merge_base: MergeBase,
        revspec_tips: Vec<CommitHash>,
    ) -> Self {
        Self {
            path,
            commit_map,
            parents_map,
            children_map,
            ref_map,
            head,
            commit_hashes,
            merge_base,
            revspec_tips,
        }
    }

    pub fn commit(&self, commit_hash: &CommitHash) -> Option<&Commit> {
        self.commit_map.get(commit_hash)
    }

    pub fn all_commits(&self) -> Vec<&Commit> {
        self.commit_hashes
            .iter()
            .filter_map(|hash| self.commit(hash))
            .collect()
    }

    pub fn parents_hash(&self, commit_hash: &CommitHash) -> Vec<&CommitHash> {
        self.parents_map
            .get(commit_hash)
            .map(|hs| hs.iter().collect::<Vec<&CommitHash>>())
            .unwrap_or_default()
    }

    pub fn children_hash(&self, commit_hash: &CommitHash) -> Vec<&CommitHash> {
        self.children_map
            .get(commit_hash)
            .map(|hs| hs.iter().collect::<Vec<&CommitHash>>())
            .unwrap_or_default()
    }

    pub fn refs(&self, commit_hash: &CommitHash) -> Vec<&Ref> {
        self.ref_map
            .get(commit_hash)
            .map(|refs| refs.iter().collect::<Vec<&Ref>>())
            .unwrap_or_default()
    }

    pub fn all_refs(&self) -> Vec<&Ref> {
        self.ref_map.values().flatten().collect()
    }

    pub fn head(&self) -> &Head {
        &self.head
    }

    /// The commit each plain revision in the revspec resolves to, in the order given. Empty
    /// unless the revspec named two or more; a commit here is not necessarily rendered.
    pub fn revspec_tips(&self) -> &[CommitHash] {
        &self.revspec_tips
    }

    /// The common ancestor of the two revisions the revspec named, if it named exactly two.
    /// The commit is not necessarily rendered: `--max-count` can cut it off.
    pub fn merge_base(&self) -> &MergeBase {
        &self.merge_base
    }

    pub fn commit_detail(&self, commit_hash: &CommitHash) -> (Commit, Vec<FileChange>) {
        let commit = self.commit(commit_hash).unwrap().clone();
        let changes = if commit.parent_commit_hashes.is_empty() {
            get_initial_commit_additions(&self.path, commit_hash)
        } else {
            get_diff_summary(&self.path, commit_hash)
        };
        (commit, changes)
    }
}

fn check_git_repository(path: &Path) -> Result<()> {
    if !is_inside_work_tree(path) && !is_bare_repository(path) {
        let msg = "not a git repository (or any of the parent directories)";
        return Err(msg.into());
    }
    Ok(())
}

/// The revspec elements that `git log` reads as revisions: everything before the first `--`.
///
/// `serie -- main -- README.md` reaches us as `["main", "--", "README.md"]`, and git treats
/// everything past the separator as pathspecs. Scanning the whole slice for revisions would mark a
/// file as a branch tip and rewrite its name, so the separator ends the search here too.
pub fn revision_args(revspec: &[String]) -> &[String] {
    match revspec.iter().position(|arg| arg == "--") {
        Some(i) => &revspec[..i],
        None => revspec,
    }
}

/// A merge base only means something when the revspec names exactly two commits, so a range, an
/// exclusion or a `git log` flag opts out. Revision modifiers (`HEAD~2`, `branch@{1}`) are kept,
/// since each still names a single commit.
///
/// Defined in terms of [`plain_revisions`] so the two cannot disagree: a flag or a range is
/// skipped in both, rather than disqualifying the whole revspec here and being filtered out there.
/// That divergence used to make `serie -- main topic --all` mark both tips while reporting that a
/// merge base needs exactly two revisions.
fn two_revisions(revspec: &[String]) -> Option<(&str, &str)> {
    let [a, b] = plain_revisions(revspec)[..] else {
        return None;
    };
    Some((a, b))
}

/// The revspec elements that name a single commit, in order. A range, an exclusion or a `git log`
/// flag names no single commit, so each is skipped rather than disqualifying the whole revspec.
fn plain_revisions(revspec: &[String]) -> Vec<&str> {
    revision_args(revspec)
        .iter()
        .map(String::as_str)
        .filter(|rev| !(rev.starts_with('-') || rev.starts_with('^') || rev.contains("..")))
        .collect()
}

/// Resolves the plain revisions to commits, once, in one `git rev-parse` call. Returns empty
/// unless at least two revisions resolve, since a single tip is almost always the top row.
fn load_revspec_tips(path: &Path, revspec: &[String]) -> Vec<CommitHash> {
    let revisions = plain_revisions(revspec);
    if revisions.len() < 2 {
        return Vec::new();
    }
    // `^{commit}` peels to the commit the revision names. Without it an annotated tag resolves to
    // its own tag object, which matches no rendered commit, so the tag silently loses its marker
    // while later revisions keep their ordinals. It also makes a non-revision fail loudly instead
    // of being stored as a tip: `rev-parse master a.txt` prints a hash and `a.txt`, which the
    // count check below cannot tell apart from two resolved revisions.
    let peeled: Vec<String> = revisions
        .iter()
        .map(|rev| format!("{rev}^{{commit}}"))
        .collect();
    let output = Command::new("git")
        .arg("rev-parse")
        .args(&peeled)
        .current_dir(path)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(stdout) = String::from_utf8(output.stdout) else {
        return Vec::new();
    };
    let tips: Vec<CommitHash> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(CommitHash::from)
        .collect();
    // `rev-parse` prints one line per argument; anything else means it did not resolve what we
    // asked, so fall back to marking nothing rather than mislabelling the ordinals.
    if tips.len() == revisions.len() {
        tips
    } else {
        Vec::new()
    }
}

fn load_merge_base(path: &Path, a: &str, b: &str) -> Option<CommitHash> {
    let output = Command::new("git")
        .arg("merge-base")
        .arg(a)
        .arg(b)
        .current_dir(path)
        .output()
        .ok()?;
    // Unrelated histories exit non-zero, which is not an error worth failing the launch over.
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8(output.stdout).ok()?;
    let hash = hash.trim();
    (!hash.is_empty()).then(|| CommitHash::from(hash))
}

fn is_inside_work_tree(path: &Path) -> bool {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .current_dir(path)
        .output()
        .unwrap();
    output.status.success() && output.stdout == b"true\n"
}

fn is_bare_repository(path: &Path) -> bool {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--is-bare-repository")
        .current_dir(path)
        .output()
        .unwrap();
    output.status.success() && output.stdout == b"true\n"
}

fn load_all_commits(
    path: &Path,
    sort: SortCommit,
    head: &Head,
    stashes: &[Commit],
    max_count: Option<usize>,
    mailmap: bool,
    revspec: &[String],
) -> Result<Vec<Commit>> {
    let mut cmd = Command::new("git");
    cmd.arg("log");

    cmd.arg(match sort {
        SortCommit::Chronological => "--date-order",
        SortCommit::Topological => "--topo-order",
    })
    .arg(format!("--pretty={}", load_commits_format(mailmap)))
    .arg("--date=iso-strict")
    .arg("-z"); // use NUL as a delimiter

    // Before the revspec, not after it: a revspec may carry its own `--`, and git reads everything
    // past that separator as pathspecs. Appended afterwards, `--max-count` and its value would be
    // taken for two file names and the limit silently dropped.
    if let Some(n) = max_count {
        cmd.arg("--max-count").arg(n.to_string());
    }

    if revspec.is_empty() {
        // exclude stashes and other refs
        cmd.arg("--branches").arg("--remotes").arg("--tags");

        // commits that are reachable from the stashes
        stashes.iter().for_each(|stash| {
            cmd.arg(stash.parent_commit_hashes[0].as_str());
        });

        if !matches!(head, Head::None) {
            cmd.arg("HEAD");
        }
    } else {
        // passed through to git as-is, so any revision, range or pathspec works
        cmd.args(revspec);
    }

    cmd.current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut process = cmd.spawn().unwrap();

    let stdout = process.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut commits = Vec::new();

    for bytes in reader.split(b'\0') {
        let bytes = bytes.unwrap();
        let s = String::from_utf8_lossy(&bytes);

        let parts: Vec<&str> = s.split('\x1f').collect();
        if parts.len() != 10 {
            panic!("unexpected number of parts: {} [{}]", parts.len(), s);
        }

        let commit = Commit {
            commit_hash: parts[0].into(),
            author_name: parts[1].into(),
            author_email: parts[2].into(),
            author_date: parse_iso_date(parts[3]),
            committer_name: parts[4].into(),
            committer_email: parts[5].into(),
            committer_date: parse_iso_date(parts[6]),
            subject: parts[7].into(),
            body: parts[8].into(),
            parent_commit_hashes: parse_parent_commit_hashes(parts[9]),
        };

        commits.push(commit);
    }

    // Drained before waiting: a child that fills the stderr pipe blocks writing to it, and
    // `wait()` would then never return. git only ever writes a line or two here, but the ordering
    // is what makes that a fact about git rather than a thing this code relies on.
    let mut stderr = String::new();
    if let Some(mut pipe) = process.stderr.take() {
        pipe.read_to_string(&mut stderr).ok();
    }

    let status = process.wait().unwrap();
    if !status.success() {
        // git has already told the user exactly what is wrong with their revspec
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("git log failed with {status}").into()
        } else {
            Box::<dyn std::error::Error>::from(stderr.to_string())
        });
    }

    Ok(commits)
}

fn no_commits_error(revspec: &[String]) -> Box<dyn std::error::Error> {
    if revspec.is_empty() {
        "no commits in the repository".into()
    } else {
        format!("no commits match {}", revspec.join(" ")).into()
    }
}

fn load_all_stashes(path: &Path, mailmap: bool) -> Vec<Commit> {
    let mut cmd = Command::new("git")
        .arg("stash")
        .arg("list")
        .arg(format!("--pretty={}", load_commits_format(mailmap)))
        .arg("--date=iso-strict")
        .arg("-z") // use NUL as a delimiter
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut commits = Vec::new();

    for bytes in reader.split(b'\0') {
        let bytes = bytes.unwrap();
        let s = String::from_utf8_lossy(&bytes);

        let parts: Vec<&str> = s.split('\x1f').collect();
        if parts.len() != 10 {
            panic!("unexpected number of parts: {} [{}]", parts.len(), s);
        }

        let commit = Commit {
            commit_hash: parts[0].into(),
            author_name: parts[1].into(),
            author_email: parts[2].into(),
            author_date: parse_iso_date(parts[3]),
            committer_name: parts[4].into(),
            committer_email: parts[5].into(),
            committer_date: parse_iso_date(parts[6]),
            subject: parts[7].into(),
            body: parts[8].into(),
            parent_commit_hashes: parse_parent_commit_hashes(parts[9]),
        };

        commits.push(commit);
    }

    cmd.wait().unwrap();

    commits
}

fn load_commits_format(mailmap: bool) -> String {
    // The uppercase name/email placeholders (`%aN`, `%aE`, `%cN`, `%cE`) resolve
    // identities through the repository's .mailmap, while the lowercase variants
    // use the raw values recorded in each commit.
    let format = if mailmap {
        [
            "%H", "%aN", "%aE", "%ad", "%cN", "%cE", "%cd", "%s", "%b", "%P",
        ]
    } else {
        [
            "%H", "%an", "%ae", "%ad", "%cn", "%ce", "%cd", "%s", "%b", "%P",
        ]
    };
    format.join("%x1f") // use Unit Separator as a delimiter
}

fn parse_iso_date(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn parse_parent_commit_hashes(s: &str) -> Vec<CommitHash> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(' ').map(|s| s.into()).collect()
}

fn build_commits_maps(commits: &Vec<Commit>) -> (CommitsMap, CommitsMap) {
    let mut parents_map: CommitsMap = FxHashMap::default();
    let mut children_map: CommitsMap = FxHashMap::default();
    for commit in commits {
        let hash = &commit.commit_hash;
        for parent_hash in &commit.parent_commit_hashes {
            parents_map
                .entry(hash.clone())
                .or_default()
                .push(parent_hash.clone());
            children_map
                .entry(parent_hash.clone())
                .or_default()
                .push(hash.clone());
        }
    }

    (parents_map, children_map)
}

fn to_commit_map(commits: Vec<Commit>) -> CommitMap {
    commits
        .into_iter()
        .map(|commit| (commit.commit_hash.clone(), commit))
        .collect()
}

fn merge_stashes_to_commits(commits: Vec<Commit>, stashes: Vec<Commit>) -> Vec<Commit> {
    // Stash commit has multiple parent commits, but the first parent commit is the commit that the stash was created from.
    // If the first parent commit is not found, the stash commit is ignored.
    let mut ret = Vec::new();
    let mut statsh_map: FxHashMap<CommitHash, Vec<Commit>> =
        stashes
            .into_iter()
            .fold(FxHashMap::default(), |mut acc, commit| {
                let parent = commit.parent_commit_hashes[0].clone();
                acc.entry(parent).or_default().push(commit);
                acc
            });
    for commit in commits {
        if let Some(stashes) = statsh_map.remove(&commit.commit_hash) {
            for stash in stashes {
                ret.push(stash);
            }
        }
        ret.push(commit);
    }
    ret
}

fn load_refs(path: &Path) -> (RefMap, Head) {
    let mut cmd = Command::new("git")
        .arg("show-ref")
        .arg("--head")
        .arg("--dereference")
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut ref_map = RefMap::default();
    let mut tag_map: FxHashMap<String, Ref> = FxHashMap::default();
    let mut head: Head = Head::None;

    for line in reader.lines() {
        let line = line.unwrap();

        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() != 2 {
            panic!("unexpected number of parts: {} [{}]", parts.len(), line);
        }

        let hash = parts[0];
        let refs = parts[1];

        if refs == "HEAD" {
            head = if let Some(branch) = get_current_branch(path) {
                Head::Branch { name: branch }
            } else {
                Head::Detached {
                    target: hash.into(),
                }
            };
        } else if let Some(r) = parse_branch_refs(hash, refs) {
            ref_map.entry(hash.into()).or_default().push(r);
        } else if let Some(r) = parse_tag_refs(hash, refs) {
            // if annotated tag exists, it will be overwritten by the following line of the same tag
            // this will make the tag point to the commit that the annotated tag points to
            tag_map.insert(r.name().into(), r);
        }
    }

    for tag in tag_map.into_values() {
        ref_map.entry(tag.target().clone()).or_default().push(tag);
    }

    ref_map.values_mut().for_each(|refs| refs.sort());

    cmd.wait().unwrap();

    (ref_map, head)
}

fn load_stashes_as_refs(path: &Path) -> RefMap {
    let format = ["%gd", "%H", "%s"].join("%x1f"); // use Unit Separator as a delimiter
    let mut cmd = Command::new("git")
        .arg("stash")
        .arg("list")
        .arg(format!("--format={format}"))
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut ref_map = RefMap::default();

    for line in reader.lines() {
        let line = line.unwrap();

        let parts: Vec<&str> = line.split('\x1f').collect();
        if parts.len() != 3 {
            panic!("unexpected number of parts: {} [{}]", parts.len(), line);
        }

        let name = parts[0];
        let hash = parts[1];
        let subject = parts[2];

        let r = Ref::Stash {
            name: name.into(),
            message: subject.into(),
            target: hash.into(),
        };

        ref_map.entry(hash.into()).or_default().push(r);
    }

    cmd.wait().unwrap();

    ref_map
}

fn merge_ref_maps(m1: &mut RefMap, m2: RefMap) {
    for (k, v) in m2 {
        m1.entry(k).or_default().extend(v);
    }
}

fn parse_branch_refs(hash: &str, refs: &str) -> Option<Ref> {
    if refs.starts_with("refs/heads/") {
        let name = refs.trim_start_matches("refs/heads/");
        Some(Ref::Branch {
            name: name.into(),
            target: hash.into(),
        })
    } else if refs.starts_with("refs/remotes/") {
        let name = refs.trim_start_matches("refs/remotes/");
        Some(Ref::RemoteBranch {
            name: name.into(),
            target: hash.into(),
        })
    } else {
        None
    }
}

fn parse_tag_refs(hash: &str, refs: &str) -> Option<Ref> {
    if refs.starts_with("refs/tags/") {
        let name = refs.trim_start_matches("refs/tags/");
        let name = name.trim_end_matches("^{}");
        Some(Ref::Tag {
            name: name.into(),
            target: hash.into(),
        })
    } else {
        None
    }
}

fn get_current_branch(path: &Path) -> Option<String> {
    let mut cmd = Command::new("git")
        .arg("branch")
        .arg("--show-current")
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let branch = if let Some(line) = reader.lines().next() {
        line.ok()
    } else {
        None
    };

    cmd.wait().unwrap();

    branch
}

#[derive(Debug)]
pub enum FileChange {
    Add { path: String },
    Modify { path: String },
    Delete { path: String },
    Move { from: String, to: String },
}

pub fn get_diff_summary(path: &Path, commit_hash: &CommitHash) -> Vec<FileChange> {
    let mut cmd = Command::new("git")
        .arg("diff")
        .arg("--name-status")
        .arg(format!("{}^", commit_hash.0))
        .arg(&commit_hash.0)
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut changes = Vec::new();

    for line in reader.lines() {
        let line = line.unwrap();
        let parts: Vec<&str> = line.split('\t').collect();

        match &parts[0][0..1] {
            "A" => changes.push(FileChange::Add {
                path: parts[1].into(),
            }),
            "M" => changes.push(FileChange::Modify {
                path: parts[1].into(),
            }),
            "D" => changes.push(FileChange::Delete {
                path: parts[1].into(),
            }),
            "R" => changes.push(FileChange::Move {
                from: parts[1].into(),
                to: parts[2].into(),
            }),
            _ => {}
        }
    }

    cmd.wait().unwrap();

    changes
}

pub fn get_initial_commit_additions(path: &Path, commit_hash: &CommitHash) -> Vec<FileChange> {
    let mut cmd = Command::new("git")
        .arg("ls-tree")
        .arg("--name-status")
        .arg("-r") // the empty tree hash
        .arg(&commit_hash.0)
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut changes = Vec::new();

    for line in reader.lines() {
        let line = line.unwrap();
        changes.push(FileChange::Add { path: line });
    }

    cmd.wait().unwrap();

    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revspec(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn test_two_revisions_accepts_a_pair_of_plain_revisions() {
        assert_eq!(
            two_revisions(&revspec(&["master", "topic"])),
            Some(("master", "topic"))
        );
        // Revision modifiers still name a single commit each.
        assert_eq!(
            two_revisions(&revspec(&["HEAD~2", "origin/master"])),
            Some(("HEAD~2", "origin/master"))
        );
    }

    #[test]
    fn test_two_revisions_requires_exactly_two() {
        assert_eq!(two_revisions(&revspec(&[])), None);
        assert_eq!(two_revisions(&revspec(&["master"])), None);
        assert_eq!(two_revisions(&revspec(&["a", "b", "c"])), None);
    }

    #[test]
    fn test_plain_revisions_keeps_only_what_names_a_single_commit() {
        assert_eq!(
            plain_revisions(&revspec(&["master", "topic"])),
            vec!["master", "topic"]
        );
        // A flag or a range is skipped rather than disqualifying the rest.
        assert_eq!(
            plain_revisions(&revspec(&["master", "--all", "topic"])),
            vec!["master", "topic"]
        );
        assert_eq!(
            plain_revisions(&revspec(&["a..b", "^c", "topic"])),
            vec!["topic"]
        );
        assert!(plain_revisions(&revspec(&["--all"])).is_empty());
    }

    #[test]
    fn test_two_revisions_rejects_ranges_exclusions_and_flags() {
        assert_eq!(two_revisions(&revspec(&["a..b", "c"])), None);
        assert_eq!(two_revisions(&revspec(&["a", "b...c"])), None);
        assert_eq!(two_revisions(&revspec(&["^a", "b"])), None);
        assert_eq!(two_revisions(&revspec(&["--all", "b"])), None);
    }
}
