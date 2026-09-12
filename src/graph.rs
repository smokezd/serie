mod calc;
mod geometry;
mod image;
mod text;

pub use calc::*;
pub use image::*;
pub use text::*;

/// How the commit graph is drawn. `Image` needs a terminal image protocol and a cell width;
/// `Text` needs neither, so it works in any terminal.
///
/// Lives here rather than in either module: it is the choice *between* the two, so neither of the
/// alternatives should own it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphRenderer {
    Image(GraphStyle),
    Text(TextStyle),
}

impl GraphRenderer {
    pub fn text_style(&self) -> Option<TextStyle> {
        match self {
            GraphRenderer::Image(_) => None,
            GraphRenderer::Text(style) => Some(*style),
        }
    }
}

/// The renderer the commit list actually draws with, holding whatever that renderer needs.
///
/// One value instead of three independent carriers — a `GraphRenderer` in the app context, a
/// `text_graph: bool` on the list state, and an image manager built even when no image is ever
/// asked for. Those three were set from the same source but nothing enforced that they agreed.
#[derive(Debug)]
pub enum GraphRows<'a> {
    Image(Box<GraphImageManager<'a>>),
    Text(GraphTextManager<'a>),
}

impl<'a> GraphRows<'a> {
    pub fn is_text(&self) -> bool {
        matches!(self, GraphRows::Text(_))
    }

    pub fn image_manager_mut(&mut self) -> Option<&mut GraphImageManager<'a>> {
        match self {
            GraphRows::Image(manager) => Some(manager),
            GraphRows::Text(_) => None,
        }
    }

    pub fn image_manager(&self) -> Option<&GraphImageManager<'a>> {
        match self {
            GraphRows::Image(manager) => Some(manager),
            GraphRows::Text(_) => None,
        }
    }

    pub fn text_manager(&self) -> Option<&GraphTextManager<'a>> {
        match self {
            GraphRows::Image(_) => None,
            GraphRows::Text(manager) => Some(manager),
        }
    }
}
