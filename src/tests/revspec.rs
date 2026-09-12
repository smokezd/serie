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

#[test]
fn refresh_keeps_a_hidden_graph_from_failing_a_narrow_refresh() {
    use crate::{refresh_graph_width, GraphWidthType};

    // Startup has no refresh context, so the configured width is used as-is.
    assert_eq!(
        refresh_graph_width(Some(GraphWidthType::Auto), None),
        Some(GraphWidthType::Auto)
    );
    // A visible graph still has to satisfy the terminal-width check on refresh.
    assert_eq!(
        refresh_graph_width(Some(GraphWidthType::Double), Some(true)),
        Some(GraphWidthType::Double)
    );
    // A graph hidden at runtime refreshes as `Hidden`, the one width that never errors.
    assert_eq!(
        refresh_graph_width(Some(GraphWidthType::Double), Some(false)),
        Some(GraphWidthType::Hidden)
    );
    assert_eq!(
        refresh_graph_width(None, Some(false)),
        Some(GraphWidthType::Hidden)
    );
}

#[test]
fn merge_base_ignores_flags_the_way_tip_marking_does() -> TestResult {
    let dir = tempfile::tempdir()?;
    let git = init_branched_repository(dir.path());
    git.checkout("master");

    // Both halves of the revspec feature have to agree that `--all` names no revision. When they
    // disagreed, this invocation marked both tips and still reported "needs exactly two revisions".
    let with_flag = load(dir.path(), &["master", "feature", "--all"])?;
    let without_flag = load(dir.path(), &["master", "feature"])?;

    assert!(matches!(with_flag.merge_base(), git::MergeBase::Found(_)));
    assert_eq!(with_flag.merge_base(), without_flag.merge_base());
    assert_eq!(with_flag.revspec_tips().len(), 2);

    // A range still names no single commit, so one plain revision is left and there is no base.
    let range = load(dir.path(), &["master", "master..feature"])?;
    assert_eq!(range.merge_base(), &git::MergeBase::NotScoped);
    assert!(range.revspec_tips().is_empty());
    Ok(())
}

#[test]
fn annotated_tags_resolve_to_the_commit_they_point_at() -> TestResult {
    let dir = tempfile::tempdir()?;
    let git = init_branched_repository(dir.path());
    git.checkout("master");
    git.tag_a("v1", "2024-01-04");

    // `git rev-parse v1` prints the tag object; only `v1^{commit}` reaches the commit, so without
    // peeling the tag matches no rendered commit and loses its marker.
    let repository = load(dir.path(), &["v1", "feature"])?;
    let tips = repository.revspec_tips();

    assert_eq!(tips.len(), 2);
    let head_of_master = git.rev_parse_head();
    assert_eq!(tips[0].as_str(), head_of_master);
    Ok(())
}

#[test]
fn a_pathspec_is_neither_a_tip_nor_renamed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let git = GitRepository::new(dir.path());
    git.init();
    git.commit_file("head", "h1", "2024-01-01");
    git.commit("m2", "2024-01-02");

    // `head` here is a file, not a revision: it must not be rewritten to `HEAD`, and it must not
    // be counted as a revision when deciding tips.
    let revspec = normalize_revspec(vec![
        "master".to_string(),
        "--".to_string(),
        "head".to_string(),
    ]);
    assert_eq!(revspec, ["master", "--", "head"]);

    let repository = load(dir.path(), &["master", "--", "head"])?;
    assert_eq!(subjects(&repository), ["h1"]);
    assert!(repository.revspec_tips().is_empty());
    assert_eq!(repository.merge_base(), &git::MergeBase::NotScoped);
    Ok(())
}

#[test]
fn max_count_survives_a_pathspec_separator() -> TestResult {
    let dir = tempfile::tempdir()?;
    let git = GitRepository::new(dir.path());
    git.init();
    git.commit_file("a.txt", "a1", "2024-01-01");
    git.commit_file("a.txt", "a2", "2024-01-02");

    // Appended after the revspec, `--max-count 1` landed past the `--` and git read it as two more
    // pathspecs, silently dropping the limit.
    let revspec: Vec<String> = ["master", "--", "a.txt"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let repository = Repository::load(
        dir.path(),
        git::SortCommit::Chronological,
        Some(1),
        false,
        &revspec,
    )?;

    assert_eq!(repository.all_commits().len(), 1);
    Ok(())
}

#[test]
fn unrelated_histories_are_distinguished_from_an_unscoped_revspec() -> TestResult {
    let dir = tempfile::tempdir()?;
    let git = GitRepository::new(dir.path());
    git.init();
    git.commit("m1", "2024-01-01");
    git.checkout_orphan("other");
    git.commit("o1", "2024-01-02");

    // Two revisions were given, so this is not "needs exactly two revisions" — the histories
    // simply share no commit, and the status line has to say which of the two happened.
    let repository = load(dir.path(), &["master", "other"])?;
    assert_eq!(repository.merge_base(), &git::MergeBase::UnrelatedHistories);
    Ok(())
}
