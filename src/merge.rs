//! `brood merge`: merge a worktree agent's branch, optionally verify, and
//! clean up the worktree and branch only on success, in one call.

use crate::{config, guard, hook};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, PartialEq, Clone)]
pub struct Worktree {
    pub path: String,
    pub branch: Option<String>,
}

/// Entries of `git worktree list --porcelain`; the first is the main worktree.
pub fn parse_worktrees(porcelain: &str) -> Vec<Worktree> {
    let mut out: Vec<Worktree> = vec![];
    for line in porcelain.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            out.push(Worktree {
                path: p.to_owned(),
                branch: None,
            });
        } else if let Some(b) = line.strip_prefix("branch ")
            && let Some(w) = out.last_mut()
        {
            w.branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_owned());
        }
    }
    out
}

fn same_path(a: &str, b: &str) -> bool {
    let canon = |s: &str| {
        std::fs::canonicalize(s).unwrap_or_else(|_| PathBuf::from(s.trim_end_matches('/')))
    };
    canon(a) == canon(b)
}

/// Resolve a branch name or worktree path to a non-main worktree with a branch.
pub fn resolve(wts: &[Worktree], arg: &str) -> Result<Worktree, String> {
    let found = wts
        .iter()
        .enumerate()
        .find(|(_, w)| w.branch.as_deref() == Some(arg) || same_path(&w.path, arg));
    match found {
        None => Err(format!("no worktree found for '{arg}'")),
        Some((0, _)) => Err(format!("'{arg}' is the main worktree, not an agent worktree")),
        Some((_, w)) if w.branch.is_none() => {
            Err(format!("worktree {} has a detached HEAD, no branch to merge", w.path))
        }
        Some((_, w)) => Ok(w.clone()),
    }
}

fn git(dir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git").arg("-C").arg(dir).args(args).output()
}

fn text(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn refuse(msg: &str) -> i32 {
    println!("brood merge: refused: {msg}");
    2
}

/// Dirty non-junk entries of a worktree, from `git status --porcelain` lines.
pub fn real_dirty(lines: &str, junk: &[String]) -> Vec<String> {
    lines
        .lines()
        .filter(|l| !l.is_empty() && !guard::is_junk(l, junk))
        .map(str::to_owned)
        .collect()
}

pub fn run(arg: &str, verify: Option<&str>, transcript: Option<&str>) -> i32 {
    let cfg = config::load();
    let cwd = PathBuf::from(".");
    let list = match git(&cwd, &["worktree", "list", "--porcelain"]) {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return refuse("not in a git repository"),
    };
    let wts = parse_worktrees(&list);
    let wt = match resolve(&wts, arg) {
        Ok(w) => w,
        Err(e) => return refuse(&e),
    };
    let branch = wt.branch.clone().unwrap_or_default();
    let here = git(&cwd, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    if same_path(&here, &wt.path) {
        return refuse("run this from the main repo, not inside the agent's worktree");
    }

    // Dirty worktree.
    let status = git(
        Path::new(&wt.path),
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    .unwrap_or_default();
    let dirty = real_dirty(&status, &cfg.worktree_junk);
    if !dirty.is_empty() {
        let mut msg = format!(
            "worktree {} has uncommitted or untracked files:",
            wt.path
        );
        for d in dirty.iter().take(10) {
            msg += &format!("\n  {d}");
        }
        if dirty.len() > 10 {
            msg += &format!("\n  ... and {} more", dirty.len() - 10);
        }
        return refuse(&msg);
    }

    // Live agent (only with session context).
    if let Some(t) = transcript.filter(|t| !t.is_empty())
        && let Some(id) = hook::live_agent_for_worktree(t, &branch, &wt.path)
    {
        return refuse(&format!(
            "agent {id} is still running (or was just resumed); wait for its final report"
        ));
    }

    let count = git(&cwd, &["rev-list", "--count", &format!("HEAD..{branch}")])
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|_| "?".into());

    // Merge.
    let m = match git(&cwd, &["merge", "--no-edit", &branch]) {
        Ok(m) => m,
        Err(e) => return refuse(&format!("cannot run git: {e}")),
    };
    if !m.status.success() {
        let out = text(&m);
        let conflicts = git(&cwd, &["diff", "--name-only", "--diff-filter=U"])
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        for f in conflicts.lines() {
            println!("conflict: {f}");
        }
        for l in out
            .lines()
            .filter(|l| l.contains("CONFLICT") || l.starts_with("error:") || l.starts_with("fatal:"))
        {
            println!("{l}");
        }
        println!("brood merge: merge failed; resolve or `git merge --abort`; worktree and branch kept");
        return 1;
    }

    // Verify.
    let mut verified = "skipped";
    if let Some(cmd) = verify {
        let log = git(&cwd, &["rev-parse", "--path-format=absolute", "--git-path", "brood-verify.log"])
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default();
        let log = if log.is_empty() { ".git/brood-verify.log".into() } else { log };
        let ok = (|| -> std::io::Result<bool> {
            let f = std::fs::File::create(&log)?;
            let st = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .stdin(Stdio::null())
                .stdout(f.try_clone()?)
                .stderr(f)
                .status()?;
            Ok(st.success())
        })()
        .unwrap_or(false);
        if !ok {
            let body = std::fs::read_to_string(&log).unwrap_or_default();
            let lines: Vec<&str> = body.lines().collect();
            for l in &lines[lines.len().saturating_sub(40)..] {
                println!("{l}");
            }
            println!(
                "brood merge: verify failed; merge commit kept, worktree and branch kept; full log: {log}"
            );
            return 1;
        }
        verified = "ok";
    }

    // Cleanup.
    let rm = git(&cwd, &["worktree", "remove", "--force", &wt.path]);
    if !rm.as_ref().is_ok_and(|o| o.status.success()) {
        println!(
            "brood merge: merged and verified, but `git worktree remove` failed: {}",
            rm.map(|o| text(&o).trim().to_owned()).unwrap_or_default()
        );
        return 1;
    }
    let br = git(&cwd, &["branch", "-d", &branch]);
    if !br.as_ref().is_ok_and(|o| o.status.success()) {
        println!(
            "brood merge: merged, worktree removed, but `git branch -d` failed: {}",
            br.map(|o| text(&o).trim().to_owned()).unwrap_or_default()
        );
        return 1;
    }
    println!("merged {branch} ({count} commits), verify {verified}, worktree and branch removed");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORCELAIN: &str = "worktree /r\nHEAD abc\nbranch refs/heads/main\n\nworktree /r/.claude/worktrees/agent-1\nHEAD def\nbranch refs/heads/worktree-agent-1\n\nworktree /r/det\nHEAD 123\ndetached\n";

    #[test]
    fn parse_and_resolve() {
        let w = parse_worktrees(PORCELAIN);
        assert_eq!(w.len(), 3);
        assert_eq!(w[1].branch.as_deref(), Some("worktree-agent-1"));
        assert_eq!(resolve(&w, "worktree-agent-1").unwrap().path, "/r/.claude/worktrees/agent-1");
        assert_eq!(
            resolve(&w, "/r/.claude/worktrees/agent-1/").unwrap().branch.as_deref(),
            Some("worktree-agent-1")
        );
        assert!(resolve(&w, "main").unwrap_err().contains("main worktree"));
        assert!(resolve(&w, "/r/det").unwrap_err().contains("detached"));
        assert!(resolve(&w, "nope").unwrap_err().contains("no worktree"));
    }

    #[test]
    fn dirty_filter() {
        let s = "?? __pycache__/a.pyc\n?? x.txt\n M y.rs\n";
        assert_eq!(real_dirty(s, &[]), vec!["?? x.txt", " M y.rs"]);
    }
}
