//! Project identity. A WSL session (`/mnt/d/x`) and a native Windows session
//! (`D:\x`) on the same folder must map to one project, so every path is
//! reduced to one canonical key (the WSL form) before it is used as identity.

use std::path::PathBuf;
use std::process::Command;

/// Canonical project key for a path in any of: POSIX, Windows drive, `\\?\` verbatim,
/// or `\\wsl$\<distro>\...` form.
pub fn normalize(p: &str) -> String {
    let mut s = p.trim().replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_owned();
    }
    for pre in ["//wsl$/", "//wsl.localhost/"] {
        if let Some(rest) = s.strip_prefix(pre) {
            // Drop the distro name; what follows is a Linux path.
            s = match rest.split_once('/') {
                Some((_, tail)) => format!("/{tail}"),
                None => "/".to_owned(),
            };
        }
    }
    let b = s.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'/')
    {
        s = format!("/mnt/{}{}", (b[0] as char).to_ascii_lowercase(), &s[2..]);
    }
    let mut out = String::with_capacity(s.len());
    for part in s.split('/').filter(|c| !c.is_empty()) {
        out.push('/');
        out.push_str(part);
    }
    if out.is_empty() { "/".into() } else { out }
}

/// `/mnt/d/x` -> `D:\x`. None for paths that are not on a mounted drive.
pub fn to_windows(key: &str) -> Option<String> {
    let rest = key.strip_prefix("/mnt/")?;
    let (drive, tail) = rest.split_once('/').unwrap_or((rest, ""));
    if drive.len() != 1 || !drive.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    Some(format!(
        "{}:\\{}",
        drive.to_ascii_uppercase(),
        tail.replace('/', "\\")
    ))
}

/// The key as a path this machine can open.
pub fn native(key: &str) -> PathBuf {
    if cfg!(windows)
        && let Some(w) = to_windows(key)
    {
        return PathBuf::from(w);
    }
    PathBuf::from(key)
}

pub fn project_id(key: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")[..12].to_owned()
}

pub fn project_name(key: &str) -> String {
    key.rsplit('/').next().unwrap_or(key).to_owned()
}

pub fn git_toplevel(cwd: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--show-toplevel"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// Project root for a working directory: the git toplevel when there is one.
pub fn project_root(cwd: &str) -> String {
    git_toplevel(cwd).unwrap_or_else(|| cwd.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_and_windows_agree() {
        let k = "/mnt/d/REPOS/x";
        assert_eq!(normalize("/mnt/d/REPOS/x"), k);
        assert_eq!(normalize(r"D:\REPOS\x"), k);
        assert_eq!(normalize("d:/REPOS/x/"), k);
        assert_eq!(normalize(r"\\?\D:\REPOS\x"), k);
        assert_eq!(project_id(&normalize(r"D:\REPOS\x")), project_id(k));
    }

    #[test]
    fn other_forms() {
        assert_eq!(normalize(r"\\wsl$\Ubuntu\home\u\p"), "/home/u/p");
        assert_eq!(normalize("/home//u/p/"), "/home/u/p");
        assert_eq!(normalize("C:"), "/mnt/c");
        assert_eq!(normalize("/"), "/");
        assert_eq!(to_windows("/mnt/d/a/b").as_deref(), Some(r"D:\a\b"));
        assert_eq!(to_windows("/home/u"), None);
        assert_eq!(project_name("/mnt/d/REPOS/x"), "x");
    }
}
