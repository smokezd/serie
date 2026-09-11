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

    /// Commit graph image cell width [default: auto]
    #[arg(short, long, value_name = "TYPE")]
    graph_width: Option<GraphWidthType>,

    /// Commit graph image edge style [default: rounded]
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
    revspec.iter().map(|rev| normalize_rev(rev)).collect()
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
        graph_renderer,
        revspec_label: (!revspec.is_empty()).then(|| revspec.join(" ")),
    });

    let ec = event::EventController::init();
    let mut refresh_view_context = None;
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

        let graph_display = match check::decide_graph_display(&graph, graph_width, graph_renderer) {
            Ok(graph_display) => graph_display,
            Err(e) => break Err(e),
        };

        let graph_image_manager = GraphImageManager::new(
            &graph,
            &graph_color_set,
            graph_display.cell_width_type,
            // The image style only matters to the image renderer; a text graph never asks the
            // manager for a row.
            graph_renderer
                .image_style()
                .unwrap_or(graph::GraphStyle::Rounded),
            graph_image_width_mode,
            image_protocol,
        );

        if terminal.is_none() {
            terminal = Some(ratatui::init());
        }

        let mut app = App::new(
            &repository,
            graph_image_manager,
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
