use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, HunkdiffMode, Sender, UserEvent, UserEventWithCount},
    git::{Commit, FileChange, Ref, Repository},
    view::{ListRefreshViewContext, RefreshViewContext},
    widget::{
        commit_detail::{CommitDetail, CommitDetailState, MessageLines},
        commit_list::{CommitList, CommitListState},
    },
};

#[derive(Debug)]
pub struct DetailView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    commit_detail_state: CommitDetailState,

    commit: Commit,
    changes: Vec<FileChange>,
    refs: Vec<Ref>,

    /// Rows the detail pane occupies. Starts at `ui.detail.height` and is adjusted at runtime by
    /// `detail_height_increase` / `detail_height_decrease`; `App` carries it across opening and
    /// closing the view, and across a refresh.
    detail_height: u16,
    /// Height of the whole view area at the last layout, so a resize knows what it may not exceed.
    /// `update_layout` runs every frame, so this is never stale by more than one.
    area_height: u16,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> DetailView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        commit: Commit,
        changes: Vec<FileChange>,
        refs: Vec<Ref>,
        detail_height: u16,
        message_lines: MessageLines,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> DetailView<'a> {
        DetailView {
            commit_list_state: Some(commit_list_state),
            commit_detail_state: CommitDetailState::new(message_lines),
            commit,
            changes,
            refs,
            detail_height,
            area_height: 0,
            ctx,
            tx,
        }
    }

    /// The pane height as the user has left it, so it survives closing and reopening the view.
    pub fn detail_height(&self) -> u16 {
        self.detail_height
    }

    /// How much of the message the user has left on show, kept for the same reason.
    pub fn message_lines(&self) -> MessageLines {
        self.commit_detail_state.message_lines()
    }

    /// One row is always left to the commit list, and the pane never shrinks past a single row.
    fn resize_detail(&mut self, delta: i32) {
        self.detail_height = resized_detail_height(self.detail_height, self.area_height, delta);
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::NavigateDown => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_down();
                }
            }
            UserEvent::NavigateUp => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_up();
                }
            }
            UserEvent::PageDown => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_page_down();
                }
            }
            UserEvent::PageUp => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_page_up();
                }
            }
            UserEvent::HalfPageDown => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_half_page_down();
                }
            }
            UserEvent::HalfPageUp => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_half_page_up();
                }
            }
            UserEvent::GoToTop => {
                self.commit_detail_state.select_first();
            }
            UserEvent::GoToBottom => {
                self.commit_detail_state.select_last();
            }
            UserEvent::SelectDown => {
                self.tx.send(AppEvent::SelectOlderCommit);
            }
            UserEvent::SelectUp => {
                self.tx.send(AppEvent::SelectNewerCommit);
            }
            UserEvent::GoToParent => {
                self.tx.send(AppEvent::SelectParentCommit);
            }
            UserEvent::ShortCopy => {
                self.copy_commit_short_hash();
            }
            UserEvent::FullCopy => {
                self.copy_commit_hash();
            }
            UserEvent::UserCommand(n) => {
                self.tx.send(AppEvent::OpenUserCommand(n));
            }
            UserEvent::HunkdiffShow => {
                self.tx.send(AppEvent::OpenHunkdiff(HunkdiffMode::Show));
            }
            UserEvent::HunkdiffDiffToHead => {
                self.tx
                    .send(AppEvent::OpenHunkdiff(HunkdiffMode::DiffToHead));
            }
            UserEvent::HunkdiffDiffThroughWorktree => {
                self.tx
                    .send(AppEvent::OpenHunkdiff(HunkdiffMode::DiffThroughWorktree));
            }
            UserEvent::DetailHeightIncrease => {
                self.resize_detail(count as i32);
            }
            UserEvent::DetailHeightDecrease => {
                self.resize_detail(-(count as i32));
            }
            UserEvent::DetailMessageToggle => {
                let message_lines = self.commit_detail_state.toggle_message_lines();
                self.tx
                    .send(AppEvent::UpdateStatusTransient(match message_lines {
                        MessageLines::Full => "Commit message: full".into(),
                        MessageLines::Five => "Commit message: 5 lines".into(),
                        MessageLines::Ten => "Commit message: 10 lines".into(),
                    }));
            }
            UserEvent::HelpToggle => {
                self.tx.send(AppEvent::OpenHelp);
            }
            UserEvent::Confirm | UserEvent::Cancel | UserEvent::Close | UserEvent::Quit => {
                self.tx.send(AppEvent::CloseDetail);
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let [list_area, detail_area] = self.split_areas(area);

        let commit_list = CommitList::new(self.ctx.clone());
        f.render_stateful_widget(commit_list, list_area, self.as_mut_list_state());

        let commit_detail =
            CommitDetail::new(&self.commit, &self.changes, &self.refs, self.ctx.clone());
        f.render_stateful_widget(commit_detail, detail_area, &mut self.commit_detail_state);
    }

    pub fn update_layout(&mut self, area: Rect) {
        self.area_height = area.height;
        let [list_area, _] = self.split_areas(area);
        self.as_mut_list_state()
            .update_height(list_area.height as usize);
    }

    pub fn prepare_graph_uploads(&mut self) {
        self.as_mut_list_state().ensure_visible_graph_uploaded();
    }
}

impl<'a> DetailView<'a> {
    pub fn take_list_state(&mut self) -> CommitListState<'a> {
        self.commit_list_state.take().unwrap()
    }

    fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        self.commit_list_state.as_mut().unwrap()
    }

    pub fn as_list_state(&self) -> &CommitListState<'a> {
        self.commit_list_state.as_ref().unwrap()
    }

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        self.as_mut_list_state().drain_pending_graph_uploads()
    }

    pub fn graph_image_ids_sorted(&self) -> Vec<u32> {
        self.as_list_state().graph_image_ids_sorted()
    }

    fn split_areas(&self, area: Rect) -> [Rect; 2] {
        // `saturating_sub` because an area with no rows at all is reachable on a tiny terminal.
        let detail_height = area.height.saturating_sub(1).min(self.detail_height);
        Layout::vertical([Constraint::Min(0), Constraint::Length(detail_height)]).areas(area)
    }

    pub fn select_older_commit(&mut self, repository: &Repository) {
        self.update_selected_commit(repository, |state| state.select_next());
    }

    pub fn select_newer_commit(&mut self, repository: &Repository) {
        self.update_selected_commit(repository, |state| state.select_prev());
    }

    pub fn select_parent_commit(&mut self, repository: &Repository) {
        self.update_selected_commit(repository, |state| state.select_parent());
    }

    fn update_selected_commit<F>(&mut self, repository: &Repository, update_commit_list_state: F)
    where
        F: FnOnce(&mut CommitListState<'a>),
    {
        let commit_list_state = self.as_mut_list_state();
        update_commit_list_state(commit_list_state);
        let selected = commit_list_state.selected_commit_hash().clone();
        let (commit, changes) = repository.commit_detail(&selected);
        let refs = repository.refs(&selected).into_iter().cloned().collect();
        self.commit = commit;
        self.changes = changes;
        self.refs = refs;

        self.commit_detail_state.select_first();
    }

    fn copy_commit_short_hash(&self) {
        let selected = &self.commit.commit_hash;
        self.copy_to_clipboard("Commit SHA (short)".into(), selected.as_short_hash().into());
    }

    fn copy_commit_hash(&self) {
        let selected = &self.commit.commit_hash;
        self.copy_to_clipboard("Commit SHA".into(), selected.as_str().into());
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        self.tx.send(AppEvent::CopyToClipboard { name, value });
    }

    pub fn refresh(&self) {
        let list_state = self.as_list_state();
        let mut list_context = ListRefreshViewContext::from(list_state);
        list_context.detail_height = Some(self.detail_height);
        list_context.detail_message_lines = Some(self.message_lines());
        let context = RefreshViewContext::Detail { list_context };
        self.tx.send(AppEvent::Refresh(context));
    }
}

/// The pane height after a resize, clamped so the commit list keeps at least one row and the pane
/// itself never disappears. `area_height` of 0 means no layout has happened yet, which leaves only
/// the floor to apply.
fn resized_detail_height(current: u16, area_height: u16, delta: i32) -> u16 {
    let max = area_height.saturating_sub(1).max(1);
    let next = i32::from(current).saturating_add(delta);
    next.clamp(1, i32::from(max)) as u16
}

#[cfg(test)]
mod tests {
    use super::resized_detail_height;

    #[test]
    fn test_resize_moves_by_the_count() {
        assert_eq!(resized_detail_height(20, 40, 1), 21);
        assert_eq!(resized_detail_height(20, 40, -1), 19);
        assert_eq!(resized_detail_height(20, 40, 10), 30);
        assert_eq!(resized_detail_height(20, 40, -10), 10);
    }

    #[test]
    fn test_resize_leaves_the_commit_list_a_row() {
        // The pane may fill everything but the last row, however hard the key is held.
        assert_eq!(resized_detail_height(20, 30, 99), 29);
        assert_eq!(resized_detail_height(29, 30, 1), 29);
    }

    #[test]
    fn test_resize_never_closes_the_pane() {
        assert_eq!(resized_detail_height(20, 30, -99), 1);
        assert_eq!(resized_detail_height(1, 30, -1), 1);
    }

    #[test]
    fn test_resize_survives_a_terminal_with_no_rows() {
        // `area_height` is 0 before the first layout, and on a terminal too short to lay out.
        assert_eq!(resized_detail_height(20, 0, 1), 1);
        assert_eq!(resized_detail_height(20, 1, 5), 1);
    }
}
