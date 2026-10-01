use harp::object::RObject;

pub(crate) fn symbol_name(symbol: libr::SEXP) -> Option<String> {
    if harp::utils::r_is_null(symbol) {
        return None;
    }
    Option::<String>::try_from(&RObject::view(symbol))
        .ok()
        .flatten()
}

pub(crate) fn names(value: libr::SEXP) -> Vec<String> {
    RObject::view(value)
        .names()
        .map(|names| names.into_iter().map(Option::unwrap_or_default).collect())
        .unwrap_or_default()
}

pub(crate) fn classes(value: libr::SEXP) -> Vec<String> {
    RObject::view(value)
        .get_attribute("class")
        .and_then(|classes| Vec::<String>::try_from(classes).ok())
        .unwrap_or_default()
}
