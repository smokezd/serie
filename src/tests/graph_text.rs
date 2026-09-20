//! Golden tests for the text graph renderers.
//!
//! The image renderer is asserted against generated PNGs under `tests/graph/`, which can only
//! show that *something* changed. A text graph is a string, so these assert the exact glyphs and
//! catch edge-combining mistakes the image tests cannot surface.

use chrono::{TimeZone, Utc};

use crate::{
    git::{self, Repository},
    graph::{build_graph_row_text, calc_graph, text_graph_width, TextStyle},
    test_git::GitRepository,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const DATE: &str = "2024-01-01";

/// Renders every row of the graph, one line per commit, trailing blanks trimmed.
fn render(repo_path: &std::path::Path, style: TextStyle) -> String {
    let repository = Repository::load(
        repo_path,
        git::SortCommit::Chronological,
        None,
        false,
        &[],
        false,
    )
    .unwrap();
    let graph = calc_graph(&repository);
    graph
        .commits
        .iter()
        .map(|commit| {
            let cells = build_graph_row_text(&graph, style, &commit.commit_hash);
            let row: String = cells
                .iter()
                .map(|cell| cell.map(|c| c.symbol).unwrap_or(' '))
                .collect();
            row.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn dated(git: &GitRepository, message: &str, day: u32) {
    let date = Utc
        .with_ymd_and_hms(2024, 1, day, 1, 2, 3)
        .unwrap()
        .format("%Y-%m-%d")
        .to_string();
    git.commit(message, &date);
}

#[test]
fn test_straight_history_is_one_lane() -> TestResult {
    let dir = tempfile::tempdir()?;
    let git = &GitRepository::new(dir.path());
    git.init();
    for i in 1..=3 {
        dated(git, &format!("{i:03}"), i);
    }

    assert_eq!(
        render(dir.path(), TextStyle::Unicode),
        ["●", "●", "●"].join("\n")
    );
    assert_eq!(
        render(dir.path(), TextStyle::Ascii),
        ["*", "*", "*"].join("\n")
    );
    Ok(())
}

#[test]
fn test_a_branch_and_merge_render_as_orthogonal_turns() -> TestResult {
    let dir = tempfile::tempdir()?;
    let git = &GitRepository::new(dir.path());
    git.init();
    dated(git, "001", 1);
    git.checkout_b("topic");
    dated(git, "002", 2);
    git.checkout("master");
    dated(git, "003", 3);
    git.merge(&["topic"], DATE);

    // `git log --graph` spends a row on each turn (`|\`, `|/`); serie folds the turn into the
    // commit row it belongs to, so the same topology is four rows instead of six.
    assert_eq!(
        render(dir.path(), TextStyle::Unicode),
        [
            "\u{25cf}\u{2500}\u{256e}", // merge: turns down into the topic lane
            "\u{25cf} \u{2502}",        // 003 on master, topic lane passing through
            "\u{2502} \u{25cf}",        // 002 on topic
            "\u{25cf}\u{2500}\u{256f}", // 001: the topic lane rejoins and turns up
        ]
        .join("\n")
    );
    assert_eq!(
        render(dir.path(), TextStyle::Ascii),
        ["*-+", "* |", "| *", "*-+"].join("\n")
    );
    Ok(())
}

#[test]
fn test_ascii_style_never_emits_a_non_ascii_glyph() -> TestResult {
    let dir = tempfile::tempdir()?;
    let git = &GitRepository::new(dir.path());
    git.init();
    dated(git, "001", 1);
    git.checkout_b("topic");
    dated(git, "002", 2);
    git.checkout("master");
    dated(git, "003", 3);
    git.merge(&["topic"], DATE);

    let ascii = render(dir.path(), TextStyle::Ascii);
    assert!(
        ascii.is_ascii(),
        "ascii style leaked a wide glyph:\n{ascii}"
    );
    Ok(())
}

#[test]
fn test_lanes_stay_aligned_across_rows() -> TestResult {
    let dir = tempfile::tempdir()?;
    let git = &GitRepository::new(dir.path());
    git.init();
    dated(git, "001", 1);
    git.checkout_b("topic");
    dated(git, "002", 2);
    git.checkout("master");
    dated(git, "003", 3);

    let repository = Repository::load(
        dir.path(),
        git::SortCommit::Chronological,
        None,
        false,
        &[],
        false,
    )?;
    let graph = calc_graph(&repository);
    let width = text_graph_width(&graph);
    for commit in &graph.commits {
        let cells = build_graph_row_text(&graph, TextStyle::Unicode, &commit.commit_hash);
        assert_eq!(
            cells.len(),
            width,
            "every row shares one grid, so widths must match"
        );
    }
    Ok(())
}
