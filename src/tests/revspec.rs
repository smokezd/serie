use std::path::Path;

use clap::Parser;
use rstest::rstest;

use crate::{
    git::{self, Ref, Repository},
    normalize_revspec,
    test_git::GitRepository,
    Args,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

// master: m1 -> m2
// feature: m1 -> f1
fn init_branched_repository(repo_path: &Path) -> GitRepository<'_> {
    let git = GitRepository::new(repo_path);
    git.init();
    git.commit("m1", "2024-01-01");
    git.checkout_b("feature");
    git.commit("f1", "2024-01-02");
    git.checkout("master");
    git.commit("m2", "2024-01-03");
    git
}

fn load(repo_path: &Path, revspec: &[&str]) -> Result<Repository, Box<dyn std::error::Error>> {
    let revspec: Vec<String> = revspec.iter().map(|s| s.to_string()).collect();
    Repository::load(
        repo_path,
        git::SortCommit::Chronological,
        None,
        false,
        &revspec,
    )
}

fn subjects(repository: &Repository) -> Vec<String> {
    let mut subjects: Vec<String> = repository
        .all_commits()
        .iter()
        .map(|c| c.subject.clone())
        .collect();
    subjects.sort();
    subjects
}

fn ref_names(repository: &Repository) -> Vec<String> {
    let mut names: Vec<String> = repository
        .all_refs()
        .iter()
        .map(|r| r.name().to_string())
        .collect();
    names.sort();
    names
}

#[test]
fn no_revspec_loads_every_branch() -> TestResult {
    let dir = tempfile::tempdir()?;
    init_branched_repository(dir.path());

    let repository = load(dir.path(), &[])?;

    assert_eq!(subjects(&repository), ["f1", "m1", "m2"]);

    Ok(())
}

#[test]
fn revspec_limits_commits_to_the_given_revision() -> TestResult {
    let dir = tempfile::tempdir()?;
    init_branched_repository(dir.path());

    let repository = load(dir.path(), &["master"])?;

    assert_eq!(subjects(&repository), ["m1", "m2"]);

    Ok(())
}

#[test]
fn revspec_unions_multiple_revisions() -> TestResult {
    let dir = tempfile::tempdir()?;
    init_branched_repository(dir.path());

    let repository = load(dir.path(), &["master", "feature"])?;

    assert_eq!(subjects(&repository), ["f1", "m1", "m2"]);

    Ok(())
}

#[test]
fn revspec_accepts_ranges() -> TestResult {
    let dir = tempfile::tempdir()?;
    init_branched_repository(dir.path());

    let repository = load(dir.path(), &["master..feature"])?;

    assert_eq!(subjects(&repository), ["f1"]);

    Ok(())
}

#[test]
fn revspec_accepts_head() -> TestResult {
    let dir = tempfile::tempdir()?;
    let git = init_branched_repository(dir.path());
    git.checkout("feature");

    let repository = load(dir.path(), &["HEAD"])?;

    assert_eq!(subjects(&repository), ["f1", "m1"]);

    Ok(())
}

#[test]
fn revspec_drops_refs_outside_the_loaded_commits() -> TestResult {
    let dir = tempfile::tempdir()?;
    init_branched_repository(dir.path());

    let repository = load(dir.path(), &["master"])?;

    assert_eq!(ref_names(&repository), ["master"]);

    Ok(())
}

#[test]
fn no_revspec_keeps_every_ref() -> TestResult {
    let dir = tempfile::tempdir()?;
    init_branched_repository(dir.path());

    let repository = load(dir.path(), &[])?;

    assert_eq!(ref_names(&repository), ["feature", "master"]);

    Ok(())
}

#[test]
fn no_revspec_loads_stashes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let git = init_branched_repository(dir.path());
    git.stash("2024-01-04");

    let repository = load(dir.path(), &[])?;

    assert!(repository
        .all_refs()
        .iter()
        .any(|r| matches!(r, Ref::Stash { .. })));

    Ok(())
}

#[test]
fn revspec_excludes_stashes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let git = init_branched_repository(dir.path());
    git.stash("2024-01-04");

    let repository = load(dir.path(), &["master"])?;

    assert!(!repository
        .all_refs()
        .iter()
        .any(|r| matches!(r, Ref::Stash { .. })));
    assert_eq!(subjects(&repository), ["m1", "m2"]);

    Ok(())
}

#[test]
fn unknown_revspec_reports_the_git_error() -> TestResult {
    let dir = tempfile::tempdir()?;
    init_branched_repository(dir.path());

    let error = load(dir.path(), &["no-such-ref"]).unwrap_err().to_string();

    assert!(
        error.contains("unknown revision"),
        "unexpected error: {error}"
    );

    Ok(())
}

#[test]
fn empty_revspec_range_reports_no_match() -> TestResult {
    let dir = tempfile::tempdir()?;
    init_branched_repository(dir.path());

    let error = load(dir.path(), &["master..master"])
        .unwrap_err()
        .to_string();

    assert_eq!(error, "no commits match master..master");

    Ok(())
}

#[rstest]
#[case("head", "HEAD")]
#[case("Head", "HEAD")]
#[case("HEAD", "HEAD")]
#[case("head~2", "HEAD~2")]
#[case("head^", "HEAD^")]
#[case("head^2", "HEAD^2")]
#[case("head@{1}", "HEAD@{1}")]
#[case("head:README.md", "HEAD:README.md")]
#[case("^head", "^HEAD")]
#[case("^head~1", "^HEAD~1")]
#[case("head..master", "HEAD..master")]
#[case("master..head", "master..HEAD")]
#[case("master...head", "master...HEAD")]
#[case("head~2..head", "HEAD~2..HEAD")]
#[case("..head", "..HEAD")]
#[case("master", "master")]
#[case("heads/master", "heads/master")]
#[case("header", "header")]
#[case("refs/heads/head", "refs/heads/head")]
#[case("TT-001-passthrough-http-head", "TT-001-passthrough-http-head")]
#[case("TT-001-passthrough-http-head~1", "TT-001-passthrough-http-head~1")]
#[case("feature/head", "feature/head")]
#[case("head-2", "head-2")]
#[case(
    "head..TT-001-passthrough-http-head",
    "HEAD..TT-001-passthrough-http-head"
)]
#[case("v1.0.0", "v1.0.0")]
#[case("--first-parent", "--first-parent")]
fn lowercase_head_is_normalized(#[case] revspec: &str, #[case] expected: &str) {
    assert_eq!(normalize_revspec(vec![revspec.to_string()]), [expected]);
}

#[test]
fn revspec_is_parsed_as_positional_arguments() {
    let args = Args::try_parse_from(["serie", "master", "feature"]).unwrap();

    assert_eq!(args.revspec, ["master", "feature"]);
}

#[test]
fn revspec_accepts_exclusions_and_ranges() {
    let args = Args::try_parse_from(["serie", "^master", "feature", "a..b"]).unwrap();

    assert_eq!(args.revspec, ["^master", "feature", "a..b"]);
}

#[test]
fn git_flags_are_forwarded_after_a_double_hyphen() {
    let args = Args::try_parse_from(["serie", "--", "--first-parent", "master"]).unwrap();

    assert_eq!(args.revspec, ["--first-parent", "master"]);
}

#[test]
fn pathspec_reaches_git_without_the_double_hyphen() {
    // clap consumes the first `--`, so git receives `git log master README.md`
    let args = Args::try_parse_from(["serie", "master", "--", "README.md"]).unwrap();

    assert_eq!(args.revspec, ["master", "README.md"]);
}

#[test]
fn options_are_parsed_before_and_after_the_revspec() {
    let before = Args::try_parse_from(["serie", "-n", "10", "master"]).unwrap();
    let after = Args::try_parse_from(["serie", "master", "-n", "10"]).unwrap();

    assert_eq!(before.max_count, Some(10));
    assert_eq!(before.revspec, ["master"]);
    assert_eq!(after.max_count, Some(10));
    assert_eq!(after.revspec, ["master"]);
}
