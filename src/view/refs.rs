use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    Frame,
};

use crate::{
    app::AppContext,
    config::UserListColumnType,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::Ref,
    view::{ListRefreshViewContext, RefreshViewContext, RefsRefreshViewContext},
    widget::{
        commit_list::{CommitList, CommitListState, GraphToggleResult},
        ref_list::{RefList, RefListState, TREE_HEAD_IDENT},
    },
};

#[derive(Debug)]
pub struct RefsView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    ref_list_state: RefListState,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> RefsView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        refs: &[&Ref],
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> RefsView<'a> {
        // Taken before the state moves into the struct; the refs tree pins HEAD at the top.
        let ref_list_state = RefListState::new(refs, commit_list_state.head());
        RefsView {
            commit_list_state: Some(commit_list_state),
            ref_list_state,
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::Quit => {
                self.tx.send(AppEvent::Quit);
            }
            UserEvent::Cancel | UserEvent::Close | UserEvent::RefList => {
                self.tx.send(AppEvent::CloseRefs);
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                for _ in 0..count {
                    self.ref_list_state.select_next();
                }
                self.update_commit_list_selected();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.ref_list_state.select_prev();
                }
                self.update_commit_list_selected();
            }
            UserEvent::GoToTop => {
                self.ref_list_state.select_first();
                self.update_commit_list_selected();
            }
            UserEvent::GoToBottom => {
                self.ref_list_state.select_last();
                self.update_commit_list_selected();
            }
            UserEvent::NavigateRight => {
                self.ref_list_state.open_node();
                self.update_commit_list_selected();
            }
            UserEvent::NavigateLeft => {
                self.ref_list_state.close_node();
                self.update_commit_list_selected();
            }
            UserEvent::ShortCopy | UserEvent::FullCopy => {
                self.copy_ref_name();
            }
            UserEvent::GraphToggle => {
                self.toggle_graph();
            }
            UserEvent::HelpToggle => {
                self.tx.send(AppEvent::OpenHelp);
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let [list_area, refs_area] = self.split_areas(area);
        let commit_list = CommitList::new(self.ctx.clone());
        f.render_stateful_widget(commit_list, list_area, self.as_mut_list_state());

        let ref_list = RefList::new(self.ctx.clone());
        f.render_stateful_widget(ref_list, refs_area, &mut self.ref_list_state);
    }

    pub fn update_layout(&mut self, area: Rect) {
        let [list_area, _] = self.split_areas(area);
        self.as_mut_list_state()
            .update_height(list_area.height as usize);
    }

    pub fn prepare_graph_uploads(&mut self) {
        self.as_mut_list_state().ensure_visible_graph_uploaded();
    }
}

impl<'a> RefsView<'a> {
    pub fn take_list_state(&mut self) -> CommitListState<'a> {
        self.commit_list_state.take().unwrap()
    }

    fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        self.commit_list_state.as_mut().unwrap()
    }

    fn as_list_state(&self) -> &CommitListState<'a> {
        self.commit_list_state.as_ref().unwrap()
    }

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        self.as_mut_list_state().drain_pending_graph_uploads()
    }

    pub fn graph_image_ids_sorted(&self) -> Vec<u32> {
        self.as_list_state().graph_image_ids_sorted()
    }

    fn split_areas(&self, area: Rect) -> [Rect; 2] {
        let graph_width = self.as_list_state().graph_area_cell_width() + 1; // graph area + marker
        let refs_width =
            (area.width.saturating_sub(graph_width)).min(self.ctx.ui_config.refs.width);
        Layout::horizontal([Constraint::Min(0), Constraint::Length(refs_width)]).areas(area)
    }

    /// The same behaviour as in the list view: the commit list is on screen here too, so the key
    /// that hides its graph column has to work here too.
    ///
    /// `go_to_merge_base` and `go_to_next_tip` are deliberately *not* forwarded. Here the commit
    /// list follows whichever ref the tree has selected, so jumping it on its own would leave the
    /// highlighted ref describing a commit that is no longer selected.
    fn toggle_graph(&mut self) {
        if !self
            .ctx
            .ui_config
            .list
            .columns
            .contains(&UserListColumnType::Graph)
        {
            self.tx.send(AppEvent::UpdateStatusTransient(
                "Graph column is not enabled".into(),
            ));
            return;
        }
        match self.as_mut_list_state().toggle_graph() {
            GraphToggleResult::Shown => {}
            GraphToggleResult::Hidden => {
                self.tx.send(AppEvent::ClearGraphImages);
            }
            GraphToggleResult::TerminalTooSmall => {
                // `toggleable` is decided once, against the terminal as it was at startup, so
                // this can be stale on a terminal the user has since widened. Naming the refresh
                // is what makes the refusal actionable rather than wrong-looking.
                self.tx.send(AppEvent::NotifyError(
                    "Terminal was too small for the commit graph at startup; resize, then refresh"
                        .into(),
                ));
            }
        }
    }

    fn update_commit_list_selected(&mut self) {
        let Some(selected) = self.ref_list_state.selected_ref_name() else {
            return;
        };
        // Only `HEAD` is worth announcing. It is listed from the `Head` enum alone, so it can name
        // a commit that `--max-count` or the revspec kept off the list, and a silent no-op there
        // looks like a dead key. Every other miss is a root or a folder node — `selected_ref_name`
        // returns the last path segment whatever the node is — and those never name a commit, so
        // announcing them would put "Branches is not among the rendered commits" on screen every
        // time the cursor passed over one.
        if !self.as_mut_list_state().select_ref(&selected) && selected == TREE_HEAD_IDENT {
            self.tx.send(AppEvent::UpdateStatusTransient(format!(
                "{selected} is not among the rendered commits"
            )));
        }
    }

    fn copy_ref_name(&self) {
        if let Some(selected) = self.ref_list_state.selected_branch() {
            self.copy_to_clipboard("Branch Name".into(), selected);
        } else if let Some(selected) = self.ref_list_state.selected_tag() {
            self.copy_to_clipboard("Tag Name".into(), selected);
        }
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        self.tx.send(AppEvent::CopyToClipboard { name, value });
    }

    pub fn refresh(&self) {
        let list_state = self.as_list_state();
        let list_context = ListRefreshViewContext::from(list_state);
        let (tree_selected, tree_opened) = self.ref_list_state.current_tree_status();
        let refs_context = RefsRefreshViewContext {
            selected: tree_selected,
            opened: tree_opened,
        };
        let context = RefreshViewContext::Refs {
            list_context,
            refs_context,
        };
        self.tx.send(AppEvent::Refresh(context));
    }

    pub fn reset_refs_with(&mut self, refs_context: RefsRefreshViewContext) {
        self.ref_list_state
            .reset_tree_status(refs_context.selected, refs_context.opened);
    }
}
