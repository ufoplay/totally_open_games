//! Pure-Rust stand-ins for the SDL3 utilities upstream calls (string folding, glob matching, path info and the
//! preference directory). They are not Aeron functions, so they carry no `#[doc(alias)]` for usage tracking.

use std::cmp::Ordering;
use std::fs;
use std::path::PathBuf;

/// `SDL_strcasecmp`.
///
/// DEVIATION: folds ASCII only. SDL folds the full Unicode range; game file names are ASCII.
pub(crate) fn strcasecmp(left: &str, right: &str) -> Ordering {
    left.bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(right.bytes().map(|byte| byte.to_ascii_lowercase()))
}

/// The matcher behind `SDL_GlobDirectory` for one path component: `*` matches any run of bytes and `?` matches one
/// byte, neither of them crossing a `/`.
pub(crate) fn glob_match(pattern: &str, value: &str, case_insensitive: bool) -> bool {
    let fold = |byte: u8| {
        if case_insensitive {
            byte.to_ascii_lowercase()
        } else {
            byte
        }
    };
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let (mut p, mut v) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while v < value.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            p += 1;
            backtrack = Some((p, v));
            continue;
        }
        if p < pattern.len()
            && value[v] != b'/'
            && (pattern[p] == b'?' || fold(pattern[p]) == fold(value[v]))
        {
            p += 1;
            v += 1;
            continue;
        }
        match backtrack {
            Some((star_p, star_v)) if value[star_v] != b'/' => {
                p = star_p;
                v = star_v + 1;
                backtrack = Some((star_p, star_v + 1));
            }
            _ => return false,
        }
    }
    pattern[p..].iter().all(|&byte| byte == b'*')
}

/// `SDL_GetCurrentDirectory`: the working directory with a trailing separator.
pub(crate) fn current_directory() -> Option<String> {
    let mut path = std::env::current_dir().ok()?.to_str()?.to_owned();
    if !path.ends_with('/') && !path.ends_with('\\') {
        path.push('/');
    }
    Some(path)
}

/// `SDL_GetPrefPath`: the per-user data directory for `org`/`app`, created if missing, with a trailing separator.
pub(crate) fn pref_path(org: &str, app: &str) -> Option<String> {
    if app.is_empty() {
        return None;
    }
    let mut path = platform_data_dir()?;
    if !org.is_empty() {
        path.push(org);
    }
    path.push(app);
    fs::create_dir_all(&path).ok()?;
    let mut path = path.to_str()?.to_owned();
    path.push('/');
    Some(path)
}

fn platform_data_dir() -> Option<PathBuf> {
    let non_empty = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    if cfg!(windows) {
        non_empty("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        Some(std::env::home_dir()?.join("Library/Application Support"))
    } else {
        match non_empty("XDG_DATA_HOME") {
            Some(path) => Some(PathBuf::from(path)),
            None => Some(std::env::home_dir()?.join(".local/share")),
        }
    }
}

/// `SDL_RemovePath`: removes a file or an empty directory. A missing path counts as removed.
pub(crate) fn remove_path(path: &str) -> std::io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match result {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strcasecmp_orders_by_lowercase() {
        assert_eq!(strcasecmp("ABC", "abc"), Ordering::Equal);
        assert_eq!(strcasecmp("a_", "AA"), Ordering::Less);
        assert_eq!(strcasecmp("B", "a"), Ordering::Greater);
    }

    #[test]
    fn glob_match_wildcards() {
        assert!(glob_match("*.plt", "PILOT.PLT", true));
        assert!(!glob_match("*.plt", "PILOT.PLT", false));
        assert!(glob_match("TIMES*.ABP", "TIMES12.ABP", false));
        assert!(glob_match("*", "anything", false));
        assert!(glob_match("a?c", "abc", false));
        assert!(!glob_match("a?c", "ac", false));
        assert!(glob_match("*a*b", "xxaxxb", false));
        assert!(!glob_match("*", "dir/file", false));
        assert!(glob_match("", "", false));
        assert!(!glob_match("", "x", false));
    }
}
