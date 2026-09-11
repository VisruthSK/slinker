use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(crate) struct RRuntime {
    pub package_root: PathBuf,
    pub runner: PathBuf,
}

pub(crate) fn prepare(work_dir: &Path) -> io::Result<RRuntime> {
    let root = work_dir.join("r-runtime");
    let package_root = root.join("slinker");
    let r_dir = package_root.join("R");
    fs::create_dir_all(&r_dir)?;

    write(
        &package_root.join("DESCRIPTION"),
        include_str!("../r/slinker/DESCRIPTION"),
    )?;
    write(
        &package_root.join("NAMESPACE"),
        include_str!("../r/slinker/NAMESPACE"),
    )?;
    write(
        &package_root.join("LICENSE"),
        include_str!("../r/slinker/LICENSE"),
    )?;
    write(
        &r_dir.join("00-utils.R"),
        include_str!("../r/slinker/R/00-utils.R"),
    )?;
    write(
        &r_dir.join("image.R"),
        include_str!("../r/slinker/R/image.R"),
    )?;
    write(
        &r_dir.join("parallel.R"),
        include_str!("../r/slinker/R/parallel.R"),
    )?;
    write(
        &r_dir.join("target.R"),
        include_str!("../r/slinker/R/target.R"),
    )?;
    write(
        &r_dir.join("validate.R"),
        include_str!("../r/slinker/R/validate.R"),
    )?;

    let runner = root.join("runner.R");
    write(&runner, include_str!("r/runner.R"))?;

    Ok(RRuntime {
        package_root,
        runner,
    })
}

fn write(path: &Path, contents: &str) -> io::Result<()> {
    if fs::read(path).ok().as_deref() == Some(contents.as_bytes()) {
        return Ok(());
    }
    fs::write(path, contents)
}
