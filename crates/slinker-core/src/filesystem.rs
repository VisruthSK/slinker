use std::fs;
use std::path::Path;

pub(crate) fn copy_entry(source: &Path, target: &Path) -> std::io::Result<()> {
    let kind = fs::symlink_metadata(source)?.file_type();
    if kind.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_entry(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else if kind.is_file() {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, target)?;
    } else {
        return Err(std::io::Error::other(format!(
            "unsupported filesystem entry: {}",
            source.display()
        )));
    }
    Ok(())
}
