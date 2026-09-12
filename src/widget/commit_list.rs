use std::rc::Rc;

use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use laurier::highlight::highlight_matched_text;
use once_cell::sync::Lazy;
use ratatui::{
    buffer::Buffer,
    crossterm::event::{Event, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{List, ListItem, StatefulWidget, Widget},
};
use rustc_hash::{FxHashMap, FxHashSet};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    app::AppContext,
    color::ColorTheme,
    config::UserListColumnType,
    git::{Commit, CommitHash, Head, Ref},
    graph::GraphImageManager,
    protocol::PreparedImage,
    search::{SearchOptions, SearchTarget},
};

static FUZZY_MATCHER: Lazy<SkimMatcherV2> = Lazy::new(|| SkimMatcherV2::default().respect_case());

const ELLIPSIS: &str = "...";

#[derive(Debug)]
pub struct CommitInfo<'a> {
    commit: &'a Commit,
    refs: Vec<&'a Ref>,
    graph_color: Color,
    is_merge_base: bool,
    is_head: bool,
    /// 1-based position in the revspec when this commit is one of its tips.
    tip_ordinal: Option<usize>,
    /// The graph row rendered as text with lane colours already resolved, empty unless the text
    /// renderer is in use.
    graph_text: Vec<Option<(char, Color)>>,
}

impl<'a> CommitInfo<'a> {
    pub fn new(
        commit: &'a Commit,
        refs: Vec<&'a Ref>,
        graph_color: Color,
        is_merge_base: bool,
        is_head: bool,
        tip_ordinal: Option<usize>,
        graph_text: Vec<Option<(char, Color)>>,
    ) -> Self {
        Self {
            commit,
            refs,
            graph_color,
            is_merge_base,
            is_head,
            tip_ordinal,
            graph_text,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchState {
    Inactive,
    Searching {
        start_index: usize,
        match_index: usize,
    },
    Applied {
        match_index: usize,
        total_match: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRefreshContext {
    query: String,
}

impl SearchState {
    fn update_match_index(&mut self, index: usize) {
        match self {
            SearchState::Searching { match_index, .. } => *match_index = index,
            SearchState::Applied { match_index, .. } => *match_index = index,
            _ => {}
        }
    }
}

#[derive(Debug, Default, Clone)]
struct SearchMatch {
    refs: FxHashMap<String, SearchMatchPosition>,
    subject: Option<SearchMatchPosition>,
    author_name: Option<SearchMatchPosition>,
    commit_hash: Option<SearchMatchPosition>,
    match_index: usize, // 1-based
}

impl SearchMatch {
    fn set(&mut self, c: &Commit, refs: &[&Ref], matcher: &SearchMatcher, target: SearchTarget) {
        self.refs = if matches!(target, SearchTarget::All | SearchTarget::Ref) {
            refs.iter()
                .filter(|r| !matches!(*r, Ref::Stash { .. }))
                .filter_map(|r| {
                    matcher
                        .matched_position(r.name())
                        .map(|pos| (r.name().into(), pos))
                })
                .collect()
        } else {
            FxHashMap::default()
        };
        self.subject = if matches!(target, SearchTarget::All | SearchTarget::Subject) {
            matcher.matched_position(&c.subject)
        } else {
            None
        };
        self.author_name = if matches!(target, SearchTarget::All | SearchTarget::Author) {
            matcher.matched_position(&c.author_name)
        } else {
            None
        };
        self.commit_hash = if matches!(target, SearchTarget::All | SearchTarget::Hash) {
            matcher.matched_position(c.commit_hash.as_short_hash())
        } else {
            None
        };
        self.match_index = 0;
    }

    fn matched(&self) -> bool {
        !self.refs.is_empty()
            || self.subject.is_some()
            || self.author_name.is_some()
            || self.commit_hash.is_some()
    }

    fn clear(&mut self) {
        self.refs.clear();
        self.subject = None;
        self.author_name = None;
        self.commit_hash = None;
    }
}

#[derive(Debug, Default, Clone)]
struct SearchMatchPosition {
    matched_indices: Vec<usize>,
}

impl SearchMatchPosition {
    fn new(matched_indices: Vec<usize>) -> Self {
        Self { matched_indices }
    }
}

struct SearchMatcher {
    query: String,
    ignore_case: bool,
    fuzzy: bool,
}

impl SearchMatcher {
    fn new(query: &str, ignore_case: bool, fuzzy: bool) -> Self {
        let query = if ignore_case {
            query.to_lowercase()
        } else {
            query.into()
        };
        Self {
            query,
            ignore_case,
            fuzzy,
        }
    }

    fn matched_position(&self, s: &str) -> Option<SearchMatchPosition> {
        if self.fuzzy {
            let result = if self.ignore_case {
                FUZZY_MATCHER.fuzzy_indices(&s.to_lowercase(), &self.query)
            } else {
                FUZZY_MATCHER.fuzzy_indices(s, &self.query)
            };
            result
                .map(|(_, indices)| indices)
                .map(SearchMatchPosition::new)
        } else {
            let result = if self.ignore_case {
                s.to_lowercase().find(&self.query)
            } else {
                s.find(&self.query)
            };
            result
                .map(|p| (p..(p + self.query.len())).collect())
                .map(SearchMatchPosition::new)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipJump {
    Selected,
    /// Fewer than two plain revisions were given, so there are no tips to rotate through.
    NoTips,
    /// Tips exist but every one was cut from the rendered commits, e.g. by `--max-count`.
    OutsideRenderedCommits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeBaseJump {
    Selected,
    /// The revspec did not name exactly two revisions, so there is no base to speak of.
    NotScoped,
    /// A base exists but was cut from the rendered commits, e.g. by `--max-count`.
    OutsideRenderedCommits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphToggleResult {
    Shown,
    Hidden,
    /// The graph is hidden and the terminal is too narrow to show it.
    TerminalTooSmall,
}

#[derive(Debug)]
pub struct CommitListState<'a> {
    commits: Vec<CommitInfo<'a>>,
    commit_hash_set: FxHashSet<&'a CommitHash>,
    graph_image_manager: GraphImageManager<'a>,
    graph_area_width: u16,
    graph_visible: bool,
    graph_toggleable: bool,
    text_graph: bool,
    head: &'a Head,
    merge_base: Option<&'a CommitHash>,
    /// Indexes into `commits` of the revspec tips, in revspec order.
    tip_indexes: Vec<usize>,

    ref_name_to_commit_index_map: FxHashMap<&'a str, usize>,

    search_state: SearchState,
    search_options: SearchOptions,
    search_input: Input,
    search_matches: Vec<SearchMatch>,

    selected: usize,
    offset: usize,
    total: usize,
    height: usize,
}

impl<'a> CommitListState<'a> {
    pub fn new(
        commits: Vec<CommitInfo<'a>>,
        graph_image_manager: GraphImageManager<'a>,
        graph_area_width: u16,
        graph_visible: bool,
        graph_toggleable: bool,
        text_graph: bool,
        head: &'a Head,
        merge_base: Option<&'a CommitHash>,
        tip_indexes: Vec<usize>,
        ref_name_to_commit_index_map: FxHashMap<&'a str, usize>,
        search_options: SearchOptions,
    ) -> CommitListState<'a> {
        let total = commits.len();
        let commit_hash_set = commits.iter().map(|c| &c.commit.commit_hash).collect();
        CommitListState {
            commits,
            commit_hash_set,
            graph_image_manager,
            graph_area_width,
            graph_visible,
            graph_toggleable,
            text_graph,
            head,
            merge_base,
            tip_indexes,
            ref_name_to_commit_index_map,
            search_state: SearchState::Inactive,
            search_options,
            search_input: Input::default(),
            search_matches: vec![SearchMatch::default(); total],
            selected: 0,
            offset: 0,
            total,
            height: 0,
        }
    }

    pub fn graph_area_cell_width(&self) -> u16 {
        if !self.graph_visible {
            return 0; // the column collapses entirely, padding included
        }
        self.graph_area_width
    }

    pub fn select_merge_base(&mut self) -> MergeBaseJump {
        let Some(merge_base) = self.merge_base else {
            return MergeBaseJump::NotScoped;
        };
        if !self.commit_hash_set.contains(merge_base) {
            return MergeBaseJump::OutsideRenderedCommits;
        }
        let merge_base = merge_base.clone();
        self.select_commit_hash(&merge_base);
        MergeBaseJump::Selected
    }

    /// Selects the next revspec tip below the current selection, wrapping at the bottom, the way
    /// `go_to_next` walks search matches.
    pub fn select_next_tip(&mut self) -> TipJump {
        if self.tip_indexes.is_empty() {
            return TipJump::NoTips;
        }
        let mut rendered: Vec<usize> = self
            .tip_indexes
            .iter()
            .copied()
            .filter(|i| *i < self.commits.len())
            .collect();
        if rendered.is_empty() {
            return TipJump::OutsideRenderedCommits;
        }
        rendered.sort_unstable();
        rendered.dedup();

        let current = self.offset + self.selected;
        let target = rendered
            .iter()
            .copied()
            .find(|i| *i > current)
            .unwrap_or(rendered[0]);
        let hash = self.commits[target].commit.commit_hash.clone();
        self.select_commit_hash(&hash);
        TipJump::Selected
    }

    pub fn head(&self) -> &'a Head {
        self.head
    }

    pub fn graph_visible(&self) -> bool {
        self.graph_visible
    }

    pub fn restore_graph_visible(&mut self, visible: bool) {
        // A terminal too narrow for the graph leaves it permanently hidden, so a restored `true`
        // from before a refresh must not override that.
        self.graph_visible = visible && self.graph_toggleable;
    }

    pub fn toggle_graph(&mut self) -> GraphToggleResult {
        if !self.graph_visible && !self.graph_toggleable {
            return GraphToggleResult::TerminalTooSmall;
        }
        self.graph_visible = !self.graph_visible;
        if self.graph_visible {
            GraphToggleResult::Shown
        } else {
            GraphToggleResult::Hidden
        }
    }

    pub fn update_height(&mut self, height: usize) {
        self.height = height;

        if self.height == 0 {
            // No row fits, so there is nothing to clamp the selection against, and the clamping
            // below would underflow. The selection is re-clamped once the area has rows again.
            // Reachable whenever the terminal is shorter than the status line plus one row.
            return;
        }

        if self.total > self.height && self.total - self.height < self.offset {
            let diff = self.offset - (self.total - self.height);
            self.selected += diff;
            self.offset -= diff;
        }
        if self.selected >= self.height {
            let diff = self.selected - self.height + 1;
            self.selected -= diff;
            self.offset += diff;
        }
    }

    pub fn ensure_visible_graph_uploaded(&mut self) {
        if !self.graph_visible {
            return; // nothing is drawn, so nothing needs to reach the terminal
        }
        if self.text_graph {
            return; // text rows are plain cells; no image ever reaches the terminal
        }
        self.commits
            .iter()
            .skip(self.offset)
            .take(self.height)
            .for_each(|commit_info| {
                self.graph_image_manager
                    .ensure_uploaded(&commit_info.commit.commit_hash);
            });
    }

    pub fn drain_pending_graph_uploads(&mut self) -> Vec<String> {
        self.graph_image_manager.drain_pending_uploads()
    }

    pub fn graph_image_ids_sorted(&self) -> Vec<u32> {
        let mut image_ids: Vec<u32> = self
            .graph_image_manager
            .image_ids()
            .iter()
            .copied()
            .collect();
        image_ids.sort_unstable();
        image_ids
    }

    pub fn select_next(&mut self) {
        if self.selected < (self.total - 1).min(self.height - 1) {
            self.selected += 1;
        } else if self.selected + self.offset < self.total - 1 {
            self.offset += 1;
        }
    }

    pub fn select_parent(&mut self) {
        if let Some(target_commit) = self.selected_commit_parent_hash().cloned() {
            if self.commit_hash_set.contains(&target_commit) {
                while target_commit.as_str() != self.selected_commit_hash().as_str() {
                    self.select_next();
                }
            }
        }
    }

    pub fn selected_commit_parent_hash(&self) -> Option<&CommitHash> {
        self.commits[self.current_selected_index()]
            .commit
            .parent_commit_hashes
            .first()
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        } else if self.offset > 0 {
            self.offset -= 1;
        }
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
        self.offset = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = (self.height - 1).min(self.total - 1);
        if self.height < self.total {
            self.offset = self.total - self.height;
        }
    }

    pub fn scroll_down(&mut self) {
        if self.offset + self.height < self.total {
            self.offset += 1;
            if self.selected > 0 {
                self.selected -= 1;
            }
        }
    }

    pub fn scroll_up(&mut self) {
        if self.offset > 0 {
            self.offset -= 1;
            if self.selected < self.height - 1 {
                self.selected += 1;
            }
        }
    }

    pub fn scroll_down_page(&mut self) {
        self.scroll_down_height(self.height);
    }

    pub fn scroll_up_page(&mut self) {
        self.scroll_up_height(self.height);
    }

    pub fn scroll_down_half(&mut self) {
        self.scroll_down_height(self.height / 2);
    }

    pub fn scroll_up_half(&mut self) {
        self.scroll_up_height(self.height / 2);
    }

    fn scroll_down_height(&mut self, scroll_height: usize) {
        if self.offset + self.height + scroll_height < self.total {
            self.offset += scroll_height;
        } else {
            let old_offset = self.offset;
            let size = self.height.min(self.total);
            self.offset = self.total - size;
            self.selected += scroll_height - (self.offset - old_offset);
            if self.selected >= size {
                self.selected = size - 1;
            }
        }
    }

    fn scroll_up_height(&mut self, scroll_height: usize) {
        if self.offset > scroll_height {
            self.offset -= scroll_height;
        } else {
            let old_offset = self.offset;
            self.offset = 0;
            self.selected = self
                .selected
                .saturating_sub(scroll_height - (old_offset - self.offset));
        }
    }

    pub fn select_high(&mut self) {
        self.selected = 0;
    }

    pub fn select_middle(&mut self) {
        if self.total > self.height {
            self.selected = self.height / 2;
        } else {
            self.selected = self.total / 2;
        }
    }

    pub fn select_low(&mut self) {
        if self.total > self.height {
            self.selected = self.height - 1;
        } else {
            self.selected = self.total - 1;
        }
    }

    fn select_index(&mut self, index: usize) {
        if index < self.total {
            if self.total > self.height {
                self.selected = 0;
                self.offset = index;
            } else {
                self.selected = index;
            }
        }
    }

    pub fn select_next_match(&mut self) {
        self.select_next_match_index(self.current_selected_index());
    }

    pub fn select_prev_match(&mut self) {
        self.select_prev_match_index(self.current_selected_index());
    }

    pub fn selected_commit_hash(&self) -> &CommitHash {
        &self.commits[self.current_selected_index()]
            .commit
            .commit_hash
    }

    fn current_selected_index(&self) -> usize {
        self.offset + self.selected
    }

    pub fn current_list_status(&self) -> (usize, usize, usize) {
        (self.selected, self.offset, self.height)
    }

    pub fn reset_height(&mut self, height: usize) {
        self.height = height;
    }

    pub fn select_ref(&mut self, ref_name: &str) {
        if let Some(&index) = self.ref_name_to_commit_index_map.get(ref_name) {
            if self.total > self.height {
                self.selected = 0;
                self.offset = index;
            } else {
                self.selected = index;
            }
        }
    }

    pub fn select_commit_hash(&mut self, commit_hash: &CommitHash) {
        if !self.commit_hash_set.contains(commit_hash) {
            return;
        }
        for (i, commit_info) in self.commits.iter().enumerate() {
            if commit_info.commit.commit_hash == *commit_hash {
                if self.total > self.height {
                    self.selected = 0;
                    self.offset = i;
                } else {
                    self.selected = i;
                }
                break;
            }
        }
    }

    pub fn search_state(&self) -> SearchState {
        self.search_state
    }

    pub fn search_options(&self) -> SearchOptions {
        self.search_options
    }

    pub fn restore_search_options(&mut self, options: SearchOptions) {
        self.search_options = options;
    }

    pub fn start_search(&mut self) {
        if let SearchState::Inactive | SearchState::Applied { .. } = self.search_state {
            self.search_state = SearchState::Searching {
                start_index: self.current_selected_index(),
                match_index: 0,
            };
            self.search_input.reset();
            self.clear_search_matches();
        }
    }

    pub fn handle_search_input(&mut self, key: KeyEvent) {
        if let SearchState::Searching { start_index, .. } = self.search_state {
            self.search_input.handle_event(&Event::Key(key));
            self.update_search_matches();
            self.select_current_or_next_match_index(start_index);
        }
    }

    pub fn apply_search(&mut self) {
        if let SearchState::Searching { match_index, .. } = self.search_state {
            if self.search_input.value().is_empty() {
                self.search_state = SearchState::Inactive;
            } else {
                let total_match = self.search_matches.iter().filter(|m| m.matched()).count();
                self.search_state = SearchState::Applied {
                    match_index,
                    total_match,
                };
            }
        }
    }

    pub fn search_refresh_context(&self) -> Option<SearchRefreshContext> {
        if let SearchState::Applied { .. } = self.search_state {
            Some(SearchRefreshContext {
                query: self.search_input.value().into(),
            })
        } else {
            None
        }
    }

    pub fn restore_search(&mut self, context: &SearchRefreshContext) {
        self.search_input = Input::new(context.query.clone());
        self.update_search_matches();

        let total_match = self.search_matches.iter().filter(|m| m.matched()).count();
        self.search_state = SearchState::Applied {
            // The selected commit may not match after refresh; next/previous updates this value.
            match_index: 0,
            total_match,
        };

        if total_match > 0 {
            let current_index = self.current_selected_index();
            if self.search_matches[current_index].matched() {
                self.search_state
                    .update_match_index(self.search_matches[current_index].match_index);
            }
        }
    }

    pub fn cancel_search(&mut self) {
        if let SearchState::Searching { .. } | SearchState::Applied { .. } = self.search_state {
            self.search_state = SearchState::Inactive;
            self.search_input.reset();
            self.clear_search_matches();
        }
    }

    pub fn toggle_ignore_case(&mut self) {
        self.search_options.ignore_case = !self.search_options.ignore_case;
        self.update_search_after_options_change();
    }

    pub fn toggle_fuzzy(&mut self) {
        self.search_options.fuzzy = !self.search_options.fuzzy;
        self.update_search_after_options_change();
    }

    pub fn toggle_search_target(&mut self) {
        self.search_options.target = self.search_options.target.next();
        self.update_search_after_options_change();
    }

    pub fn search_query_string(&self) -> Option<String> {
        if let SearchState::Searching { .. } = self.search_state {
            let query = self.search_input.value();
            Some(format!("/{query}"))
        } else {
            None
        }
    }

    pub fn matched_query_string(&self) -> Option<(String, bool)> {
        if let SearchState::Applied {
            match_index,
            total_match,
            ..
        } = self.search_state
        {
            let query = self.search_input.value();
            if total_match == 0 {
                let msg = format!("No matches found (query: \"{query}\")");
                Some((msg, false))
            } else {
                let msg = format!("Match {match_index} of {total_match} (query: \"{query}\")");
                Some((msg, true))
            }
        } else {
            None
        }
    }

    pub fn search_query_cursor_position(&self) -> u16 {
        self.search_input.visual_cursor() as u16 + 1 // add 1 for "/"
    }

    fn update_search_matches(&mut self) {
        let matcher = SearchMatcher::new(
            self.search_input.value(),
            self.search_options.ignore_case,
            self.search_options.fuzzy,
        );
        let mut match_index = 1;
        for (i, commit_info) in self.commits.iter().enumerate() {
            let m = &mut self.search_matches[i];
            m.set(
                commit_info.commit,
                commit_info.refs.as_slice(),
                &matcher,
                self.search_options.target,
            );
            if m.matched() {
                m.match_index = match_index;
                match_index += 1;
            }
        }
    }

    fn update_search_after_options_change(&mut self) {
        match self.search_state {
            SearchState::Inactive => {}
            SearchState::Searching { start_index, .. } => {
                self.update_search_matches();
                self.select_current_or_next_match_index(start_index);
            }
            SearchState::Applied { .. } => {
                let current_index = self.current_selected_index();
                self.update_search_matches();
                let total_match = self.search_matches.iter().filter(|m| m.matched()).count();
                self.search_state = SearchState::Applied {
                    match_index: 0,
                    total_match,
                };
                if total_match > 0 {
                    self.select_current_or_next_match_index(current_index);
                }
            }
        }
    }

    fn clear_search_matches(&mut self) {
        self.search_matches.iter_mut().for_each(|m| m.clear());
    }

    fn select_current_or_next_match_index(&mut self, current_index: usize) {
        if self.search_matches[current_index].matched() {
            self.select_index(current_index);
            self.search_state
                .update_match_index(self.search_matches[current_index].match_index);
        } else {
            self.select_next_match_index(current_index)
        }
    }

    fn select_next_match_index(&mut self, current_index: usize) {
        let mut i = (current_index + 1) % self.total;
        while i != current_index {
            if self.search_matches[i].matched() {
                self.select_index(i);
                self.search_state
                    .update_match_index(self.search_matches[i].match_index);
                return;
            }
            if i == self.total - 1 {
                i = 0;
            } else {
                i += 1;
            }
        }
    }

    fn select_prev_match_index(&mut self, current_index: usize) {
        let mut i = (current_index + self.total - 1) % self.total;
        while i != current_index {
            if self.search_matches[i].matched() {
                self.select_index(i);
                self.search_state
                    .update_match_index(self.search_matches[i].match_index);
                return;
            }
            if i == 0 {
                i = self.total - 1;
            } else {
                i -= 1;
            }
        }
    }

    fn prepared_image(&self, commit_info: &'a CommitInfo) -> &PreparedImage {
        self.graph_image_manager
            .prepared_image(&commit_info.commit.commit_hash)
    }
}

pub struct CommitList<'a> {
    ctx: Rc<AppContext>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> CommitList<'a> {
    pub fn new(ctx: Rc<AppContext>) -> Self {
        Self {
            ctx,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> StatefulWidget for CommitList<'a> {
    type State = CommitListState<'a>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        self.update_state(area, state);

        let constraints = calc_cell_widths(
            area.width,
            self.ctx.ui_config.list.subject_min_width,
            state.graph_area_cell_width(),
            self.ctx.ui_config.list.name_width,
            self.ctx.ui_config.list.date_width,
            &self.ctx.ui_config.list.columns,
        );
        let chunks = Layout::horizontal(constraints).split(area);

        for (i, col) in self.ctx.ui_config.list.columns.iter().enumerate() {
            match col {
                UserListColumnType::Graph => {
                    self.render_graph(buf, chunks[i], state);
                }
                UserListColumnType::Marker => {
                    self.render_marker(buf, chunks[i], state);
                }
                UserListColumnType::Subject => {
                    self.render_subject(buf, chunks[i], state);
                }
                UserListColumnType::Name => {
                    self.render_name(buf, chunks[i], state);
                }
                UserListColumnType::Hash => {
                    self.render_hash(buf, chunks[i], state);
                }
                UserListColumnType::Date => {
                    self.render_date(buf, chunks[i], state);
                }
            }
        }
    }
}

impl CommitList<'_> {
    fn update_state(&self, area: Rect, state: &mut CommitListState) {
        state.update_height(area.height as usize);
    }

    fn render_graph(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        if area.is_empty() {
            return;
        }
        if self.ctx.graph_renderer.text_style().is_some() {
            self.render_graph_text(buf, area, state);
            return;
        }
        self.rendering_commit_info_iter(state)
            .for_each(|(i, commit_info)| {
                let prepared_image = state.prepared_image(commit_info);
                let max_graph_width = area.width.saturating_sub(1) as usize;
                let y = area.top() + i as u16;
                for (x, image_cell) in prepared_image
                    .cells()
                    .iter()
                    .take(max_graph_width)
                    .enumerate()
                {
                    let cell = &mut buf[(area.left() + x as u16, y)];
                    cell.set_symbol(image_cell.symbol());
                    cell.set_style(image_cell.style());
                    cell.set_skip(image_cell.skip());
                }
            });
    }

    /// Draws the precomputed text rows. Unlike the image path there is no protocol and no upload,
    /// so this works in any terminal.
    fn render_graph_text(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        self.rendering_commit_info_iter(state)
            .for_each(|(i, commit_info)| {
                let y = area.top() + i as u16;
                for (x, text_cell) in commit_info
                    .graph_text
                    .iter()
                    .take(area.width as usize)
                    .enumerate()
                {
                    let Some((symbol, color)) = text_cell else {
                        continue;
                    };
                    let cell = &mut buf[(area.left() + x as u16, y)];
                    cell.set_symbol(&symbol.to_string());
                    cell.set_fg(*color);
                }
            });
    }

    fn render_marker(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        if area.is_empty() {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_info_iter(state)
            .map(|(_, commit_info)| {
                let (symbol, color) = marker_symbol(commit_info, &self.ctx.color_theme);
                ListItem::new(symbol.fg(color).bold())
            })
            .collect();
        Widget::render(List::new(items), area, buf)
    }

    fn render_subject(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        let max_width = (area.width as usize).saturating_sub(2);
        if area.is_empty() || max_width == 0 {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_info_iter(state)
            .map(|(i, commit_info)| {
                let mut spans = refs_spans(
                    commit_info,
                    state.head,
                    &state.search_matches[state.offset + i].refs,
                    &self.ctx.color_theme,
                );
                let ref_spans_width: usize = spans.iter().map(|s| s.width()).sum();
                let max_width = max_width.saturating_sub(ref_spans_width);
                let commit = commit_info.commit;
                if max_width > ELLIPSIS.len() {
                    let truncate = console::measure_text_width(&commit.subject) > max_width;
                    let subject = if truncate {
                        console::truncate_str(&commit.subject, max_width, ELLIPSIS).to_string()
                    } else {
                        commit.subject.to_string()
                    };

                    // The merge base is bold so it can be found without scanning the one-cell
                    // marker column. The selected row's own style still layers on top.
                    let modifier = if commit_info.is_merge_base {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    };
                    let sub_spans =
                        if let Some(pos) = state.search_matches[state.offset + i].subject.clone() {
                            highlighted_spans(
                                subject.into(),
                                pos,
                                self.ctx.color_theme.list_subject_fg,
                                modifier,
                                &self.ctx.color_theme,
                                truncate,
                            )
                        } else {
                            vec![subject
                                .fg(self.ctx.color_theme.list_subject_fg)
                                .add_modifier(modifier)]
                        };

                    spans.extend(sub_spans)
                }
                self.to_commit_list_item(i, spans, state)
            })
            .collect();
        Widget::render(List::new(items), area, buf);
    }

    fn render_name(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        let max_width = (area.width as usize).saturating_sub(2);
        if area.is_empty() || max_width == 0 {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_iter(state)
            .map(|(i, commit)| {
                let truncate = console::measure_text_width(&commit.author_name) > max_width;
                let name = if truncate {
                    console::truncate_str(&commit.author_name, max_width, ELLIPSIS).to_string()
                } else {
                    commit.author_name.to_string()
                };
                let spans =
                    if let Some(pos) = state.search_matches[state.offset + i].author_name.clone() {
                        highlighted_spans(
                            name.into(),
                            pos,
                            self.ctx.color_theme.list_name_fg,
                            Modifier::empty(),
                            &self.ctx.color_theme,
                            truncate,
                        )
                    } else {
                        vec![name.fg(self.ctx.color_theme.list_name_fg)]
                    };
                self.to_commit_list_item(i, spans, state)
            })
            .collect();
        Widget::render(List::new(items), area, buf);
    }

    fn render_hash(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        if area.is_empty() {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_iter(state)
            .map(|(i, commit)| {
                let hash = commit.commit_hash.as_short_hash();
                let spans =
                    if let Some(pos) = state.search_matches[state.offset + i].commit_hash.clone() {
                        highlighted_spans(
                            hash.into(),
                            pos,
                            self.ctx.color_theme.list_hash_fg,
                            Modifier::empty(),
                            &self.ctx.color_theme,
                            false,
                        )
                    } else {
                        vec![hash.fg(self.ctx.color_theme.list_hash_fg)]
                    };
                self.to_commit_list_item(i, spans, state)
            })
            .collect();
        Widget::render(List::new(items), area, buf);
    }

    fn render_date(&self, buf: &mut Buffer, area: Rect, state: &CommitListState) {
        if area.is_empty() {
            return;
        }
        let items: Vec<ListItem> = self
            .rendering_commit_iter(state)
            .map(|(i, commit)| {
                let date = &commit.author_date;
                let date_str = if self.ctx.ui_config.list.date_local {
                    let local = date.with_timezone(&chrono::Local);
                    local
                        .format(&self.ctx.ui_config.list.date_format)
                        .to_string()
                } else {
                    date.format(&self.ctx.ui_config.list.date_format)
                        .to_string()
                };
                self.to_commit_list_item(
                    i,
                    vec![date_str.fg(self.ctx.color_theme.list_date_fg)],
                    state,
                )
            })
            .collect();
        Widget::render(List::new(items), area, buf);
    }

    fn rendering_commit_info_iter<'a>(
        &'a self,
        state: &'a CommitListState,
    ) -> impl Iterator<Item = (usize, &'a CommitInfo<'a>)> {
        state
            .commits
            .iter()
            .skip(state.offset)
            .take(state.height)
            .enumerate()
    }

    fn rendering_commit_iter<'a>(
        &'a self,
        state: &'a CommitListState,
    ) -> impl Iterator<Item = (usize, &'a Commit)> {
        self.rendering_commit_info_iter(state)
            .map(|(i, commit_info)| (i, commit_info.commit))
    }

    fn to_commit_list_item<'a, 'b>(
        &'b self,
        i: usize,
        spans: Vec<Span<'a>>,
        state: &'b CommitListState,
    ) -> ListItem<'a> {
        let mut spans = spans;
        spans.insert(0, Span::raw(" "));
        spans.push(Span::raw(" "));
        let mut line = Line::from(spans);
        if i == state.selected {
            line = line
                .bg(self.ctx.color_theme.list_selected_bg)
                .fg(self.ctx.color_theme.list_selected_fg);
        }
        ListItem::new(line)
    }
}

/// The marker column holds one cell, so the three facts that can land on a row are ranked.
///
/// A merge base outranks a tip because it is the rarer fact, and both outrank HEAD because HEAD
/// already shows as `(HEAD -> ...)` in the subject while they have no other indicator.
fn marker_symbol(commit_info: &CommitInfo, color_theme: &ColorTheme) -> (String, Color) {
    if commit_info.is_merge_base {
        return ("\u{25c6}".into(), color_theme.list_marker_base_fg); // ◆
    }
    if let Some(ordinal) = commit_info.tip_ordinal {
        let symbol = match char::from_digit(ordinal as u32, 10) {
            Some(digit) => digit.to_string(),
            // More tips than the one cell can name; they still rotate.
            None => "\u{25b6}".into(), // ▶
        };
        return (symbol, color_theme.list_marker_tip_fg);
    }
    if commit_info.is_head {
        return ("@".into(), color_theme.list_marker_head_fg);
    }
    ("\u{2502}".into(), commit_info.graph_color) // │
}

fn refs_spans<'a>(
    commit_info: &'a CommitInfo,
    head: &'a Head,
    refs_matches: &'a FxHashMap<String, SearchMatchPosition>,
    color_theme: &'a ColorTheme,
) -> Vec<Span<'a>> {
    let refs = &commit_info.refs;

    if refs.len() == 1 {
        if let Ref::Stash { name, .. } = refs[0] {
            return vec![
                Span::raw(name).fg(color_theme.list_ref_stash_fg).bold(),
                Span::raw(" "),
            ];
        }
    }

    let ref_spans: Vec<(Vec<Span>, &String)> = refs
        .iter()
        .filter_map(|r| match r {
            Ref::Branch { name, .. } => {
                let fg = color_theme.list_ref_branch_fg;
                Some((name, fg))
            }
            Ref::RemoteBranch { name, .. } => {
                let fg = color_theme.list_ref_remote_branch_fg;
                Some((name, fg))
            }
            Ref::Tag { name, .. } => {
                let fg = color_theme.list_ref_tag_fg;
                Some((name, fg))
            }
            Ref::Stash { .. } => None,
        })
        .map(|(name, fg)| {
            let spans = refs_matches
                .get(name)
                .map(|pos| {
                    highlighted_spans(
                        name.into(),
                        pos.clone(),
                        fg,
                        Modifier::BOLD,
                        color_theme,
                        false,
                    )
                })
                .unwrap_or_else(|| vec![Span::raw(name).fg(fg).bold()]);
            (spans, name)
        })
        .collect();

    let mut spans = vec![Span::raw("(").fg(color_theme.list_ref_paren_fg).bold()];

    if let Head::Detached { target } = head {
        if commit_info.commit.commit_hash == *target {
            spans.push(Span::raw("HEAD").fg(color_theme.list_head_fg).bold());
            if !ref_spans.is_empty() {
                spans.push(Span::raw(", ").fg(color_theme.list_ref_paren_fg).bold());
            }
        }
    }

    for (i, ss) in ref_spans.into_iter().enumerate() {
        let (ref_spans, ref_name) = ss;
        if let Head::Branch { name } = head {
            if ref_name == name {
                spans.push(Span::raw("HEAD -> ").fg(color_theme.list_head_fg).bold());
            }
        }
        spans.extend(ref_spans);
        if i < refs.len() - 1 {
            spans.push(Span::raw(", ").fg(color_theme.list_ref_paren_fg).bold());
        }
    }

    spans.push(Span::raw(") ").fg(color_theme.list_ref_paren_fg).bold());

    if spans.len() == 2 {
        spans.clear(); // contains only "(" and ")", so clear it
    }

    spans
}

fn highlighted_spans(
    s: Span<'_>,
    pos: SearchMatchPosition,
    base_fg: Color,
    base_modifier: Modifier,
    color_theme: &ColorTheme,
    truncate: bool,
) -> Vec<Span<'static>> {
    let mut hm = highlight_matched_text(vec![s])
        .matched_indices(pos.matched_indices)
        .not_matched_style(Style::default().fg(base_fg).add_modifier(base_modifier))
        .matched_style(
            Style::default()
                .fg(color_theme.list_match_fg)
                .bg(color_theme.list_match_bg)
                .add_modifier(base_modifier),
        );
    if truncate {
        hm = hm.ellipsis(ELLIPSIS);
    }
    hm.into_spans()
}

fn calc_cell_widths(
    area_width: u16,
    subject_min_width: u16,
    graph_width: u16,
    name_width: u16,
    date_width: u16,
    columns: &[UserListColumnType],
) -> Vec<Constraint> {
    let pad = 2;
    let (
        mut graph_cell_width,
        mut marker_cell_width,
        mut name_cell_width,
        mut hash_cell_width,
        mut date_cell_width,
    ) = (0, 0, 0, 0, 0);

    for col in columns {
        match col {
            UserListColumnType::Graph => {
                graph_cell_width = graph_width;
            }
            UserListColumnType::Marker => {
                marker_cell_width = 1;
            }
            UserListColumnType::Name => {
                name_cell_width = name_width + pad;
            }
            UserListColumnType::Hash => {
                hash_cell_width = 7 + pad;
            }
            UserListColumnType::Date => {
                date_cell_width = date_width + pad;
            }
            UserListColumnType::Subject => {}
        }
    }

    let mut total_width = graph_cell_width
        + marker_cell_width
        + hash_cell_width
        + name_cell_width
        + date_cell_width
        + subject_min_width;

    if total_width > area_width {
        total_width = total_width.saturating_sub(name_cell_width);
        name_cell_width = 0;
    }
    if total_width > area_width {
        total_width = total_width.saturating_sub(date_cell_width);
        date_cell_width = 0;
    }
    if total_width > area_width {
        hash_cell_width = 0;
    }

    let mut constraints = Vec::new();
    for col in columns {
        match col {
            UserListColumnType::Graph => {
                constraints.push(Constraint::Length(graph_cell_width));
            }
            UserListColumnType::Marker => {
                constraints.push(Constraint::Length(marker_cell_width));
            }
            UserListColumnType::Subject => {
                constraints.push(Constraint::Min(0));
            }
            UserListColumnType::Name => {
                constraints.push(Constraint::Length(name_cell_width));
            }
            UserListColumnType::Hash => {
                constraints.push(Constraint::Length(hash_cell_width));
            }
            UserListColumnType::Date => {
                constraints.push(Constraint::Length(date_cell_width));
            }
        }
    }
    constraints
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::crossterm::event::KeyCode;

    use crate::{
        color::GraphColorSet,
        config::GraphColorConfig,
        git::Repository,
        graph::{calc_graph, CellWidthType, GraphImageWidthMode, GraphStyle},
        protocol::ImageProtocol,
    };

    use super::*;

    fn with_commit_list_state<R>(
        subjects: &[&str],
        f: impl FnOnce(&mut CommitListState<'_>) -> R,
    ) -> R {
        with_full_commit_list_state(subjects, 0, true, true, None, Vec::new(), f)
    }

    fn with_graph_commit_list_state<R>(
        subjects: &[&str],
        graph_cell_width: u16,
        graph_visible: bool,
        graph_toggleable: bool,
        f: impl FnOnce(&mut CommitListState<'_>) -> R,
    ) -> R {
        with_full_commit_list_state(
            subjects,
            graph_cell_width,
            graph_visible,
            graph_toggleable,
            None,
            Vec::new(),
            f,
        )
    }

    /// `merge_base` is a raw hash rather than an index so that a base outside the rendered
    /// commits can be set up, which is the case `MergeBaseJump::OutsideRenderedCommits` covers.
    fn with_full_commit_list_state<R>(
        subjects: &[&str],
        graph_cell_width: u16,
        graph_visible: bool,
        graph_toggleable: bool,
        merge_base: Option<CommitHash>,
        tip_indexes: Vec<usize>,
        f: impl FnOnce(&mut CommitListState<'_>) -> R,
    ) -> R {
        let commits: Vec<Commit> = subjects
            .iter()
            .enumerate()
            .map(|(i, subject)| Commit {
                commit_hash: CommitHash::from(format!("{:040x}", i + 1).as_str()),
                subject: (*subject).into(),
                ..Commit::default()
            })
            .collect();
        let commit_hashes = commits.iter().map(|c| c.commit_hash.clone()).collect();
        let commit_map = commits
            .into_iter()
            .map(|c| (c.commit_hash.clone(), c))
            .collect();
        let repository = Repository::new(
            PathBuf::new(),
            commit_map,
            FxHashMap::default(),
            FxHashMap::default(),
            FxHashMap::default(),
            Head::None,
            commit_hashes,
            merge_base,
            Vec::new(),
        );
        let graph = calc_graph(&repository);
        let graph_color_set = GraphColorSet::new(&GraphColorConfig::default());
        let graph_image_manager = GraphImageManager::new(
            &graph,
            &graph_color_set,
            CellWidthType::Double,
            GraphStyle::Rounded,
            GraphImageWidthMode::Compact,
            ImageProtocol::Iterm2,
        );
        let commit_infos = graph
            .commits
            .iter()
            .map(|commit| {
                let is_merge_base = repository.merge_base() == Some(&commit.commit_hash);
                CommitInfo::new(
                    commit,
                    repository.refs(&commit.commit_hash),
                    Color::Reset,
                    is_merge_base,
                    false,
                    None,
                    Vec::new(),
                )
            })
            .collect();
        let mut state = CommitListState::new(
            commit_infos,
            graph_image_manager,
            graph_cell_width,
            graph_visible,
            graph_toggleable,
            false,
            repository.head(),
            repository.merge_base(),
            tip_indexes,
            FxHashMap::default(),
            SearchOptions::default(),
        );
        state.reset_height(subjects.len());
        f(&mut state)
    }

    /// The hash `with_full_commit_list_state` gives the nth subject.
    fn test_hash(n: usize) -> CommitHash {
        CommitHash::from(format!("{:040x}", n + 1).as_str())
    }

    fn input_search_query(state: &mut CommitListState<'_>, query: &str) {
        state.start_search();
        for c in query.chars() {
            state.handle_search_input(KeyEvent::from(KeyCode::Char(c)));
        }
    }

    #[test]
    fn test_restore_search_recalculates_matches_with_applied_options() {
        let (context, options) = with_commit_list_state(&["Fix parser", "other"], |state| {
            input_search_query(state, "fx");
            state.toggle_ignore_case();
            state.toggle_fuzzy();
            state.apply_search();

            (
                state.search_refresh_context().unwrap(),
                state.search_options(),
            )
        });

        assert_eq!(context, SearchRefreshContext { query: "fx".into() });
        assert_eq!(
            options,
            SearchOptions {
                target: SearchTarget::All,
                ignore_case: true,
                fuzzy: true,
            }
        );

        with_commit_list_state(&["unrelated", "FIX new", "fix parser"], |state| {
            state.restore_search_options(options);
            state.restore_search(&context);

            assert_eq!(state.search_refresh_context(), Some(context.clone()));
            assert_eq!(
                state.commits[state.current_selected_index()].commit.subject,
                "unrelated"
            );

            state.select_next_match();
            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 2 (query: \"fx\")".into(), true))
            );
            assert_eq!(
                state.commits[state.current_selected_index()].commit.subject,
                "FIX new"
            );

            state.select_next_match();
            assert_eq!(
                state.matched_query_string(),
                Some(("Match 2 of 2 (query: \"fx\")".into(), true))
            );
            assert_eq!(
                state.commits[state.current_selected_index()].commit.subject,
                "fix parser"
            );
        });
    }

    #[test]
    fn test_restore_search_options_without_applied_search() {
        let options = with_commit_list_state(&["FIX"], |state| {
            state.toggle_ignore_case();
            state.search_options()
        });

        with_commit_list_state(&["FIX"], |state| {
            state.restore_search_options(options);
            input_search_query(state, "fix");
            state.apply_search();

            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_restore_search_keeps_selected_match_position() {
        let context = with_commit_list_state(&["fix"], |state| {
            input_search_query(state, "fix");
            state.apply_search();
            state.search_refresh_context().unwrap()
        });

        with_commit_list_state(&["first", "second", "fix", "last"], |state| {
            state.reset_height(2);
            state.select_index(2);
            state.scroll_up();
            assert_eq!(state.current_list_status(), (1, 1, 2));

            state.restore_search(&context);

            assert_eq!(state.current_list_status(), (1, 1, 2));
            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_search_options_are_reused_for_next_search() {
        with_commit_list_state(&["FIX"], |state| {
            input_search_query(state, "fix");
            state.toggle_ignore_case();
            state.apply_search();
            state.cancel_search();

            input_search_query(state, "fix");
            state.apply_search();

            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_search_options_can_be_changed_before_search() {
        with_commit_list_state(&["FIX"], |state| {
            state.toggle_ignore_case();
            input_search_query(state, "fix");
            state.apply_search();

            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_search_option_string_after_toggles() {
        with_commit_list_state(&["fix"], |state| {
            state.toggle_ignore_case();
            assert_eq!(
                state.search_options().status_string(),
                "[all] [ignore-case] [substring]"
            );
            state.toggle_ignore_case();
            assert_eq!(
                state.search_options().status_string(),
                "[all] [case-sensitive] [substring]"
            );
            state.toggle_fuzzy();
            assert_eq!(
                state.search_options().status_string(),
                "[all] [case-sensitive] [fuzzy]"
            );
            state.toggle_fuzzy();
            assert_eq!(
                state.search_options().status_string(),
                "[all] [case-sensitive] [substring]"
            );
            state.toggle_search_target();
            assert_eq!(
                state.search_options().status_string(),
                "[subject] [case-sensitive] [substring]"
            );
        });
    }

    #[test]
    fn test_search_target_matches_only_the_selected_field() {
        let commit = Commit {
            commit_hash: CommitHash::from("abcdef0123456789abcdef0123456789abcdef01"),
            subject: "subject-match".into(),
            author_name: "author-match".into(),
            ..Commit::default()
        };
        let reference = Ref::Branch {
            name: "ref-match".into(),
            target: commit.commit_hash.clone(),
        };
        let refs = [&reference];
        let cases = [
            (SearchTarget::All, "subject-match", true),
            (SearchTarget::All, "author-match", true),
            (SearchTarget::All, "ref-match", true),
            (SearchTarget::All, "abcdef0", true),
            (SearchTarget::Subject, "subject-match", true),
            (SearchTarget::Subject, "author-match", false),
            (SearchTarget::Author, "author-match", true),
            (SearchTarget::Author, "ref-match", false),
            (SearchTarget::Ref, "ref-match", true),
            (SearchTarget::Ref, "abcdef0", false),
            (SearchTarget::Hash, "abcdef0", true),
            (SearchTarget::Hash, "subject-match", false),
        ];

        for (target, query, expected) in cases {
            let matcher = SearchMatcher::new(query, false, false);
            let mut search_match = SearchMatch::default();
            search_match.set(&commit, &refs, &matcher, target);
            assert_eq!(search_match.matched(), expected, "{target:?}: {query}");
        }
    }

    #[test]
    fn test_search_target_change_clears_previous_field_matches() {
        let commit = Commit {
            commit_hash: CommitHash::from("abcdef0123456789abcdef0123456789abcdef01"),
            subject: "match".into(),
            author_name: "match".into(),
            ..Commit::default()
        };
        let reference = Ref::Branch {
            name: "match".into(),
            target: commit.commit_hash.clone(),
        };
        let refs = [&reference];
        let matcher = SearchMatcher::new("match", false, false);
        let mut search_match = SearchMatch::default();

        search_match.set(&commit, &refs, &matcher, SearchTarget::All);
        assert!(!search_match.refs.is_empty());
        assert!(search_match.subject.is_some());
        assert!(search_match.author_name.is_some());

        search_match.set(&commit, &refs, &matcher, SearchTarget::Hash);
        assert!(search_match.refs.is_empty());
        assert!(search_match.subject.is_none());
        assert!(search_match.author_name.is_none());
    }

    #[test]
    fn test_applied_search_target_toggle_recalculates_matches() {
        with_commit_list_state(&["fix", "other"], |state| {
            input_search_query(state, "fix");
            state.apply_search();

            state.toggle_search_target();
            assert_eq!(state.search_options().target, SearchTarget::Subject);
            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );

            state.toggle_search_target();
            assert_eq!(state.search_options().target, SearchTarget::Author);
            assert_eq!(
                state.matched_query_string(),
                Some(("No matches found (query: \"fix\")".into(), false))
            );
        });
    }

    #[test]
    fn test_applied_search_options_keep_selected_match() {
        with_commit_list_state(&["FIX", "fix"], |state| {
            input_search_query(state, "fix");
            state.apply_search();
            state.toggle_ignore_case();

            assert_eq!(
                state.commits[state.current_selected_index()].commit.subject,
                "fix"
            );
            assert_eq!(
                state.matched_query_string(),
                Some(("Match 2 of 2 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_applied_search_options_select_next_match() {
        with_commit_list_state(&["FIX", "fix"], |state| {
            state.toggle_ignore_case();
            input_search_query(state, "fix");
            state.apply_search();
            state.toggle_ignore_case();

            assert_eq!(
                state.commits[state.current_selected_index()].commit.subject,
                "fix"
            );
            assert_eq!(
                state.matched_query_string(),
                Some(("Match 1 of 1 (query: \"fix\")".into(), true))
            );
        });
    }

    #[test]
    fn test_applied_search_options_keep_selection_when_no_matches() {
        with_commit_list_state(&["fix", "other"], |state| {
            state.toggle_fuzzy();
            input_search_query(state, "fx");
            state.apply_search();
            let selected = state.current_selected_index();

            state.toggle_fuzzy();

            assert_eq!(state.current_selected_index(), selected);
            assert_eq!(
                state.matched_query_string(),
                Some(("No matches found (query: \"fx\")".into(), false))
            );
        });
    }

    #[test]
    fn test_calc_cell_widths_all_columns() {
        let area_width = 80;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(6),  // Graph
            Constraint::Length(1),  // Marker
            Constraint::Min(0),     // Subject
            Constraint::Length(12), // Name (10 + 2 pad)
            Constraint::Length(9),  // Hash (7 + 2 pad)
            Constraint::Length(17), // Date (15 + 2 pad)
        ];
        assert_eq!(actual, expected);
    }

    fn with_tips<R>(
        subjects: &[&str],
        tips: Vec<usize>,
        f: impl FnOnce(&mut CommitListState<'_>) -> R,
    ) -> R {
        with_full_commit_list_state(subjects, 0, true, true, None, tips, f)
    }

    fn marker_of(is_merge_base: bool, tip: Option<usize>, is_head: bool) -> String {
        let commit = Commit::default();
        let info = CommitInfo::new(
            &commit,
            Vec::new(),
            Color::Reset,
            is_merge_base,
            is_head,
            tip,
            Vec::new(),
        );
        marker_symbol(&info, &ColorTheme::default()).0
    }

    #[test]
    fn test_marker_ranks_merge_base_over_tip_over_head() {
        // A row can be all three at once, which happens whenever you sit on one of the revisions.
        assert_eq!(marker_of(true, Some(1), true), "\u{25c6}");
        assert_eq!(marker_of(false, Some(1), true), "1");
        assert_eq!(marker_of(false, None, true), "@");
        assert_eq!(marker_of(false, None, false), "\u{2502}");
    }

    #[test]
    fn test_marker_falls_back_when_there_are_more_tips_than_digits() {
        assert_eq!(marker_of(false, Some(9), false), "9");
        assert_eq!(marker_of(false, Some(10), false), "\u{25b6}");
    }

    #[test]
    fn test_tip_rotation_walks_downward_and_wraps() {
        with_tips(&["a", "b", "c", "d", "e"], vec![1, 3], |state| {
            // Starts on row 0, so the first press lands on the tip below it.
            assert_eq!(state.select_next_tip(), TipJump::Selected);
            assert_eq!(state.selected_commit_hash(), &test_hash(1));

            assert_eq!(state.select_next_tip(), TipJump::Selected);
            assert_eq!(state.selected_commit_hash(), &test_hash(3));

            // Past the last tip it wraps to the first rather than stopping.
            assert_eq!(state.select_next_tip(), TipJump::Selected);
            assert_eq!(state.selected_commit_hash(), &test_hash(1));
        });
    }

    #[test]
    fn test_tip_rotation_is_relative_to_the_current_selection() {
        with_tips(&["a", "b", "c", "d", "e"], vec![1, 3], |state| {
            // Scrolling past both tips means the next press wraps to the top one.
            state.select_commit_hash(&test_hash(4));
            assert_eq!(state.select_next_tip(), TipJump::Selected);
            assert_eq!(state.selected_commit_hash(), &test_hash(1));
        });
    }

    #[test]
    fn test_tip_rotation_visits_tips_in_row_order_not_revspec_order() {
        // Revspec order decides the ordinal shown in the marker column; rotation follows the
        // list, so walking down never jumps backwards.
        with_tips(&["a", "b", "c"], vec![2, 0], |state| {
            state.select_commit_hash(&test_hash(0));
            assert_eq!(state.select_next_tip(), TipJump::Selected);
            assert_eq!(state.selected_commit_hash(), &test_hash(2));
        });
    }

    #[test]
    fn test_tip_rotation_without_tips_reports_it() {
        with_tips(&["a", "b"], Vec::new(), |state| {
            assert_eq!(state.select_next_tip(), TipJump::NoTips);
            assert_eq!(state.selected_commit_hash(), &test_hash(0));
        });
    }

    #[test]
    fn test_tip_rotation_reports_tips_outside_the_rendered_commits() {
        // `--max-count` can cut every tip off; the indexes then point past the list.
        with_tips(&["a", "b"], vec![7, 9], |state| {
            assert_eq!(state.select_next_tip(), TipJump::OutsideRenderedCommits);
            assert_eq!(state.selected_commit_hash(), &test_hash(0));
        });
    }

    #[test]
    fn test_merge_base_jump_selects_the_base_commit() {
        let subjects = &["a", "b", "c", "d"];
        with_full_commit_list_state(
            subjects,
            0,
            true,
            true,
            Some(test_hash(2)),
            Vec::new(),
            |state| {
                assert_eq!(state.selected_commit_hash(), &test_hash(0));
                assert_eq!(state.select_merge_base(), MergeBaseJump::Selected);
                assert_eq!(state.selected_commit_hash(), &test_hash(2));
            },
        );
    }

    #[test]
    fn test_merge_base_jump_is_idempotent() {
        let subjects = &["a", "b", "c"];
        with_full_commit_list_state(
            subjects,
            0,
            true,
            true,
            Some(test_hash(1)),
            Vec::new(),
            |state| {
                assert_eq!(state.select_merge_base(), MergeBaseJump::Selected);
                assert_eq!(state.select_merge_base(), MergeBaseJump::Selected);
                assert_eq!(state.selected_commit_hash(), &test_hash(1));
            },
        );
    }

    #[test]
    fn test_merge_base_jump_without_a_base_is_not_scoped() {
        with_commit_list_state(&["a", "b"], |state| {
            assert_eq!(state.select_merge_base(), MergeBaseJump::NotScoped);
            assert_eq!(state.selected_commit_hash(), &test_hash(0));
        });
    }

    #[test]
    fn test_merge_base_jump_reports_a_base_outside_the_rendered_commits() {
        // A base that `--max-count` cut off: a real hash that no rendered commit carries.
        let outside = CommitHash::from("00000000000000000000000000000000000000ff");
        with_full_commit_list_state(
            &["a", "b"],
            0,
            true,
            true,
            Some(outside),
            Vec::new(),
            |state| {
                assert_eq!(
                    state.select_merge_base(),
                    MergeBaseJump::OutsideRenderedCommits
                );
                assert_eq!(state.selected_commit_hash(), &test_hash(0));
            },
        );
    }

    #[test]
    fn test_zero_height_area_keeps_the_selection_intact() {
        // A terminal shorter than the status line leaves the list no rows at all. `-g hidden`
        // reaches this, since it is the one width that does not refuse to start on a tiny
        // terminal.
        with_commit_list_state(&["a", "b", "c"], |state| {
            state.select_next();
            let before = state.current_list_status();

            state.update_height(0);
            assert_eq!(state.current_list_status(), (before.0, before.1, 0));

            state.update_height(3);
            assert_eq!(state.current_list_status(), before);
        });
    }

    #[test]
    fn test_hidden_graph_reports_no_area_and_toggles_back() {
        with_graph_commit_list_state(&["a", "b"], 6, false, true, |state| {
            assert_eq!(state.graph_area_cell_width(), 0);
            assert!(!state.graph_visible());

            assert_eq!(state.toggle_graph(), GraphToggleResult::Shown);
            assert!(state.graph_visible());
            assert_eq!(state.graph_area_cell_width(), 6); // the whole column, pad included

            assert_eq!(state.toggle_graph(), GraphToggleResult::Hidden);
            assert!(!state.graph_visible());
            assert_eq!(state.graph_area_cell_width(), 0);
        });
    }

    #[test]
    fn test_graph_toggle_is_refused_when_the_terminal_is_too_small() {
        with_graph_commit_list_state(&["a", "b"], 6, false, false, |state| {
            assert_eq!(state.toggle_graph(), GraphToggleResult::TerminalTooSmall);
            assert!(!state.graph_visible());
            assert_eq!(state.graph_area_cell_width(), 0);
        });
    }

    #[test]
    fn test_restore_graph_visible_cannot_reveal_an_untoggleable_graph() {
        with_graph_commit_list_state(&["a", "b"], 6, false, false, |state| {
            state.restore_graph_visible(true);
            assert!(!state.graph_visible());
        });
        with_graph_commit_list_state(&["a", "b"], 6, false, true, |state| {
            state.restore_graph_visible(true);
            assert!(state.graph_visible());
        });
    }

    #[test]
    fn test_hidden_graph_builds_no_images() {
        // Image ids appear only once a row image has actually been built, so an empty set proves
        // the hidden graph costs nothing. (Pending uploads would not: the iTerm2 protocol used
        // here inlines its images instead of uploading them.)
        with_graph_commit_list_state(&["a", "b"], 6, false, true, |state| {
            state.ensure_visible_graph_uploaded();
            assert!(state.graph_image_ids_sorted().is_empty());

            state.toggle_graph();
            state.ensure_visible_graph_uploaded();
            assert_eq!(state.graph_image_ids_sorted().len(), 2);
        });
    }

    #[test]
    fn test_calc_cell_width_hidden_graph_collapses_its_column() {
        let area_width = 80;
        let subject_min_width = 20;
        let graph_width = 0; // what `graph_area_cell_width` reports while the graph is hidden
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(0),  // Graph
            Constraint::Length(1),  // Marker
            Constraint::Min(0),     // Subject
            Constraint::Length(12), // Name (10 + 2 pad)
            Constraint::Length(9),  // Hash (7 + 2 pad)
            Constraint::Length(17), // Date (15 + 2 pad)
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_hidden_graph_keeps_the_other_columns_on_a_small_area() {
        // The same area that has to drop Name when the graph takes 6 columns fits every column
        // once the graph is hidden.
        let area_width = 60;
        let subject_min_width = 20;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let visible = calc_cell_widths(
            area_width,
            subject_min_width,
            6,
            name_width,
            date_width,
            &columns,
        );
        assert_eq!(
            visible,
            vec![
                Constraint::Length(6),  // Graph
                Constraint::Length(1),  // Marker
                Constraint::Min(0),     // Subject
                Constraint::Length(0),  // Name dropped to make room for the graph
                Constraint::Length(9),  // Hash
                Constraint::Length(17), // Date
            ]
        );

        let hidden = calc_cell_widths(
            area_width,
            subject_min_width,
            0,
            name_width,
            date_width,
            &columns,
        );
        assert_eq!(
            hidden,
            vec![
                Constraint::Length(0),  // Graph
                Constraint::Length(1),  // Marker
                Constraint::Min(0),     // Subject
                Constraint::Length(12), // Name kept
                Constraint::Length(9),  // Hash
                Constraint::Length(17), // Date kept
            ]
        );
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name_date_hash() {
        let area_width = 30;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        // Graph + Marker + Subject + Hash = 6 + 1 + 20 + 9 = 36 > 30
        // => Name, Date, and Hash are removed
        let expected = vec![
            Constraint::Length(6), // Graph
            Constraint::Length(1), // Marker
            Constraint::Min(0),    // Subject
            Constraint::Length(0), // Name removed
            Constraint::Length(0), // Hash removed
            Constraint::Length(0), // Date removed
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name_date() {
        let area_width = 40;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        // Graph + Marker + Subject + Hash = 6 + 1 + 20 + 9 = 36
        // Graph + Marker + Subject + Date + Hash = 6 + 1 + 20 + 17 + 9 = 53 > 40
        // => Name and Date are removed
        let expected = vec![
            Constraint::Length(6), // Graph
            Constraint::Length(1), // Marker
            Constraint::Min(0),    // Subject
            Constraint::Length(0), // Name removed
            Constraint::Length(9), // Hash (7 + 2 pad)
            Constraint::Length(0), // Date removed
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name() {
        let area_width = 60;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Name,
            UserListColumnType::Hash,
            UserListColumnType::Date,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        // Graph + Marker + Subject + Date + Hash = 6 + 1 + 20 + 17 + 9 = 53 <= 60
        // Graph + Marker + Subject + Name + Date + Hash = 6 + 1 + 20 + 12 + 17 + 9 = 65 > 60
        // => Name is removed
        let expected = vec![
            Constraint::Length(6),  // Graph
            Constraint::Length(1),  // Marker
            Constraint::Min(0),     // Subject
            Constraint::Length(0),  // Name removed
            Constraint::Length(9),  // Hash (7 + 2 pad)
            Constraint::Length(17), // Date (15 + 2 pad)
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_columns_order() {
        let area_width = 80;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Date,
            UserListColumnType::Subject,
            UserListColumnType::Hash,
            UserListColumnType::Graph,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(17), // Date (15 + 2 pad)
            Constraint::Min(0),     // Subject
            Constraint::Length(9),  // Hash (7 + 2 pad)
            Constraint::Length(6),  // Graph
        ];
        assert_eq!(actual, expected);
    }
}
