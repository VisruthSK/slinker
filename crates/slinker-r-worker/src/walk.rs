use super::InspectionResult;
use slinker_core::package::MemberStep;
use std::num::NonZeroUsize;

/// Structural edges only: never evaluates calls, promises, or active bindings.
pub(super) fn children(value: libr::SEXP) -> InspectionResult<Vec<(MemberStep, libr::SEXP)>> {
    let mut children = Vec::new();
    harp::r::attrib_for_each(value, |tag, attribute| children.push((tag, attribute)));
    let mut children = children
        .into_iter()
        .enumerate()
        .map(|(index, (tag, attribute))| {
            Ok((
                MemberStep::Attribute {
                    position: (index + 1)
                        .try_into()
                        .map_err(|_| "invalid attribute position")?,
                    name: String::from(harp::symbol::RSymbol::new(tag)?).into(),
                },
                attribute,
            ))
        })
        .collect::<InspectionResult<Vec<_>>>()?;
    // ALTREP class policy belongs to the consumer. Looking at its attributes does
    // not require dispatching its element accessors or materializing its storage.
    if harp::utils::r_is_altrep(value) {
        return Ok(children);
    }
    match harp::utils::r_typeof(value) {
        libr::CLOSXP => {
            let body = harp::r::fn_body(value);
            let body = if harp::utils::r_typeof(body) == libr::BCODESXP {
                harp::utils::r_bytecode_expr(body)
            } else {
                body
            };
            children.extend([
                (MemberStep::Formals, harp::r::fn_formals(value)),
                (MemberStep::Body, body),
            ]);
        }
        libr::VECSXP | libr::EXPRSXP => {
            for index in 0..harp::object::r_length(value) {
                let position = usize::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_add(1))
                    .and_then(NonZeroUsize::new)
                    .ok_or("object element position exceeds addressable range")?;
                children.push((
                    MemberStep::Element(position),
                    harp::object::list_get(value, index),
                ));
            }
        }
        libr::LISTSXP | libr::LANGSXP => {
            let elements = std::cell::RefCell::new(Vec::new());
            harp::utils::r_pairlist_any(value, |element| {
                elements.borrow_mut().push(element);
                false
            });
            for (index, element) in elements.into_inner().into_iter().enumerate() {
                let position = index
                    .checked_add(1)
                    .and_then(NonZeroUsize::new)
                    .ok_or("object element position exceeds addressable range")?;
                children.push((MemberStep::Element(position), element));
            }
        }
        _ => {}
    }
    Ok(children)
}
