use slinker_core::package::ResourcePath;

#[test]
fn resource_paths_are_checked_when_parsed_or_deserialized() {
    for path in ["", ".", "data/x.json", "data/./x.json"] {
        assert_eq!(path.parse::<ResourcePath>().unwrap().as_str(), path);
        let encoded = serde_json::to_string(path).unwrap();
        assert_eq!(
            serde_json::from_str::<ResourcePath>(&encoded)
                .unwrap()
                .as_str(),
            path
        );
    }
    for path in ["..", "data/../../escape", "/absolute", "nul\0byte"] {
        assert!(path.parse::<ResourcePath>().is_err(), "{path:?}");
        let encoded = serde_json::to_string(path).unwrap();
        assert!(
            serde_json::from_str::<ResourcePath>(&encoded).is_err(),
            "{path:?}"
        );
    }
}
