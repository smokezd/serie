use std::path::Path;

use clap::Parser;

use crate::{
    git::{self, Repository},
    test_git::GitRepository,
    Args,
};

fn load(repo_path: &Path, include_uncommitted: bool) -> Repository {
    load_revspec(repo_path, &[], include_uncommitted)
}

fn load_revspec(repo_path: &Path, revspec: &[&str], include_uncommitted: bool) -> Repository {
    let revspec: Vec<String> = revspec.iter().map(|s| s.to_string()).collect();
    git::Repository::load(
        repo_path,
        git::SortCommit::Chronological,
        None,
        false,
        &revspec,
        include_uncommitted,
    )
    .unwrap()
}

fn subjects(repository: &Repository) -> Vec<String> {
    repository
        .all_commits()
        .iter()
        .map(|c| c.subject.clone())
        .collect()
}

#[test]
fn uncommitted_flag_off_adds_no_pseudo_rows_even_when_dirty() {
    let dir = tempfile::tempdir().unwrap();
    let git = GitRepository::new(dir.path());
    git.init();
    git.commit_file("tracked.txt", "base", "2024-01-01");
    git.write_file("tracked.txt", "unstaged edit");

    let repository = load(dir.path(), false);

    assert_eq!(subjects(&repository), ["base"]);
}

#[test]
fn uncommitted_flag_on_but_clean_worktree_adds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let git = GitRepository::new(dir.path());
    git.init();
    git.commit_file("tracked.txt", "base", "2024-01-01");

    let repository = load(dir.path(), true);

    assert_eq!(subjects(&repository), ["base"]);
}

#[test]
fn uncommitted_flag_chains_unstaged_on_staged_on_head_as_a_single_lane() {
    let dir = tempfile::tempdir().unwrap();
    let git = GitRepository::new(dir.path());
    git.init();
    git.commit_file("tracked.txt", "base", "2024-01-01");
    git.stage_file("staged.txt", "staged content"); // new staged file
    git.write_file("untracked.txt", "new"); // untracked counts as unstaged

    let repository = load(dir.path(), true);
    let head_hash = git.rev_parse_head();
    let commits = repository.all_commits();

    assert_eq!(commits.len(), 3);
    assert_eq!(commits[0].subject, "Unstaged changes (1 file)");
    assert_eq!(commits[1].subject, "Staged changes (1 file)");
    assert_eq!(commits[2].commit_hash.as_str(), head_hash);

    // A single lane, not a fork off HEAD: unstaged parents on staged, staged parents on HEAD.
    assert_eq!(
        commits[0].parent_commit_hashes[0].as_str(),
        git::STAGED_COMMIT_HASH
    );
    assert_eq!(commits[1].parent_commit_hashes[0].as_str(), head_hash);
}

#[test]
fn unstaged_parents_directly_on_head_when_nothing_is_staged() {
    let dir = tempfile::tempdir().unwrap();
    let git = GitRepository::new(dir.path());
    git.init();
    git.commit_file("tracked.txt", "base", "2024-01-01");
    git.write_file("tracked.txt", "unstaged edit"); // unstaged only, nothing staged

    let repository = load(dir.path(), true);
    let head_hash = git.rev_parse_head();
    let commits = repository.all_commits();

    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].subject, "Unstaged changes (1 file)");
    assert_eq!(commits[0].parent_commit_hashes[0].as_str(), head_hash);
}

#[test]
fn uncommitted_flag_skips_silently_when_head_is_scoped_out_of_the_revspec() {
    let dir = tempfile::tempdir().unwrap();
    let git = GitRepository::new(dir.path());
    git.init();
    git.commit_file("m1.txt", "m1", "2024-01-01");
    git.commit_file("m2.txt", "m2", "2024-01-02"); // this becomes HEAD
    git.write_file("m2.txt", "unstaged edit"); // dirty relative to the real HEAD

    // Renders only the older commit; the real HEAD (m2) never appears in the set.
    let repository = load_revspec(dir.path(), &["HEAD~1"], true);

    assert_eq!(subjects(&repository), ["m1"]);
}

#[test]
fn uncommitted_flag_still_applies_when_revspec_includes_head() {
    let dir = tempfile::tempdir().unwrap();
    let git = GitRepository::new(dir.path());
    git.init();
    git.commit_file("tracked.txt", "base", "2024-01-01");
    git.write_file("tracked.txt", "unstaged edit");

    let repository = load_revspec(dir.path(), &["HEAD"], true);

    assert_eq!(subjects(&repository), ["Unstaged changes (1 file)", "base"]);
}

#[test]
fn commit_detail_reports_staged_and_unstaged_file_changes_separately() {
    let dir = tempfile::tempdir().unwrap();
    let git = GitRepository::new(dir.path());
    git.init();
    git.commit_file("tracked.txt", "base", "2024-01-01");
    git.stage_file("staged.txt", "staged content");
    git.write_file("tracked.txt", "unstaged edit");
    git.write_file("untracked.txt", "new");

    let repository = load(dir.path(), true);
    let commits = repository.all_commits();
    let unstaged = commits
        .iter()
        .find(|c| c.subject.starts_with("Unstaged"))
        .unwrap();
    let staged = commits
        .iter()
        .find(|c| c.subject.starts_with("Staged"))
        .unwrap();

    let (_, staged_changes) = repository.commit_detail(&staged.commit_hash);
    let (_, unstaged_changes) = repository.commit_detail(&unstaged.commit_hash);

    assert_eq!(staged_changes.len(), 1); // staged.txt add
    assert_eq!(unstaged_changes.len(), 2); // tracked.txt modify + untracked.txt add
}

#[test]
fn neither_uncommitted_flag_is_set_by_default() {
    // Both unset is what lets the config setting be reached at all.
    let args = Args::try_parse_from(["serie"]).unwrap();

    assert!(!args.uncommitted);
    assert!(!args.no_uncommitted);
}

#[test]
fn each_uncommitted_flag_sets_only_itself() {
    let on = Args::try_parse_from(["serie", "--uncommitted"]).unwrap();
    assert!(on.uncommitted);
    assert!(!on.no_uncommitted);

    let off = Args::try_parse_from(["serie", "--no-uncommitted"]).unwrap();
    assert!(off.no_uncommitted);
    assert!(!off.uncommitted);
}

#[test]
fn the_last_uncommitted_flag_given_wins() {
    // `overrides_with` clears the other one, so the pair can never both be set and the config is
    // skipped either way.
    let off = Args::try_parse_from(["serie", "--uncommitted", "--no-uncommitted"]).unwrap();
    assert!(off.no_uncommitted);
    assert!(!off.uncommitted);

    let on = Args::try_parse_from(["serie", "--no-uncommitted", "--uncommitted"]).unwrap();
    assert!(on.uncommitted);
    assert!(!on.no_uncommitted);
}

#[test]
fn is_uncommitted_pseudo_only_matches_the_two_sentinels() {
    let real: git::CommitHash = "deadbeef".into();
    let staged: git::CommitHash = git::STAGED_COMMIT_HASH.into();
    let unstaged: git::CommitHash = git::UNSTAGED_COMMIT_HASH.into();

    assert!(!real.is_uncommitted_pseudo());
    assert!(staged.is_uncommitted_pseudo());
    assert!(unstaged.is_uncommitted_pseudo());
}
