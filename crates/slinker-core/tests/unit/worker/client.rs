use super::*;

#[test]
fn stale_protocol_files_do_not_block_worker_startup() {
    let stale = (0..64)
        .map(|index| {
            std::env::temp_dir().join(format!(
                "slinker-r-worker-{}-{index}.jsonl",
                std::process::id()
            ))
        })
        .filter(|path| std::fs::File::create_new(path).is_ok())
        .collect::<Vec<_>>();
    let created = (0..3).map(|_| protocol_file()).collect::<Vec<_>>();
    for path in stale {
        let _ = std::fs::remove_file(path);
    }
    for result in created {
        result.expect("a fresh protocol file");
    }
}

#[test]
fn worker_crash_context_identifies_exact_binding_request() {
    let request = WorkerRequest::Binding {
        request_id: 41,
        package: PackageSpec {
            name: "fixture".into(),
            version: "1.0.0".into(),
            image_fingerprint: "abc123".into(),
            root: "fixture".into(),
        },
        name: "bad".into(),
    };

    assert_eq!(
        request_context(&request),
        "request 41 binding fixture::bad 1.0.0 abc123"
    );
}
