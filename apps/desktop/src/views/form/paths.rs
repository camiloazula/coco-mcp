//! Addressing a field by its JSON path, in the form model and in the form state.

use super::*;

/// Field trail (indices into nested `Object` fields) for a `$.a.b` path.
pub(super) fn trail_of(path: &str, model: &FormModel) -> Vec<usize> {
    let mut trail = Vec::new();
    let mut current = model;
    for seg in path
        .trim_start_matches('$')
        .split('.')
        .filter(|s| !s.is_empty())
    {
        let seg = seg.split(['[', '|']).next().unwrap_or(seg);
        let FormModel::Object { fields, .. } = current else {
            break;
        };
        let Some(ix) = fields.iter().position(|f| f.name == seg) else {
            break;
        };
        trail.push(ix);
        current = &fields[ix].model;
    }
    trail
}

pub(super) fn field_model<'a>(model: &'a FormModel, trail: &[usize]) -> Option<&'a FormModel> {
    let mut current = model;
    for &ix in trail {
        let FormModel::Object { fields, .. } = current else {
            return None;
        };
        current = &fields.get(ix)?.model;
    }
    Some(current)
}

pub(super) fn set_field(state: &mut FormState, trail: &[usize], value: Option<FormState>) {
    let Some((&last, parents)) = trail.split_last() else {
        return;
    };
    let mut current = state;
    for &ix in parents {
        let FormState::Object(values) = current else {
            return;
        };
        let Some(Some(next)) = values.get_mut(ix) else {
            return;
        };
        current = next;
    }
    if let FormState::Object(values) = current
        && let Some(slot) = values.get_mut(last)
    {
        *slot = value;
    }
}

/// Segment of a path: `.name`, `[index]`, `|variant`.
pub(super) enum Seg<'a> {
    Field(&'a str),
    Index(usize),
    Variant(usize),
}

pub(super) fn segments(path: &str) -> Vec<Seg<'_>> {
    let mut out = Vec::new();
    let rest = path.trim_start_matches('$');
    let mut i = 0;
    let bytes = rest.as_bytes();
    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && !matches!(bytes[end], b'.' | b'[' | b'|') {
                    end += 1;
                }
                out.push(Seg::Field(&rest[start..end]));
                i = end;
            }
            b'[' => {
                let end = rest[i..].find(']').map(|e| i + e).unwrap_or(bytes.len());
                out.push(Seg::Index(rest[i + 1..end].parse().unwrap_or(0)));
                i = end + 1;
            }
            b'|' => {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                out.push(Seg::Variant(rest[start..end].parse().unwrap_or(0)));
                i = end;
            }
            _ => i += 1,
        }
    }
    out
}

pub(super) fn model_at<'a>(model: &'a FormModel, path: &str) -> Option<&'a FormModel> {
    let mut current = model;
    for seg in segments(path) {
        current = match (seg, current) {
            (Seg::Field(name), FormModel::Object { fields, .. }) => {
                &fields.iter().find(|f| f.name == name)?.model
            }
            (Seg::Index(_), FormModel::Array { items, .. }) => items,
            (Seg::Variant(i), FormModel::OneOf { variants, .. }) => &variants.get(i)?.model,
            _ => return None,
        };
    }
    Some(current)
}

pub(super) fn state_at_mut<'a>(
    state: &'a mut FormState,
    model: &FormModel,
    path: &str,
) -> Option<&'a mut FormState> {
    let mut current = state;
    let mut current_model = model;
    for seg in segments(path) {
        match (seg, current_model) {
            (Seg::Field(name), FormModel::Object { fields, .. }) => {
                let ix = fields.iter().position(|f| f.name == name)?;
                current_model = &fields[ix].model;
                let FormState::Object(values) = current else {
                    return None;
                };
                current = values.get_mut(ix)?.as_mut()?;
            }
            (Seg::Index(i), FormModel::Array { items, .. }) => {
                current_model = items;
                let FormState::Array(values) = current else {
                    return None;
                };
                current = values.get_mut(i)?;
            }
            (Seg::Variant(i), FormModel::OneOf { variants, .. }) => {
                current_model = &variants.get(i)?.model;
                let FormState::OneOf { states, .. } = current else {
                    return None;
                };
                current = states.get_mut(i)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

pub(super) fn set_at(state: &mut FormState, model: &FormModel, path: &str, value: FormState) {
    if let Some(slot) = state_at_mut(state, model, path) {
        *slot = value;
    }
}
