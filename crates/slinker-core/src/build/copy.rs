use super::MaterializationContext;
use crate::ir::ProgramIr;
use std::fs;
use std::path::Path;

pub(super) fn copy_root_resources(source: &Path, output: &Path) -> Result<(), std::io::Error> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(
            name.to_str(),
            Some("DESCRIPTION" | "NAMESPACE" | "MD5" | "R" | "target" | ".git")
        ) {
            continue;
        }
        copy_entry(&entry.path(), &output.join(name))?;
    }
    Ok(())
}

pub(super) fn copy_linked_resources(
    program: &ProgramIr,
    context: MaterializationContext<'_>,
    output: &Path,
) -> Result<(), std::io::Error> {
    for (id, resource) in program.indexed_resources() {
        let package = program.package(resource.package).identity();
        let source = context.resource(id);
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
    context: MaterializationContext<'_>,
    output: &Path,
) -> Result<(), std::io::Error> {
    for (package, _) in program.dataset_libraries() {
        let identity = program.package(package).identity();
        let files = context.dataset_library(package);
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

pub(super) fn copy_entry(source: &Path, target: &Path) -> Result<(), std::io::Error> {
    if source.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_entry(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, target)?;
    }
    Ok(())
}
