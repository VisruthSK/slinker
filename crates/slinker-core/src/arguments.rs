use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub(crate) enum MatchScope {
    Signature,
    Projection,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ArgumentMatchError {
    #[error("invalid or duplicate formal argument `{0}`")]
    InvalidFormal(String),
    #[error("formal argument `{0}` matched by multiple actual arguments")]
    Duplicate(String),
    #[error("argument `{0}` matches multiple formal arguments")]
    Ambiguous(String),
    #[error("unused argument at position {0}")]
    Unused(usize),
}

pub(crate) struct MatchedArguments(Vec<Option<usize>>);

impl MatchedArguments {
    pub(crate) fn argument(&self, formal: usize) -> Option<usize> {
        self.0[formal]
    }
}

pub(crate) fn match_arguments<'a, S: AsRef<str>>(
    names: impl Iterator<Item = Option<&'a str>>,
    formals: &[S],
    scope: MatchScope,
) -> Result<MatchedArguments, ArgumentMatchError> {
    let mut unique = BTreeSet::new();
    for formal in formals {
        if formal.as_ref().is_empty() || !unique.insert(formal.as_ref()) {
            return Err(ArgumentMatchError::InvalidFormal(formal.as_ref().into()));
        }
    }
    let names = names.collect::<Vec<_>>();
    let mut assigned = vec![None; formals.len()];
    let mut exact = vec![false; formals.len()];
    let mut consumed = vec![false; names.len()];
    let dots = formals.iter().position(|formal| formal.as_ref() == "...");
    let before_dots = dots.unwrap_or(formals.len());

    for (actual, name) in names.iter().enumerate() {
        let Some(name) = name else { continue };
        if let Some(formal) = formals
            .iter()
            .position(|formal| formal.as_ref() == *name && *name != "...")
        {
            if assigned[formal].is_some() {
                return Err(ArgumentMatchError::Duplicate((*name).into()));
            }
            assigned[formal] = Some(actual);
            exact[formal] = true;
            consumed[actual] = true;
        }
    }
    for (actual, name) in names.iter().enumerate() {
        let Some(name) = name.filter(|_| !consumed[actual]) else {
            continue;
        };
        let mut candidates = formals
            .iter()
            .enumerate()
            .take(before_dots)
            .filter(|(formal, candidate)| !exact[*formal] && candidate.as_ref().starts_with(name))
            .map(|(formal, _)| formal);
        if let Some(formal) = candidates.next() {
            if candidates.next().is_some() {
                return Err(ArgumentMatchError::Ambiguous(name.into()));
            }
            if assigned[formal].is_some() {
                return Err(ArgumentMatchError::Duplicate(
                    formals[formal].as_ref().into(),
                ));
            }
            assigned[formal] = Some(actual);
            consumed[actual] = true;
        }
    }
    let mut next = 0;
    for (actual, name) in names.iter().enumerate() {
        if name.is_some() || consumed[actual] {
            continue;
        }
        while next < before_dots && assigned[next].is_some() {
            next += 1;
        }
        if next == before_dots {
            break;
        }
        assigned[next] = Some(actual);
        consumed[actual] = true;
        next += 1;
    }
    if dots.is_none()
        && matches!(scope, MatchScope::Signature)
        && let Some(unused) = consumed.iter().position(|consumed| !consumed)
    {
        return Err(ArgumentMatchError::Unused(unused + 1));
    }
    Ok(MatchedArguments(assigned))
}
