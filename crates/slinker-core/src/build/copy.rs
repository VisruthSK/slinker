use super::FrozenInputs;
use crate::filesystem::copy_entry;
use crate::ir::ProgramIr;
use std::fs;
use std::path::Path;

pub(super) fn copy_root_resources(source: &Path, output: &Path) -> Result<(), std::io::Error> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(
            name.to_str(),
            Some(
                "DESCRIPTION"
                    | "NAMESPACE"
                    | "MD5"
                    | "R"
                    | "inst"
                    | "src"
                    | "configure"
                    | "configure.win"
                    | "cleanup"
                    | "cleanup.win"
                    | ".Rbuildignore"
                    | ".Rinstignore"
            )
        ) {
            continue;
        }
        copy_entry(&entry.path(), &output.join(name))?;
    }
    Ok(())
}

pub(super) fn freeze_inst_resources(
    source: &Path,
    installed: &Path,
    frozen: &Path,
    native: Option<&Path>,
) -> Result<(), std::io::Error> {
    fs::create_dir_all(frozen)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let selected = installed.join(entry.file_name());
        if !selected.exists() {
            continue;
        }
        if native
            .is_some_and(|native| dunce::canonicalize(&selected).ok().as_deref() == Some(native))
        {
            continue;
        }
        let kind = entry.file_type()?;
        let target = frozen.join(entry.file_name());
        if kind.is_dir() && selected.is_dir() {
            freeze_inst_resources(&entry.path(), &selected, &target, None)?;
        } else if kind.is_file() && selected.is_file() {
            copy_entry(&selected, &target)?;
        } else {
            return Err(std::io::Error::other(format!(
                "unsupported staged resource: {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

pub(super) fn copy_linked_resources(
    program: &ProgramIr,
    frozen: &FrozenInputs,
    output: &Path,
) -> Result<(), std::io::Error> {
    for (id, resource) in program.indexed_resources() {
        let package = program.package(resource.package).identity();
        let source = &frozen.resources[&id];
        let target = output
            .join("inst/slinker/resources")
            .join(package.name.as_str())
            .join(resource.path.as_str());
        copy_entry(source, &target)?;
    }
    Ok(())
}

pub(super) fn copy_dataset_libraries(
    program: &ProgramIr,
    frozen: &FrozenInputs,
    output: &Path,
) -> Result<(), std::io::Error> {
    for (package, _) in program.dataset_libraries() {
        let identity = program.package(package).identity();
        let files = &frozen.datasets[&package];
        let root = output
            .join("inst/slinker/datalib")
            .join(identity.name.as_str());
        let data = root.join("data");
        fs::create_dir_all(&data)?;
        fs::write(
            root.join("DESCRIPTION"),
            format!(
                "Package: {}\nVersion: {}\n",
                identity.name, identity.version
            ),
        )?;
        fs::write(data.join("Rdata.rdb"), &files.rdb)?;
        fs::write(data.join("Rdata.rdx"), &files.rdx)?;
        fs::write(data.join("Rdata.rds"), &files.rds)?;
    }
    Ok(())
}
