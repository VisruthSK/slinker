use super::*;

#[test]
fn rendered_names_parse_back_to_their_kind_package_and_key() {
    let index = EntryName::parse(&EntryKind::index_name("rlang", "abc")).expect("index");
    assert_eq!(
        (index.kind, index.package.as_deref(), index.key.as_str()),
        (EntryKind::Index, Some("rlang"), "abc")
    );
    let binding = EntryName::parse(&EntryKind::Binding.member_name("rlang.x", "abc", "def"))
        .expect("binding");
    assert_eq!(
        (
            binding.kind,
            binding.package.as_deref(),
            binding.key.as_str()
        ),
        (EntryKind::Binding, Some("rlang.x"), "abc")
    );
    let dispatch = EntryName::parse(&EntryKind::dispatch_name("abc")).expect("dispatch");
    assert_eq!(
        (dispatch.kind, dispatch.package),
        (EntryKind::Dispatch, None)
    );
    assert!(EntryName::parse("unrelated").is_none());
}
