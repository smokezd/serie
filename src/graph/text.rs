use ratatui::style::Color;

use crate::{
    color::GraphColorSet,
    git::CommitHash,
    graph::calc::{Edge, EdgeType, Graph},
};

/// Which glyph set a text graph draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextStyle {
    /// Strictly ASCII, for terminals or fonts without box-drawing characters.
    Ascii,
    /// Box-drawing characters, matching the shapes `EdgeType` is named after.
    Unicode,
}

/// Terminal columns a single lane occupies. Two, so that a horizontal run has a column to cross
/// between one lane and the next, the same spacing `git log --graph` uses.
pub const CELLS_PER_LANE: usize = 2;

/// Terminal columns a whole text graph occupies.
///
/// Every row shares one grid, so this is the width of each of them — the caller laying out the
/// column and the renderer filling it have to agree, and a drift between the two would be hidden
/// by `render_graph_text` clipping rather than reported.
pub fn text_graph_width(graph: &Graph<'_>) -> usize {
    (graph.max_pos_x + 1) * CELLS_PER_LANE
}

/// One rendered column: the glyph, and the lane whose colour it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextCell {
    pub symbol: char,
    pub lane: usize,
}

/// The four directions a glyph can connect toward. A cell is rendered from the set of directions
/// the edges painted onto it, which is what makes overlapping edges combine into `┼`, `├` and the
/// rest instead of one edge silently winning.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Connections {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
}

impl Connections {
    fn is_empty(&self) -> bool {
        !(self.up || self.down || self.left || self.right)
    }

    fn merge(&mut self, other: Connections) {
        self.up |= other.up;
        self.down |= other.down;
        self.left |= other.left;
        self.right |= other.right;
    }

    fn vertical() -> Self {
        Connections {
            up: true,
            down: true,
            ..Default::default()
        }
    }

    fn horizontal() -> Self {
        Connections {
            left: true,
            right: true,
            ..Default::default()
        }
    }

    fn of(up: bool, down: bool, left: bool, right: bool) -> Self {
        Connections {
            up,
            down,
            left,
            right,
        }
    }
}

/// Renders the row holding `commit_hash` as one cell per terminal column.
///
/// The row is always `(max_pos_x + 1) * CELLS_PER_LANE` columns wide so that lanes line up from
/// one row to the next; the image renderer can afford to trim each row to its own widest lane
/// because every row is a separate image, but text shares one grid.
pub fn build_graph_row_text(
    graph: &Graph<'_>,
    style: TextStyle,
    commit_hash: &CommitHash,
) -> Vec<Option<TextCell>> {
    let (pos_x, pos_y) = graph.commit_pos_map[commit_hash];
    let width = text_graph_width(graph);

    let mut connections: Vec<Connections> = vec![Connections::default(); width];
    // The lane each column takes its colour from. The first edge to paint a column wins, so a
    // crossing keeps the colour of the line that got there first rather than flickering.
    let mut lanes: Vec<Option<usize>> = vec![None; width];

    for edge in &graph.edges[pos_y] {
        for (column, paint) in edge_paints(edge) {
            if column >= width {
                continue;
            }
            connections[column].merge(paint);
            lanes[column].get_or_insert(edge.associated_line_pos_x);
        }
    }

    let mut cells: Vec<Option<TextCell>> = connections
        .iter()
        .zip(lanes.iter())
        .map(|(connection, lane)| {
            if connection.is_empty() {
                return None;
            }
            Some(TextCell {
                symbol: glyph(*connection, style),
                lane: lane.unwrap_or(0),
            })
        })
        .collect();

    // The commit itself outranks whatever edges pass through its column.
    let node_column = pos_x * CELLS_PER_LANE;
    if node_column < width {
        cells[node_column] = Some(TextCell {
            symbol: node_glyph(style),
            lane: pos_x,
        });
    }

    cells
}

/// Where a single edge paints, and which directions it contributes there.
///
/// Lane `x` owns columns `2x` (its left half, carrying the lane's own vertical line) and `2x + 1`
/// (its right half, the gap toward lane `x + 1`). An edge that leaves a lane sideways therefore
/// paints the half it leaves through, and the neighbouring lane paints the adjoining half, so the
/// two meet without either needing to know about the other.
fn edge_paints(edge: &Edge) -> Vec<(usize, Connections)> {
    let left_half = edge.pos_x * CELLS_PER_LANE;
    let right_half = left_half + 1;

    match edge.edge_type {
        EdgeType::Vertical => vec![(left_half, Connections::vertical())],
        EdgeType::Up => vec![(left_half, Connections::of(true, false, false, false))],
        EdgeType::Down => vec![(left_half, Connections::of(false, true, false, false))],
        EdgeType::Horizontal => vec![
            (left_half, Connections::horizontal()),
            (right_half, Connections::horizontal()),
        ],
        // ╴ and ╶ start at the lane's centre, so they contribute their direction to the lane
        // column itself. That is what turns a lane carrying a vertical line into `├`/`┤` when a
        // branch leaves it, the way the two overlap in a single image cell.
        EdgeType::Left => vec![(left_half, Connections::of(false, false, true, false))],
        EdgeType::Right => vec![
            (left_half, Connections::of(false, false, false, true)),
            (right_half, Connections::horizontal()),
        ],
        // ╮ arrives from the left and turns down; ╯ arrives from the left and turns up. The dash
        // that feeds them sits in the previous lane's right half, so neither paints one itself.
        EdgeType::RightTop => vec![(left_half, Connections::of(false, true, true, false))],
        EdgeType::RightBottom => vec![(left_half, Connections::of(true, false, true, false))],
        // ╭ and ╰ turn toward the right, so they also fill the half they leave through.
        EdgeType::LeftTop => vec![
            (left_half, Connections::of(false, true, false, true)),
            (right_half, Connections::horizontal()),
        ],
        EdgeType::LeftBottom => vec![
            (left_half, Connections::of(true, false, false, true)),
            (right_half, Connections::horizontal()),
        ],
    }
}

fn node_glyph(style: TextStyle) -> char {
    match style {
        TextStyle::Ascii => '*',
        TextStyle::Unicode => '●',
    }
}

fn glyph(connection: Connections, style: TextStyle) -> char {
    let Connections {
        up,
        down,
        left,
        right,
    } = connection;
    match style {
        // ASCII has no corner or tee characters, so everything that turns or branches is a `+`,
        // the usual convention for ASCII box art.
        TextStyle::Ascii => match (up, down, left, right) {
            (_, _, false, false) => '|',
            (false, false, _, _) => '-',
            _ => '+',
        },
        TextStyle::Unicode => match (up, down, left, right) {
            (true, true, true, true) => '┼',
            (true, true, true, false) => '┤',
            (true, true, false, true) => '├',
            (true, false, true, true) => '┴',
            (false, true, true, true) => '┬',
            (false, true, true, false) => '╮',
            (true, false, true, false) => '╯',
            (false, true, false, true) => '╭',
            (true, false, false, true) => '╰',
            (_, _, false, false) => '│',
            (false, false, _, _) => '─',
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(edges: &[Edge], max_pos_x: usize, style: TextStyle) -> String {
        let width = (max_pos_x + 1) * CELLS_PER_LANE;
        let mut connections: Vec<Connections> = vec![Connections::default(); width];
        for edge in edges {
            for (column, paint) in edge_paints(edge) {
                if column < width {
                    connections[column].merge(paint);
                }
            }
        }
        connections
            .iter()
            .map(|c| if c.is_empty() { ' ' } else { glyph(*c, style) })
            .collect()
    }

    #[test]
    fn test_vertical_lanes_sit_two_columns_apart() {
        let edges = [
            Edge::new(EdgeType::Vertical, 0, 0),
            Edge::new(EdgeType::Vertical, 1, 1),
        ];
        assert_eq!(render(&edges, 1, TextStyle::Unicode), "│ │ ");
        assert_eq!(render(&edges, 1, TextStyle::Ascii), "| | ");
    }

    #[test]
    fn test_branch_to_the_right_connects_across_the_gap() {
        // A line leaving lane 0 rightward and turning down at lane 2, as `calc_edges` emits it.
        let edges = [
            Edge::new(EdgeType::Right, 0, 2),
            Edge::new(EdgeType::Horizontal, 1, 2),
            Edge::new(EdgeType::RightBottom, 2, 2),
        ];
        assert_eq!(render(&edges, 2, TextStyle::Unicode), "────╯ ");
        assert_eq!(render(&edges, 2, TextStyle::Ascii), "----+ ");
    }

    #[test]
    fn test_branch_to_the_left_connects_across_the_gap() {
        let edges = [
            Edge::new(EdgeType::Left, 2, 0),
            Edge::new(EdgeType::Horizontal, 1, 0),
            Edge::new(EdgeType::LeftBottom, 0, 0),
        ];
        assert_eq!(render(&edges, 2, TextStyle::Unicode), "╰──── ");
        assert_eq!(render(&edges, 2, TextStyle::Ascii), "+---- ");
    }

    #[test]
    fn test_a_horizontal_run_crossing_a_lane_combines_into_a_cross() {
        // A branch passing through lane 1 while lane 1 also carries its own vertical line.
        let edges = [
            Edge::new(EdgeType::Vertical, 1, 1),
            Edge::new(EdgeType::Right, 0, 2),
            Edge::new(EdgeType::Horizontal, 1, 2),
            Edge::new(EdgeType::RightBottom, 2, 2),
        ];
        assert_eq!(render(&edges, 2, TextStyle::Unicode), "──┼─╯ ");
        assert_eq!(render(&edges, 2, TextStyle::Ascii), "--+-+ ");
    }

    #[test]
    fn test_tees_appear_where_a_line_branches_off_a_lane() {
        let edges = [
            Edge::new(EdgeType::Vertical, 0, 0),
            Edge::new(EdgeType::Right, 0, 1),
        ];
        // The lane keeps its vertical and gains the branch, so it reads as a tee.
        assert_eq!(render(&edges, 1, TextStyle::Unicode), "├─  ");
        assert_eq!(render(&edges, 1, TextStyle::Ascii), "+-  ");
    }
}

/// Builds text graph rows on demand, mirroring `GraphImageManager`.
///
/// The rows used to be built for every commit in `App::new`, before the first frame and again on
/// every refresh, which cost commits × lanes of work and memory the image path never paid. Only
/// the visible rows are ever drawn, and a row is pure integer arithmetic, so there is nothing
/// worth caching: building one costs less than keeping one.
#[derive(Debug)]
pub struct GraphTextManager<'a> {
    graph: &'a Graph<'a>,
    style: TextStyle,
    graph_color_set: &'a GraphColorSet,
}

impl<'a> GraphTextManager<'a> {
    pub fn new(graph: &'a Graph<'a>, style: TextStyle, graph_color_set: &'a GraphColorSet) -> Self {
        GraphTextManager {
            graph,
            style,
            graph_color_set,
        }
    }

    pub fn width(&self) -> usize {
        text_graph_width(self.graph)
    }

    /// One entry per terminal column: the glyph and the colour of the lane it belongs to. The
    /// lane-to-colour step happens here, the way `GraphImageManager` resolves colours when it
    /// draws, so the cell-building logic itself stays free of theming.
    pub fn row(&self, commit_hash: &CommitHash) -> Vec<Option<(char, Color)>> {
        build_graph_row_text(self.graph, self.style, commit_hash)
            .into_iter()
            .map(|cell| {
                cell.map(|c| {
                    (
                        c.symbol,
                        self.graph_color_set.get(c.lane).to_ratatui_color(),
                    )
                })
            })
            .collect()
    }
}
