use slinker_core::Error;
use slinker_core::analysis::ANALYSIS_STACK_BYTES;
use std::path::Path;

pub fn main(run: fn()) -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "__r-worker")
    {
        let protocol = arguments
            .get(2)
            .ok_or_else(|| Error::Analysis("missing worker protocol path".into()))?;
        return Ok(slinker_r_worker::run(Path::new(protocol))?);
    }
    if !arguments.iter().any(|argument| argument == "--bench") {
        println!("benchmarks run only under `cargo bench`");
        return Ok(());
    }
    std::thread::Builder::new()
        .stack_size(ANALYSIS_STACK_BYTES)
        .spawn(run)?
        .join()
        .map_err(|_| Error::Analysis("benchmark failed".into()))?;
    Ok(())
}

pub fn target_request(r_home: &Path) -> slinker_core::TargetEnvironmentRequest {
    let mut request = slinker_core::TargetEnvironmentRequest::new(r_home.to_path_buf());
    if let Some(libraries) = std::env::var_os("R_LIBS_USER") {
        request.libraries = std::env::split_paths(&libraries).collect();
    }
    request
}
