use std::ffi::OsStr;

pub const DIST_NAME: &str = "crabgent";

pub fn app_config_name() -> String {
    match std::env::args_os()
        .next()
        .as_deref()
        .map(std::path::Path::new)
        .and_then(std::path::Path::file_stem)
        .and_then(OsStr::to_str)
    {
        Some(DIST_NAME) => DIST_NAME.to_owned(),
        _ => ["crabgent", "-", "tri", "stan"].concat(),
    }
}
