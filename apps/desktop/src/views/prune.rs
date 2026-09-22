//! Letting go of what the workspace keeps beside the model once its subject
//! is gone: the folds of log rows that left their server's log, every fold
//! and reveal kept for a deleted server, and those of calls a cleared history
//! let go. The model says what left with [`Gone`].

use std::collections::HashSet;

use crate::state::Gone;
use crate::views::Workspace;
use crate::views::log_list::fold_row;

impl Workspace {
    /// Drop what the workspace kept for rows or a server that are gone.
    pub(crate) fn forget(&mut self, gone: &Gone) {
        for keys in [&mut self.collapsed, &mut self.revealed] {
            retain_live(keys, gone);
        }
        self.splits.forget(gone);
    }
}

/// Keep only the keys that do not belong to what `gone` names.
///
/// A key belongs to a server when the server's id sits between colons in it,
/// the way every server-scoped prefix spells it (`log:`, `schema:`, `resp:`
/// and the rest), and to one of its recorded calls when `hist:` or `args:` is
/// followed by the call's id. Content trees append letters straight after the
/// id (`s`, `c0`, `m1`), so the id is matched as a prefix; a call id is a
/// fixed-length uuid, so no other call's key starts with it.
fn retain_live(keys: &mut HashSet<String>, gone: &Gone) {
    match gone {
        Gone::LogRows { server_id, before } => keys.retain(|key| {
            !fold_row(key).is_some_and(|(server, id)| server == server_id && id < *before)
        }),
        Gone::Server { id, calls } => {
            let scoped = format!(":{id}:");
            keys.retain(|key| !key.contains(&scoped) && !of_calls(key, calls));
        }
        Gone::Calls { calls } => keys.retain(|key| !of_calls(key, calls)),
    }
}

/// Whether `key` belongs to one of `calls`: `hist:` or `args:` followed by
/// the call's id.
fn of_calls(key: &str, calls: &[String]) -> bool {
    key.strip_prefix("hist:")
        .or_else(|| key.strip_prefix("args:"))
        .is_some_and(|rest| calls.iter().any(|c| rest.starts_with(c.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::log_list::fold_prefix;

    const A: &str = "0198f3a2-7c1e-7000-8000-00000000000a";
    const B: &str = "0198f3a2-7c1e-7000-8000-00000000000b";

    fn keys(list: &[String]) -> HashSet<String> {
        list.iter().cloned().collect()
    }

    #[test]
    fn the_folds_of_rows_that_left_are_dropped() {
        let kept = [
            format!("{}$", fold_prefix(A, 7)),
            format!("{}$.params", fold_prefix(A, 12)),
            format!("{}$", fold_prefix(B, 3)),
            format!("schema:{A}:6$"),
            "req:msg0$".to_owned(),
        ];
        let dropped = [
            format!("{}$.x", fold_prefix(A, 6)),
            format!("{}$", fold_prefix(A, 0)),
        ];
        let mut folds = keys(&[kept.as_slice(), dropped.as_slice()].concat());
        let gone = Gone::LogRows {
            server_id: A.into(),
            before: 7,
        };
        retain_live(&mut folds, &gone);
        assert_eq!(folds, keys(&kept));
    }

    #[test]
    fn a_deleted_server_takes_every_key_it_scoped() {
        let kept = [
            format!("{}$", fold_prefix(B, 1)),
            format!("schema:{B}:t$"),
            format!("resp:{B}:Tools:x"),
            "hist:call-b$".to_owned(),
            "req:msg0$".to_owned(),
        ];
        let dropped = [
            format!("{}$", fold_prefix(A, 1)),
            format!("schema:{A}:t$"),
            format!("outschema:{A}:t$.properties"),
            format!("annot:{A}:t$"),
            format!("resp:{A}:Tools:x$"),
            format!("resp:{A}:Tools:x"),
            "hist:call-a$.content".to_owned(),
            // Content trees extend the prefix with letters: structured
            // content, a text block parsed as JSON.
            "hist:call-as$".to_owned(),
            "hist:call-ac0$".to_owned(),
            "hist:call-a".to_owned(),
            "args:call-a$".to_owned(),
        ];
        let mut folds = keys(&[kept.as_slice(), dropped.as_slice()].concat());
        let gone = Gone::Server {
            id: A.into(),
            calls: vec!["call-a".into()],
        };
        retain_live(&mut folds, &gone);
        assert_eq!(folds, keys(&kept));
    }

    #[test]
    fn cleared_calls_take_only_their_own_keys() {
        let kept = [
            format!("resp:{A}:History:call-b"),
            "hist:call-b$".to_owned(),
            format!("schema:{A}:t$"),
        ];
        let dropped = ["hist:call-a$".to_owned(), "args:call-a$.x".to_owned()];
        let mut folds = keys(&[kept.as_slice(), dropped.as_slice()].concat());
        let gone = Gone::Calls {
            calls: vec!["call-a".into()],
        };
        retain_live(&mut folds, &gone);
        assert_eq!(folds, keys(&kept));
    }
}
