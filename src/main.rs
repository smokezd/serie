mod app;
mod check;
mod color;
mod config;
mod event;
mod external;
mod git;
mod graph;
mod keybind;
mod protocol;
mod search;
mod view;
mod widget;

#[cfg(test)]
#[path = "tests/graph.rs"]
mod graph_tests;

#[cfg(test)]
#[path = "tests/mailmap.rs"]
mod mailmap_tests;

#[cfg(test)]
#[path = "tests/git.rs"]
mod test_git;

#[cfg(test)]
#[path = "tests/revspec.rs"]
mod revspec_tests;

#[cfg(test)]
#[path = "tests/graph_text.rs"]
mod graph_text_tests;

use std::{path::Path, rc::Rc};

use app::{App, Ret};
use clap::{Parser, ValueEnum};
use graph::GraphImageManager;
use serde::Deserialize;

/// Serie - A rich git commit graph in your terminal, like magic 📚
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Maximum number of commits to render
    #[arg(short = 'n', long, value_name = "NUMBER")]
    max_count: Option<usize>,

    /// Image protocol to render graph [default: auto]
    #[arg(short, long, value_name = "TYPE")]
    protocol: Option<ImageProtocolType>,

    /// Commit ordering algorithm [default: chrono]
    #[arg(short, long, value_name = "TYPE")]
    order: Option<CommitOrderType>,

    /// Commit graph image cell width; only `hidden` applies to a text style [default: auto]
    #[arg(short, long, value_name = "TYPE")]
    graph_width: Option<GraphWidthType>,

    /// Commit graph style: image edges, or text [default: rounded]
    #[arg(short = 's', long, value_name = "TYPE")]
    graph_style: Option<GraphStyle>,

    /// Initial selection of commit [default: latest]
    #[arg(short, long, value_name = "TYPE")]
    initial_selection: Option<InitialSelection>,

    /// Revisions to render, passed to `git log` as-is [default: all branches, remotes and tags]
    #[arg(value_name = "REVSPEC", num_args = 0..)]
    revspec: Vec<String>,
}

/// `HEAD` is spelled in upper case in git, and only resolves in lower case on case-insensitive
/// file systems, so accept `head` everywhere rather than only on some machines.
fn normalize_revspec(revspec: Vec<String>) -> Vec<String> {
    // Everything from the first `--` onward is a pathspec to git, not a revision, so a file named
    // `head` must keep its name. `git::revision_args` draws the same line for tip and merge-base
    // detection, so both halves of the revspec agree on where revisions stop.
    let revisions = git::revision_args(&revspec).len();
    revspec
        .iter()
        .enumerate()
        .map(|(i, rev)| {
            if i < revisions {
                normalize_rev(rev)
            } else {
                rev.to_string()
            }
        })
        .collect()
}

/// Rewrites `head` wherever it names a revision, including both ends of a range and revisions
/// carrying `~`, `^`, `@` or `:` suffixes: `head~2`, `^head`, `head..main`.
fn normalize_rev(rev: &str) -> String {
    if rev.starts_with('-') {
        // an option for git itself, which never names a revision
        return rev.to_string();
    }

    let mut normalized = String::with_capacity(rev.len());
    let mut rest = rev;
    loop {
        let (revision, range_operator, remainder) = match rest.find("..") {
            Some(i) => {
                let operator_len = if rest[i..].starts_with("...") { 3 } else { 2 };
                (
                    &rest[..i],
                    &rest[i..i + operator_len],
                    &rest[i + operator_len..],
                )
            }
            None => (rest, "", ""),
        };

        normalized.push_str(&normalize_revision(revision));
        normalized.push_str(range_operator);

        if range_operator.is_empty() {
            break;
        }
        rest = remainder;
    }
    normalized
}

fn normalize_revision(revision: &str) -> String {
    // `^` means exclusion in front of a revision and a parent reference behind it
    let (exclusion, rest) = match revision.strip_prefix('^') {
        Some(rest) => ("^", rest),
        None => ("", revision),
    };
    let name_len = rest.find(['~', '^', '@', ':']).unwrap_or(rest.len());
    let (name, suffix) = rest.split_at(name_len);

    if name.eq_ignore_ascii_case("head") {
        format!("{exclusion}HEAD{suffix}")
    } else {
        revision.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ImageProtocolType {
    Auto,
    Iterm,
    Kitty,
    KittyUnicode,
}

impl From<Option<ImageProtocolType>> for protocol::ImageProtocol {
    fn from(protocol: Option<ImageProtocolType>) -> Self {
        match protocol {
            Some(ImageProtocolType::Auto) => protocol::auto_detect(),
            Some(ImageProtocolType::Iterm) => protocol::ImageProtocol::Iterm2,
            Some(ImageProtocolType::Kitty) => protocol::ImageProtocol::Kitty,
            Some(ImageProtocolType::KittyUnicode) => protocol::ImageProtocol::KittyUnicode {
                tmux: protocol::detect_tmux(),
            },
            None => protocol::auto_detect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CommitOrderType {
    Chrono,
    Topo,
}

impl From<Option<CommitOrderType>> for git::SortCommit {
    fn from(order: Option<CommitOrderType>) -> Self {
        match order {
            Some(CommitOrderType::Chrono) => git::SortCommit::Chronological,
            Some(CommitOrderType::Topo) => git::SortCommit::Topological,
            None => git::SortCommit::Chronological,
        }
    }
}

/// The display a refresh falls back to when the terminal is too narrow for a graph that is not
/// being shown anyway.
///
/// A graph hidden by `graph_toggle` must not keep a refresh from succeeding: a terminal too narrow
/// for a graph nobody is looking at is no more an error here than it is at startup for
/// `-g hidden`. Only the error is swallowed — the configured `-g single` / `-g double` is left to
/// the normal decision, so a width the user asked for survives a refresh rather than quietly
/// reverting to `auto`.
fn hidden_refresh_display(restored_graph_visible: Option<bool>) -> Option<check::GraphDisplay> {
    matches!(restored_graph_visible, Some(false)).then_some(check::GraphDisplay {
        cell_width_type: graph::CellWidthType::Single,
        visible: false,
        toggleable: false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
enum GraphWidthType {
    Auto,
    Double,
    Single,
    /// Do not render the graph column at all. `graph_toggle` brings it back at `Auto` width.
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
enum GraphStyle {
    Rounded,
    Angular,
    /// Draw the graph with strictly ASCII characters instead of images.
    Ascii,
    /// Draw the graph with box-drawing characters instead of images.
    Unicode,
}

impl From<Option<GraphStyle>> for graph::GraphRenderer {
    fn from(style: Option<GraphStyle>) -> Self {
        match style {
            Some(GraphStyle::Rounded) | None => {
                graph::GraphRenderer::Image(graph::GraphStyle::Rounded)
            }
            Some(GraphStyle::Angular) => graph::GraphRenderer::Image(graph::GraphStyle::Angular),
            Some(GraphStyle::Ascii) => graph::GraphRenderer::Text(graph::TextStyle::Ascii),
            Some(GraphStyle::Unicode) => graph::GraphRenderer::Text(graph::TextStyle::Unicode),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
enum InitialSelection {
    Latest,
    Head,
}

impl From<Option<InitialSelection>> for app::InitialSelection {
    fn from(selection: Option<InitialSelection>) -> Self {
        match selection {
            Some(InitialSelection::Latest) => app::InitialSelection::Latest,
            Some(InitialSelection::Head) => app::InitialSelection::Head,
            None => app::InitialSelection::Latest,
        }
    }
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let args = Args::parse();
    let (core_config, ui_config, graph_config, color_theme, keybind_patch) = config::load()?;
    let keybind = keybind::KeyBind::new(keybind_patch);

    let max_count = args.max_count;
    let image_protocol = args.protocol.or(core_config.option.protocol).into();
    let order = args.order.or(core_config.option.order).into();
    let graph_width = args.graph_width.or(core_config.option.graph_width);
    let graph_renderer: graph::GraphRenderer =
        args.graph_style.or(core_config.option.graph_style).into();
    let graph_image_width_mode = graph_config.row_image_width;
    let initial_selection = args
        .initial_selection
        .or(core_config.option.initial_selection)
        .into();
    let mailmap = core_config.git.mailmap;
    let revspec = normalize_revspec(args.revspec);

    let graph_color_set = color::GraphColorSet::new(&graph_config.color);

    let ctx = Rc::new(app::AppContext {
        keybind,
        core_config,
        ui_config,
        color_theme,
        image_protocol,
        revspec_label: (!revspec.is_empty()).then(|| revspec.join(" ")),
    });

    let ec = event::EventController::init();
    let mut refresh_view_context: Option<view::RefreshViewContext> = None;
    let mut terminal = None;

    // Every failure inside the loop has to leave through `break`, since a refresh runs it again
    // with the terminal already initialized and `?` would skip `ratatui::restore()`.
    let ret: Result<()> = loop {
        let repository =
            match git::Repository::load(Path::new("."), order, max_count, mailmap, &revspec) {
                Ok(repository) => repository,
                Err(e) => break Err(e),
            };

        let graph = graph::calc_graph(&repository);

        let restored_graph_visible = refresh_view_context
            .as_ref()
            .map(|context| context.list_context().graph_visible);

        let graph_display = match check::decide_graph_display(&graph, graph_width, graph_renderer) {
            Ok(graph_display) => graph_display,
            Err(e) => match hidden_refresh_display(restored_graph_visible) {
                Some(graph_display) => graph_display,
                None => break Err(e),
            },
        };

        // Built from the renderer itself, so a text graph constructs no image manager at all
        // rather than one it never asks for — and there is no second place that has to be kept
        // agreeing about which renderer is in use.
        let graph_rows = match graph_renderer {
            graph::GraphRenderer::Image(style) => {
                graph::GraphRows::Image(Box::new(GraphImageManager::new(
                    &graph,
                    &graph_color_set,
                    graph_display.cell_width_type,
                    style,
                    graph_image_width_mode,
                    image_protocol,
                )))
            }
            graph::GraphRenderer::Text(style) => graph::GraphRows::Text(
                graph::GraphTextManager::new(&graph, style, &graph_color_set),
            ),
        };

        if terminal.is_none() {
            terminal = Some(ratatui::init());
        }

        let mut app = App::new(
            &repository,
            graph_rows,
            &graph,
            &graph_color_set,
            graph_display,
            initial_selection,
            ctx.clone(),
            &ec,
            refresh_view_context,
        );

        match app.run(terminal.as_mut().unwrap()) {
            Ok(Ret::Quit) => {
                break Ok(());
            }
            Ok(Ret::Refresh(request)) => {
                refresh_view_context = Some(request.context);
                continue;
            }
            Err(e) => {
                break Err(e.into());
            }
        }
    };

    ratatui::restore();
    ret
}
