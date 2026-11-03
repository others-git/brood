use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Env {
    data: PathBuf,
    cfg: PathBuf,
    session: String,
}

impl Env {
    fn new(name: &str, config: &str) -> Env {
        let base = std::env::temp_dir().join(format!("brood-hook-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("cfg/brood")).unwrap();
        std::fs::write(base.join("cfg/brood/config.toml"), config).unwrap();
        Env {
            data: base.join("data"),
            cfg: base.join("cfg"),
            session: format!("sess-{name}"),
        }
    }

    fn hook(&self, mut ev: Value) -> Option<Value> {
        ev["session_id"] = json!(self.session);
        let mut child = Command::new(env!("CARGO_BIN_EXE_brood"))
            .arg("hook")
            .env("BROOD_DATA_DIR", &self.data)
            .env("XDG_CONFIG_HOME", &self.cfg)
            .env("HOME", &self.cfg)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(ev.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        serde_json::from_slice(&out.stdout).ok()
    }

    fn prompt(&self) {
        self.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}));
    }

    fn pre(&self, tool: &str, input: Value, agent: Option<&str>) -> Option<String> {
        let mut ev =
            json!({"hook_event_name": "PreToolUse", "tool_name": tool, "tool_input": input});
        if let Some(a) = agent {
            ev["agent_id"] = json!(a);
        }
        self.hook(ev).map(|v| {
            assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
            v["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .to_owned()
        })
    }

    fn edit(&self, file: &str, lines: usize) {
        let input =
            json!({"file_path": file, "old_string": "a", "new_string": "x\n".repeat(lines)});
        assert!(self.pre("Edit", input, None).is_none());
    }

    fn build(&self, cmd: &str, timeout: Option<u64>, agent: Option<&str>) -> Option<String> {
        let mut input = json!({"command": cmd});
        if let Some(t) = timeout {
            input["timeout"] = json!(t);
        }
        self.pre("Bash", input, agent)
    }
}

#[test]
fn small_edit_then_capped_build() {
    let e = Env::new("small", "");
    e.prompt();
    assert!(
        e.build("dotnet build", Some(60000), None)
            .unwrap()
            .contains("quick_builds")
    );
    e.edit("/p/Player.cs", 3);
    assert!(e.build("dotnet build", Some(60000), None).is_none());
    let r = e.build("dotnet build", None, None).unwrap();
    assert!(r.contains("timeout") && r.contains("120000"));
    assert!(
        e.build("dotnet build", Some(600000), None)
            .unwrap()
            .contains("timeout")
    );
    // Replays stay denied even after a small edit.
    let r = e.build("tools/run_replays.sh", Some(60000), None).unwrap();
    assert!(r.contains("quick_builds"));
}

#[test]
fn large_edit_then_build_denied() {
    let e = Env::new("large", "");
    e.prompt();
    e.edit("/p/Player.cs", 40);
    assert!(
        e.build("dotnet build", Some(60000), None)
            .unwrap()
            .contains("quick_builds")
    );
}

#[test]
fn quick_builds_match_without_edit() {
    let e = Env::new(
        "qb",
        "quick_builds = ['^dotnet build\\b']\nquick_build_timeout_secs = 30\n",
    );
    e.prompt();
    assert!(
        e.build("dotnet build g.csproj", Some(30000), None)
            .is_none()
    );
    assert!(
        e.build("dotnet build g.csproj", Some(31000), None)
            .unwrap()
            .contains("30000")
    );
    assert!(e.build("cargo build", Some(1000), None).is_some());
}

#[test]
fn subagent_unaffected() {
    let e = Env::new("sub", "");
    e.prompt();
    assert!(e.build("dotnet build", None, Some("agent1")).is_none());
    assert!(
        e.build("tools/run_replays.sh", None, Some("agent1"))
            .is_none()
    );
}
