use std::path::{Path, MAIN_SEPARATOR};

pub fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace(MAIN_SEPARATOR, "/")
}

pub fn relative_slash_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                ".".to_owned()
            } else {
                slash_path(relative)
            }
        })
        .unwrap_or_else(|_| slash_path(path))
}
