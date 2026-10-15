use slinker_core::cache::{Cache, CacheLocation};
use std::time::Instant;

fn main() {
    if std::env::args().any(|arg| arg == "--test") {
        return;
    }
    let values = (0..1_000)
        .map(|index| {
            (
                format!("entry-{index:04}"),
                vec![u8::try_from(index % 251).unwrap(); 2_048],
            )
        })
        .collect::<Vec<_>>();
    let mut samples = Vec::new();
    for _ in 0..7 {
        let directory = tempfile::tempdir().unwrap();
        let location = CacheLocation::Directory(directory.path().into());
        let started = Instant::now();
        // Multiple invocations exercise the storage format's publication and maintenance.
        for batch in values.chunks(100) {
            let cache = Cache::new(location.clone(), "benchmark").unwrap();
            for (key, value) in batch {
                cache.publish(key, value);
            }
        }
        let write = started.elapsed();
        let started = Instant::now();
        let cache = Cache::new(location.clone(), "benchmark").unwrap();
        let open = started.elapsed();
        let started = Instant::now();
        for (key, value) in &values {
            assert_eq!(cache.read::<Vec<u8>>(key).as_ref(), Some(value));
        }
        let read = started.elapsed();
        let started = Instant::now();
        assert_eq!(cache.remove_where(|key| key.ends_with('0')).unwrap(), 100);
        drop(cache);
        let remove = started.elapsed();
        let cache = Cache::new(location, "benchmark").unwrap();
        for (key, value) in &values {
            let expected = (!key.ends_with('0')).then_some(value);
            assert_eq!(cache.read::<Vec<u8>>(key).as_ref(), expected);
        }
        assert_eq!(cache.entries().len(), 900);
        samples.push([
            write.as_secs_f64(),
            open.as_secs_f64(),
            read.as_secs_f64(),
            remove.as_secs_f64(),
        ]);
    }
    for (column, name) in ["publish", "open", "read", "remove"].iter().enumerate() {
        let mut times = samples
            .iter()
            .map(|sample| sample[column])
            .collect::<Vec<_>>();
        times.sort_by(f64::total_cmp);
        println!(
            "{name}: {:.3} ms median (7 samples, 1000 entries, 2048 bytes each)",
            times[3] * 1_000.0
        );
    }
}
