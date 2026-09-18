//! Scroll state of the log drawer's row list, kept on the workspace so it
//! survives re-renders. gpui's `uniform_list` sizes every row from the
//! first one, which made an expanded payload paint over the rows below it;
//! `list` measures each row, so the expanded one pushes the rest down.
//!
//! The row cap drops rows from the front of a log and shifts every index, so
//! rows are named here, and in the drawer's fold keys, by id
//! (`LogRow::row_id`). Which rows pass the filter is worked out once per
//! frame and kept here as log indices, so drawing a row borrows it from the
//! model rather than copying the log to find it.

use gpui_kit::{FollowMode, ListAlignment, ListOffset, ListState, px};

use crate::state::{AppState, LogFilter};

/// Tree prefix of the payload of row `id` of server `server`'s log. Keyed by
/// the row's id, so a fold stays on its row when older rows leave.
pub fn fold_prefix(server: &str, id: u64) -> String {
    format!("log:{server}:{id}")
}

/// The server and row id a key of a log row's payload tree was built from,
/// the inverse of [`fold_prefix`]; `None` for a key of any other tree.
pub(crate) fn fold_row(key: &str) -> Option<(&str, u64)> {
    // A tree's keys continue its prefix with `$`, which neither part holds.
    let (prefix, _) = key.split_once('$')?;
    let (server, id) = prefix.strip_prefix("log:")?.rsplit_once(':')?;
    Some((server, id.parse().ok()?))
}

/// The list state and what it was last sized for, so appends and the rows
/// the cap drops splice instead of resetting and losing the scroll position.
#[derive(Debug)]
pub struct LogList {
    state: ListState,
    server: Option<String>,
    /// Log index of each row the state holds, in list order.
    rows: Vec<usize>,
    /// Id one past the newest row the state holds.
    end: u64,
    filter: LogFilter,
    min_level: Option<&'static str>,
    /// Id of the expanded row.
    expanded: Option<u64>,
    collapse_rev: u64,
}

impl Default for LogList {
    fn default() -> Self {
        // Scrolled to the bottom, the drawer follows new messages; scrolled
        // up, it stays where it was.
        let state = ListState::new(0, ListAlignment::Top, px(240.));
        state.set_follow_mode(FollowMode::Tail);
        Self {
            state,
            server: None,
            rows: Vec::new(),
            end: 0,
            filter: LogFilter::All,
            min_level: None,
            expanded: None,
            collapse_rev: 0,
        }
    }
}

/// The selected server's log as the drawer draws it.
#[derive(Debug)]
pub(crate) struct LogView {
    server: Option<String>,
    /// Id of the first log row, so the row at index `ix` has id `first + ix`.
    first: u64,
    /// Id the next row logged will get.
    next: u64,
    /// Log indices of the rows that pass the filter, ascending.
    visible: Vec<usize>,
    filter: LogFilter,
    /// Lowest server log level shown, when the level filter is on.
    min_level: Option<&'static str>,
    /// Id of the expanded row.
    expanded: Option<u64>,
}

impl LogView {
    /// What `state` shows of the selected server's log.
    pub(crate) fn of(state: &AppState) -> Self {
        let server = state.server();
        Self {
            server: server.map(|s| s.record.id.clone()),
            first: server.map_or(0, |s| s.first_log_id()),
            next: server.map_or(0, |s| s.next_log_id()),
            visible: state.visible_log(),
            filter: state.log_filter,
            min_level: state.log_min_level,
            expanded: state.expanded_log,
        }
    }
}

impl LogList {
    /// The list state to render with.
    pub(crate) fn state(&self) -> ListState {
        self.state.clone()
    }

    /// Whether no row passed the filter at the last sync.
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Log index of the row at `position` of the list, as of the last sync.
    pub(crate) fn row_at(&self, position: usize) -> Option<usize> {
        self.rows.get(position).copied()
    }

    /// Bring the list state in step with the rows about to be rendered.
    pub(crate) fn sync(&mut self, log: LogView, collapse_rev: u64) {
        let count = log.visible.len();
        let id = |ix: usize| log.first + ix as u64;
        // Rows with ids before the old end were already in the state; the
        // ones missing from its front left the log (the cap, Clear).
        let kept = log.visible.partition_point(|&ix| id(ix) < self.end);
        if log.server != self.server
            || log.filter != self.filter
            || log.min_level != self.min_level
            || kept == 0
        {
            // Another server or filter, or nothing left in common: start over.
            self.state.reset(count);
        } else {
            let dropped = self.rows.len().saturating_sub(kept);
            if dropped > 0 {
                self.state.splice(0..dropped, 0);
            }
            if count > kept {
                self.state.splice(kept..kept, count - kept);
            }
        }
        // A toggled row changes height, and so does folding a node inside the
        // open one: re-measure both the old and the new row.
        let position = |target: u64| {
            target
                .checked_sub(log.first)
                .and_then(|ix| usize::try_from(ix).ok())
                .and_then(|ix| log.visible.binary_search(&ix).ok())
        };
        if log.expanded != self.expanded || collapse_rev != self.collapse_rev {
            for pos in [self.expanded, log.expanded]
                .into_iter()
                .flatten()
                .filter_map(position)
            {
                self.state.splice(pos..pos + 1, 1);
            }
        }
        // The row a click toggled starts at the top of the drawer, opened or
        // closed: a payload of many lines reads from its first, and closing
        // one leaves the eye on the row it was reading, not on whatever the
        // rows below happen to have moved up to. Only within the same log:
        // another server's list has nothing to do with the id.
        if log.server == self.server
            && log.expanded != self.expanded
            && let Some(pos) = log.expanded.or(self.expanded).and_then(position)
        {
            self.state.scroll_to(ListOffset {
                item_ix: pos,
                offset_in_item: px(0.),
            });
        }
        self.server = log.server;
        self.rows = log.visible;
        self.end = log.next;
        self.filter = log.filter;
        self.min_level = log.min_level;
        self.expanded = log.expanded;
        self.collapse_rev = collapse_rev;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::ListOffset;

    /// A log of `len` rows with ids from `first`, of which `visible` pass.
    fn log(server: &str, first: u64, len: usize, visible: &[usize]) -> LogView {
        LogView {
            server: Some(server.to_owned()),
            first,
            next: first + len as u64,
            visible: visible.to_vec(),
            filter: LogFilter::All,
            min_level: None,
            expanded: None,
        }
    }

    fn scroll(list: &LogList, item_ix: usize) {
        list.state.scroll_to(ListOffset {
            item_ix,
            offset_in_item: px(0.),
        });
    }

    fn top(list: &LogList) -> usize {
        list.state.logical_scroll_top().item_ix
    }

    #[test]
    fn a_toggled_row_starts_at_the_top_open_or_closed() {
        let mut list = LogList::default();
        list.sync(log("a", 0, 40, &(0..40).collect::<Vec<_>>()), 0);
        scroll(&list, 30);
        // Opening row 12 brings it to the top.
        let mut open = log("a", 0, 40, &(0..40).collect::<Vec<_>>());
        open.expanded = Some(12);
        list.sync(open, 0);
        assert_eq!(top(&list), 12, "the opened row is the first drawn");
        // Scrolled away and closed again, it is the first drawn again.
        scroll(&list, 25);
        list.sync(log("a", 0, 40, &(0..40).collect::<Vec<_>>()), 0);
        assert_eq!(top(&list), 12, "the closed row is the first drawn");
        // Folding a node inside an open row is not a toggle.
        let mut open = log("a", 0, 40, &(0..40).collect::<Vec<_>>());
        open.expanded = Some(12);
        list.sync(open, 0);
        scroll(&list, 20);
        let mut same = log("a", 0, 40, &(0..40).collect::<Vec<_>>());
        same.expanded = Some(12);
        list.sync(same, 1);
        assert_eq!(
            top(&list),
            20,
            "a fold inside the row leaves the scroll alone"
        );
        // Another server's list is not scrolled by the id it carries over.
        scroll(&list, 5);
        let mut other = log("b", 0, 40, &(0..40).collect::<Vec<_>>());
        other.expanded = Some(30);
        list.sync(other, 1);
        assert_ne!(top(&list), 30);
    }

    #[test]
    fn rows_dropped_at_the_cap_leave_the_front_of_the_list() {
        let mut list = LogList::default();
        list.sync(log("a", 0, 5, &[0, 1, 2, 3, 4]), 0);
        scroll(&list, 3);
        // Full: one row in, the oldest out, the count unchanged.
        list.sync(log("a", 1, 5, &[0, 1, 2, 3, 4]), 0);
        assert_eq!(list.state.item_count(), 5);
        assert_eq!(top(&list), 2, "the same row stays at the top");
    }

    #[test]
    fn a_filtered_list_drops_only_the_rows_it_held() {
        let mut list = LogList::default();
        // Rows 0, 2 and 4 pass, scrolled to row 4.
        list.sync(log("a", 0, 5, &[0, 2, 4]), 0);
        scroll(&list, 2);
        // Rows 0 and 1 dropped, 5 and 6 appended, and 6 passes.
        list.sync(log("a", 2, 5, &[0, 2, 4]), 0);
        assert_eq!(list.state.item_count(), 3);
        assert_eq!(top(&list), 1, "row 4 is now second");
    }

    #[test]
    fn clearing_or_switching_servers_starts_over() {
        let mut list = LogList::default();
        list.sync(log("a", 0, 5, &[0, 1, 2, 3, 4]), 0);
        scroll(&list, 3);
        // Cleared, then one new row.
        list.sync(log("a", 5, 1, &[0]), 0);
        assert_eq!((list.state.item_count(), top(&list)), (1, 0));
        list.sync(log("a", 5, 4, &[0, 1, 2, 3]), 0);
        assert_eq!(list.state.item_count(), 4, "appends splice");
        scroll(&list, 2);
        // Another server, whose row ids overlap this one's.
        list.sync(log("b", 0, 9, &[0, 1, 2, 3, 4, 5, 6, 7, 8]), 0);
        assert_eq!((list.state.item_count(), top(&list)), (9, 0));
    }

    #[test]
    fn a_position_names_the_log_row_it_draws() {
        let mut list = LogList::default();
        assert_eq!(list.row_at(0), None, "nothing synced yet");
        // Rows 0, 2 and 4 pass the filter.
        list.sync(log("a", 0, 5, &[0, 2, 4]), 0);
        let rows = |list: &LogList| [0, 1, 2, 3].map(|p| list.row_at(p));
        assert_eq!(rows(&list), [Some(0), Some(2), Some(4), None]);
        // Rows 0 and 1 dropped and 5 and 6 appended: rows 2, 4 and 6 pass,
        // now at indices 0, 2 and 4.
        list.sync(log("a", 2, 5, &[0, 2, 4]), 0);
        assert_eq!(rows(&list), [Some(0), Some(2), Some(4), None]);
        assert_eq!(list.state.item_count(), 3);
        // Another filter shows other rows at the same positions.
        let mut stderr = log("a", 2, 5, &[1, 3]);
        stderr.filter = LogFilter::Stderr;
        list.sync(stderr, 0);
        assert_eq!(rows(&list), [Some(1), Some(3), None, None]);
    }

    #[test]
    fn a_fold_key_names_the_row_it_was_built_for() {
        let server = "0198f3a2-7c1e-7000-8000-0000000000aa";
        let prefix = fold_prefix(server, 42);
        assert_eq!(fold_row(&format!("{prefix}$")), Some((server, 42)));
        assert_eq!(
            fold_row(&format!("{prefix}$.params[0].a:b$c")),
            Some((server, 42)),
            "any node under the payload"
        );
        assert_eq!(fold_row(&format!("schema:{server}:42$")), None);
        assert_eq!(fold_row(&format!("log:{server}:tool$")), None);
        assert_eq!(fold_row(&prefix), None, "a prefix is not a node");
    }
}
