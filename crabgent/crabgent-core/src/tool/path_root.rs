use std::path::{Component, Path, PathBuf};

use crate::error::ToolError;

pub(super) fn validate_existing_path(
    path: &Path,
    root: Option<&Path>,
) -> Result<PathBuf, ToolError> {
    let Some(root) = root else {
        return Ok(path.to_path_buf());
    };

    let root = canonical_root(root)?;
    reject_parent_dir(path)?;
    let candidate = rooted_candidate(path, &root);
    let canonical = candidate
        .canonicalize()
        .map_err(|err| existing_path_io_error(&candidate, &err))?;
    ensure_inside_root(&canonical, &root)?;
    Ok(canonical)
}

pub(super) fn validate_creatable_path(
    path: &Path,
    root: Option<&Path>,
) -> Result<PathBuf, ToolError> {
    let Some(root) = root else {
        return Ok(path.to_path_buf());
    };

    let root = canonical_root(root)?;
    reject_parent_dir(path)?;
    let candidate = rooted_candidate(path, &root);
    if let Ok(metadata) = candidate.symlink_metadata() {
        if metadata.file_type().is_symlink() {
            return Err(ToolError::Permission(
                "path is a symlink under configured root".to_owned(),
            ));
        }
        let canonical = candidate
            .canonicalize()
            .map_err(|err| ToolError::Io(err.to_string()))?;
        ensure_inside_root(&canonical, &root)?;
        return Ok(canonical);
    }

    let parent = candidate
        .parent()
        .ok_or_else(|| ToolError::InvalidArgs("path has no parent".into()))?;
    let anchor = existing_ancestor(parent)?;
    let canonical_anchor = anchor
        .canonicalize()
        .map_err(|err| ToolError::Io(err.to_string()))?;
    ensure_inside_root(&canonical_anchor, &root)?;
    Ok(candidate)
}

fn canonical_root(root: &Path) -> Result<PathBuf, ToolError> {
    root.canonicalize()
        .map_err(|err| ToolError::Io(format!("invalid root {}: {err}", root.display())))
}

fn rooted_candidate(path: &Path, root: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn reject_parent_dir(path: &Path) -> Result<(), ToolError> {
    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(ToolError::Permission(
            "path escapes configured root".to_owned(),
        ));
    }
    Ok(())
}

fn existing_ancestor(path: &Path) -> Result<&Path, ToolError> {
    path.ancestors()
        .find(|ancestor| ancestor.exists())
        .ok_or_else(|| ToolError::NotFound("path not found".to_owned()))
}

pub(super) fn existing_path_io_error(_path: &Path, err: &std::io::Error) -> ToolError {
    match err.kind() {
        std::io::ErrorKind::NotFound => ToolError::NotFound("path not found".to_owned()),
        _ => ToolError::Io("path I/O failed".to_owned()),
    }
}

fn ensure_inside_root(path: &Path, root: &Path) -> Result<(), ToolError> {
    if path.starts_with(root) {
        return Ok(());
    }
    Err(ToolError::Permission(
        "path is outside configured root".to_owned(),
    ))
}
