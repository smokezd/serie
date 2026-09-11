use ratatui::crossterm::terminal;

use crate::{
    graph::{CellWidthType, Graph},
    GraphWidthType, Result,
};

/// How the graph column starts out, and whether `graph_toggle` may change that.
///
/// `cell_width_type` is always resolved, even while `visible` is false, because the graph images
/// are built against it and a later toggle has to have something to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphDisplay {
    pub cell_width_type: CellWidthType,
    pub visible: bool,
    /// False when the terminal is too narrow for even a single-width graph. Toggling is refused
    /// rather than drawing a clipped graph.
    pub toggleable: bool,
}

pub fn decide_graph_display(
    graph: &Graph,
    cell_width_type: Option<GraphWidthType>,
) -> Result<GraphDisplay> {
    let (w, h) = terminal::size()?;
    decide_graph_display_from(graph.max_pos_x, w as usize, h as usize, cell_width_type)
}

fn decide_graph_display_from(
    max_pos_x: usize,
    term_width: usize,
    term_height: usize,
    cell_width_type: Option<GraphWidthType>,
) -> Result<GraphDisplay> {
    // A graph that is hidden from the start must never keep the app from launching: the terminal
    // being too narrow for a graph nobody asked to see is not an error.
    if let Some(GraphWidthType::Hidden) = cell_width_type {
        let (cell_width_type, toggleable) =
            match decide_cell_width_type_from(max_pos_x, term_width, term_height, None) {
                Ok(cell_width_type) => (cell_width_type, true),
                Err(_) => (CellWidthType::Single, false),
            };
        return Ok(GraphDisplay {
            cell_width_type,
            visible: false,
            toggleable,
        });
    }

    let cell_width_type =
        decide_cell_width_type_from(max_pos_x, term_width, term_height, cell_width_type)?;
    Ok(GraphDisplay {
        cell_width_type,
        visible: true,
        toggleable: true,
    })
}

fn decide_cell_width_type_from(
    max_pos_x: usize,
    term_width: usize,
    term_height: usize,
    cell_width_type: Option<GraphWidthType>,
) -> Result<CellWidthType> {
    let single_image_cell_width = max_pos_x + 1;
    let double_image_cell_width = single_image_cell_width * 2;

    match cell_width_type {
        Some(GraphWidthType::Double) => {
            let required_width = double_image_cell_width + 2;
            if required_width > term_width {
                let msg = format!("Terminal too small ({term_width}x{term_height} characters). The current graph needs at least {required_width} columns to display properly.");
                return Err(msg.into());
            }
            Ok(CellWidthType::Double)
        }
        Some(GraphWidthType::Single) => {
            let required_width = single_image_cell_width + 2;
            if required_width > term_width {
                let msg = format!("Terminal too small ({term_width}x{term_height} characters). The current graph needs at least {required_width} columns to display properly.");
                return Err(msg.into());
            }
            Ok(CellWidthType::Single)
        }
        // `Hidden` is resolved by `decide_graph_display_from` before reaching here, and falls back
        // to the same width `Auto` would pick so that a later toggle has a graph to show.
        Some(GraphWidthType::Auto) | Some(GraphWidthType::Hidden) | None => {
            let double_required_width = double_image_cell_width + 2;
            if double_required_width <= term_width {
                return Ok(CellWidthType::Double);
            }
            let single_required_width = single_image_cell_width + 2;
            if single_required_width <= term_width {
                return Ok(CellWidthType::Single);
            }
            let msg = format!("Terminal too small ({term_width}x{term_height} characters). The current graph needs at least {single_required_width} columns to display properly.");
            Err(msg.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A graph with `max_pos_x` 2 needs 3 cells single, 6 double, each plus 2 columns of padding.
    const MAX_POS_X: usize = 2;
    const TERM_HEIGHT: usize = 40;

    fn display(term_width: usize, width_type: Option<GraphWidthType>) -> Result<GraphDisplay> {
        decide_graph_display_from(MAX_POS_X, term_width, TERM_HEIGHT, width_type)
    }

    #[test]
    fn test_auto_prefers_double_then_single() {
        assert_eq!(
            display(80, Some(GraphWidthType::Auto)).unwrap(),
            GraphDisplay {
                cell_width_type: CellWidthType::Double,
                visible: true,
                toggleable: true,
            }
        );
        assert_eq!(
            display(7, Some(GraphWidthType::Auto)).unwrap(),
            GraphDisplay {
                cell_width_type: CellWidthType::Single,
                visible: true,
                toggleable: true,
            }
        );
    }

    #[test]
    fn test_visible_graph_still_fails_on_narrow_terminal() {
        assert!(display(4, Some(GraphWidthType::Auto)).is_err());
        assert!(display(4, None).is_err());
        assert!(display(4, Some(GraphWidthType::Single)).is_err());
        assert!(display(7, Some(GraphWidthType::Double)).is_err());
    }

    #[test]
    fn test_hidden_resolves_to_the_auto_width() {
        assert_eq!(
            display(80, Some(GraphWidthType::Hidden)).unwrap(),
            GraphDisplay {
                cell_width_type: CellWidthType::Double,
                visible: false,
                toggleable: true,
            }
        );
        assert_eq!(
            display(7, Some(GraphWidthType::Hidden)).unwrap(),
            GraphDisplay {
                cell_width_type: CellWidthType::Single,
                visible: false,
                toggleable: true,
            }
        );
    }

    #[test]
    fn test_hidden_never_fails_but_is_not_toggleable_when_too_narrow() {
        assert_eq!(
            display(4, Some(GraphWidthType::Hidden)).unwrap(),
            GraphDisplay {
                cell_width_type: CellWidthType::Single,
                visible: false,
                toggleable: false,
            }
        );
    }
}
