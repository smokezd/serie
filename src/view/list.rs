use std::rc::Rc;

use ratatui::{crossterm::event::KeyEvent, layout::Rect, Frame};

use crate::{
    app::AppContext,
    config::UserListColumnType,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::CommitHash,
    view::{ListRefreshViewContext, RefreshViewContext},
    widget::commit_list::{
        CommitList, CommitListState, GraphToggleResult, MergeBaseJump, SearchState, TipJump,
    },
};

#[derive(Debug)]
pub struct ListView<'a> {
    commit_list_state: Option<CommitListState<'a>>,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> ListView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> ListView<'a> {
        ListView {
            commit_list_state: Some(commit_list_state),
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;
        if let SearchState::Searching { .. } = self.as_list_state().search_state() {
            match event {
                UserEvent::Confirm => {
                    self.as_mut_list_state().apply_search();
                    self.update_matched_message();
                }
                UserEvent::Cancel => {
                    self.as_mut_list_state().cancel_search();
                    self.clear_search_query();
                }
                UserEvent::SearchTargetToggle => {
                    self.as_mut_list_state().toggle_search_target();
                    self.update_search_status();
                }
                UserEvent::IgnoreCaseToggle => {
                    self.as_mut_list_state().toggle_ignore_case();
                    self.update_search_status();
                }
                UserEvent::FuzzyToggle => {
                    self.as_mut_list_state().toggle_fuzzy();
                    self.update_search_status();
                }
                _ => {
                    self.as_mut_list_state().handle_search_input(key);
                    self.update_search_status();
                }
            }
            return;
        } else {
            match event {
                UserEvent::Quit => {
                    self.tx.send(AppEvent::Quit);
                }
                UserEvent::NavigateDown | UserEvent::SelectDown => {
                    for _ in 0..count {
                        self.as_mut_list_state().select_next();
                    }
                }
                UserEvent::NavigateUp | UserEvent::SelectUp => {
                    for _ in 0..count {
                        self.as_mut_list_state().select_prev();
                    }
                }
                UserEvent::GoToParent => {
                    for _ in 0..count {
                        self.as_mut_list_state().select_parent();
                    }
                }
                UserEvent::GoToTop => {
                    self.as_mut_list_state().select_first();
                }
                UserEvent::GoToBottom => {
                    self.as_mut_list_state().select_last();
                }
                UserEvent::ScrollDown => {
                    for _ in 0..count {
                        self.as_mut_list_state().scroll_down();
                    }
                }
                UserEvent::ScrollUp => {
                    for _ in 0..count {
                        self.as_mut_list_state().scroll_up();
                    }
                }
                UserEvent::PageDown => {
                    for _ in 0..count {
                        self.as_mut_list_state().scroll_down_page();
                    }
                }
                UserEvent::PageUp => {
                    for _ in 0..count {
                        self.as_mut_list_state().scroll_up_page();
                    }
                }
                UserEvent::HalfPageDown => {
                    for _ in 0..count {
                        self.as_mut_list_state().scroll_down_half();
                    }
                }
                UserEvent::HalfPageUp => {
                    for _ in 0..count {
                        self.as_mut_list_state().scroll_up_half();
                    }
                }
                UserEvent::SelectTop => {
                    self.as_mut_list_state().select_high();
                }
                UserEvent::SelectMiddle => {
                    self.as_mut_list_state().select_middle();
                }
                UserEvent::SelectBottom => {
                    self.as_mut_list_state().select_low();
                }
                UserEvent::ShortCopy => {
                    self.copy_commit_short_hash();
                }
                UserEvent::FullCopy => {
                    self.copy_commit_hash();
                }
                UserEvent::Search => {
                    self.as_mut_list_state().start_search();
                    self.update_search_status();
                }
                UserEvent::SearchTargetToggle => {
                    self.as_mut_list_state().toggle_search_target();
                    self.update_search_options_message();
                }
                UserEvent::IgnoreCaseToggle => {
                    self.as_mut_list_state().toggle_ignore_case();
                    self.update_search_options_message();
                }
                UserEvent::FuzzyToggle => {
                    self.as_mut_list_state().toggle_fuzzy();
                    self.update_search_options_message();
                }
                UserEvent::GraphToggle => {
                    self.toggle_graph();
                }
                UserEvent::GoToMergeBase => {
                    self.go_to_merge_base();
                }
                UserEvent::GoToNextTip => {
                    self.go_to_next_tip();
                }
                UserEvent::UserCommand(n) => {
                    self.tx.send(AppEvent::OpenUserCommand(n));
                }
                UserEvent::HelpToggle => {
                    self.tx.send(AppEvent::OpenHelp);
                }
                UserEvent::Cancel => {
                    self.as_mut_list_state().cancel_search();
                    self.clear_search_query();
                }
                UserEvent::Confirm => {
                    self.tx.send(AppEvent::OpenDetail);
                }
                UserEvent::RefList => {
                    self.tx.send(AppEvent::OpenRefs);
                }
                UserEvent::Refresh => {
                    self.refresh();
                }
                _ => {}
            }
        }

        if let SearchState::Applied { .. } = self.as_list_state().search_state() {
            match event {
                UserEvent::GoToNext => {
                    self.as_mut_list_state().select_next_match();
                    self.update_matched_message();
                }
                UserEvent::GoToPrevious => {
                    self.as_mut_list_state().select_prev_match();
                    self.update_matched_message();
                }
                _ => {}
            }
            // Do not return here
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let commit_list = CommitList::new(self.ctx.clone());
        f.render_stateful_widget(commit_list, area, self.as_mut_list_state());
    }

    pub fn update_layout(&mut self, area: Rect) {
        self.as_mut_list_state().update_height(area.height as usize);
    }

    pub fn prepare_graph_uploads(&mut self) {
        self.as_mut_list_state().ensure_visible_graph_uploaded();
    }
}

impl<'a> ListView<'a> {
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

    fn go_to_next_tip(&mut self) {
        match self.as_mut_list_state().select_next_tip() {
            // Landing on the next tip is visible on screen, so it needs no announcement.
            TipJump::Selected => {}
            TipJump::NoTips => {
                self.tx.send(AppEvent::UpdateStatusTransient(
                    "Revspec tips need two or more revisions".into(),
                ));
            }
            TipJump::OutsideRenderedCommits => {
                self.tx.send(AppEvent::NotifyError(
                    "No revspec tip is among the rendered commits".into(),
                ));
            }
        }
    }

    fn go_to_merge_base(&mut self) {
        match self.as_mut_list_state().select_merge_base() {
            // Arriving where the key said to go needs no announcement.
            MergeBaseJump::Selected => {}
            MergeBaseJump::NotScoped => {
                self.tx.send(AppEvent::UpdateStatusTransient(
                    "Merge base needs exactly two revisions".into(),
                ));
            }
            MergeBaseJump::UnrelatedHistories => {
                self.tx.send(AppEvent::UpdateStatusTransient(
                    "The two revisions have no common ancestor".into(),
                ));
            }
            MergeBaseJump::OutsideRenderedCommits => {
                self.tx.send(AppEvent::NotifyError(
                    "Merge base is not among the rendered commits".into(),
                ));
            }
        }
    }

    fn toggle_graph(&mut self) {
        if !self
            .ctx
            .ui_config
            .list
            .columns
            .contains(&UserListColumnType::Graph)
        {
            // Hiding a column that was never laid out would look like a dead key, so say so
            // instead of silently doing nothing.
            self.tx.send(AppEvent::UpdateStatusTransient(
                "Graph column is not enabled".into(),
            ));
            return;
        }
        match self.as_mut_list_state().toggle_graph() {
            // Showing or hiding the graph is visible on screen, so it needs no announcement.
            GraphToggleResult::Shown => {}
            GraphToggleResult::Hidden => {
                // Images already placed for the graph outlive the column that held them.
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

    fn update_search_status(&self) {
        if let SearchState::Searching { .. } = self.as_list_state().search_state() {
            let list_state = self.as_list_state();
            if let Some(query) = list_state.search_query_string() {
                let cursor_position = list_state.search_query_cursor_position();
                let options = list_state.search_options().status_string();
                self.tx.send(AppEvent::UpdateStatusInput {
                    message: query,
                    cursor_position,
                    metadata: options,
                });
            }
        }
    }

    fn update_search_options_message(&self) {
        if let SearchState::Applied { .. } = self.as_list_state().search_state() {
            self.update_matched_message();
        } else {
            let options = self.as_list_state().search_options().status_string();
            self.tx.send(AppEvent::UpdateStatusTransient(format!(
                "Search: {options}"
            )));
        }
    }

    fn clear_search_query(&self) {
        self.tx.send(AppEvent::ClearStatusLine);
    }

    fn update_matched_message(&self) {
        if let Some((msg, matched)) = self.as_list_state().matched_query_string() {
            let options = self.as_list_state().search_options().status_string();
            self.tx.send(AppEvent::UpdateSearchResult {
                message: msg,
                options,
                matched,
            });
        } else {
            self.tx.send(AppEvent::ClearStatusLine);
        }
    }

    fn copy_commit_short_hash(&self) {
        let selected = self.as_list_state().selected_commit_hash();
        self.copy_to_clipboard("Commit SHA (short)".into(), selected.as_short_hash().into());
    }

    fn copy_commit_hash(&self) {
        let selected = self.as_list_state().selected_commit_hash();
        self.copy_to_clipboard("Commit SHA".into(), selected.as_str().into());
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        self.tx.send(AppEvent::CopyToClipboard { name, value });
    }

    pub fn refresh(&self) {
        let list_state = self.as_list_state();
        let list_context = ListRefreshViewContext::from(list_state);
        let context = RefreshViewContext::List { list_context };
        self.tx.send(AppEvent::Refresh(context));
    }

    pub fn reset_commit_list_with(&mut self, list_context: &ListRefreshViewContext) {
        let ListRefreshViewContext {
            commit_hash,
            selected,
            height,
            scroll_to_top,
            search_options,
            search_context,
            graph_visible,
            detail_height: _, // restored by `App`, which owns it across views
            detail_message_lines: _,
        } = list_context;
        let list_state = self.as_mut_list_state();
        list_state.restore_search_options(*search_options);
        list_state.restore_graph_visible(*graph_visible);
        list_state.reset_height(*height);
        if *scroll_to_top {
            list_state.select_first();
        } else {
            list_state.select_commit_hash(&CommitHash::from(commit_hash.as_str()));
            for _ in 0..*selected {
                list_state.scroll_up();
            }
        }
        if let Some(search_context) = search_context {
            list_state.restore_search(search_context);
        }
    }
}
