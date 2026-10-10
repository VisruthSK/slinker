use harp::object::RObject;

pub(crate) fn names(value: libr::SEXP) -> Vec<String> {
    RObject::view(value)
        .names()
        .map(|names| names.into_iter().map(Option::unwrap_or_default).collect())
        .unwrap_or_default()
}
