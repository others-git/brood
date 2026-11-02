use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn git(dir: &Path, args: &[&str]) -> Output {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    o
}

fn repo(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("brood-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    git(&d, &["init", "-q", "-b", "main"]);
    std::fs::write(d.join("f.txt"), "base\n").unwrap();
    git(&d, &["add", "."]);
    git(&d, &["commit", "-qm", "base"]);
    d
}

fn worktree(d: &Path, branch: &str) -> PathBuf {
    let w = d.join("wt").join(branch);
    git(d, &["worktree", "add", "-q", "-b", branch, w.to_str().unwrap()]);
    w
}

fn brood(d: &Path, args: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_brood"))
        .arg("merge")
        .args(args)
        .current_dir(d)
        .output()
        .unwrap();
    (o.status.code().unwrap(), String::from_utf8_lossy(&o.stdout).into_owned())
}

fn branches(d: &Path) -> String {
    String::from_utf8_lossy(&git(d, &["branch", "--list"]).stdout).into_owned()
}

#[test]
fn clean_merge_with_pycache_and_verify() {
    let d = repo("clean");
    let w = worktree(&d, "agent-a");
    std::fs::write(w.join("new.txt"), "x\n").unwrap();
    git(&w, &["add", "."]);
    git(&w, &["commit", "-qm", "add"]);
    std::fs::create_dir_all(w.join("__pycache__")).unwrap();
    std::fs::write(w.join("__pycache__/a.pyc"), "x").unwrap();
    let (code, out) = brood(&d, &["agent-a", "--verify", "test -f new.txt"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("merged agent-a (1 commits), verify ok"), "{out}");
    assert!(d.join("new.txt").exists());
    assert!(!w.exists());
    assert!(!branches(&d).contains("agent-a"));
    assert!(d.join(".git/brood-verify.log").exists());
}

#[test]
fn merge_by_path() {
    let d = repo("path");
    let w = worktree(&d, "agent-p");
    std::fs::write(w.join("p.txt"), "x\n").unwrap();
    git(&w, &["add", "."]);
    git(&w, &["commit", "-qm", "p"]);
    let (code, out) = brood(&d, &[w.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("verify skipped"));
}

#[test]
fn verify_failure_keeps_everything() {
    let d = repo("vfail");
    let w = worktree(&d, "agent-v");
    std::fs::write(w.join("v.txt"), "x\n").unwrap();
    git(&w, &["add", "."]);
    git(&w, &["commit", "-qm", "v"]);
    let (code, out) = brood(&d, &["agent-v", "--verify", "echo boom; exit 3"]);
    assert_eq!(code, 1);
    assert!(out.contains("boom") && out.contains("brood-verify.log"), "{out}");
    assert!(w.exists() && branches(&d).contains("agent-v"));
}

#[test]
fn conflict_keeps_everything() {
    let d = repo("conflict");
    let w = worktree(&d, "agent-c");
    std::fs::write(w.join("f.txt"), "theirs\n").unwrap();
    git(&w, &["commit", "-qam", "theirs"]);
    std::fs::write(d.join("f.txt"), "ours\n").unwrap();
    git(&d, &["commit", "-qam", "ours"]);
    let (code, out) = brood(&d, &["agent-c"]);
    assert_eq!(code, 1);
    assert!(out.contains("conflict: f.txt") && out.contains("worktree and branch kept"), "{out}");
    assert!(w.exists() && branches(&d).contains("agent-c"));
    assert!(d.join(".git/MERGE_HEAD").exists());
}

#[test]
fn dirty_worktree_refused() {
    let d = repo("dirty");
    let w = worktree(&d, "agent-d");
    std::fs::write(w.join("real.rs"), "x").unwrap();
    let (code, out) = brood(&d, &["agent-d"]);
    assert_eq!(code, 2);
    assert!(out.contains("real.rs"), "{out}");
    assert!(w.exists());
}
