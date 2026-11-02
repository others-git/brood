//! Card storage: `<project>/kanban/<column>/<id>.md`, TOML frontmatter between `+++` lines.
//! Writes are atomic (temp file + rename). A move is a rename between column dirs.
//! Callers serialise writes (the board holds one mutex).

use super::{date8, iso, now_secs, rand_hex};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Meta {
    pub id: String,
    pub title: String,
    pub rev: u64,
    pub priority: String,
    pub tags: Vec<String>,
    /// Session id that claimed the card, or empty.
    pub owner: String,
    pub created: String,
    pub updated: String,
    /// Frontmatter keys brood does not know: kept, so a rewrite never drops them.
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl Default for Meta {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            rev: 1,
            priority: "normal".into(),
            tags: vec![],
            owner: String::new(),
            created: String::new(),
            updated: String::new(),
            extra: toml::Table::new(),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Card {
    #[serde(flatten)]
    pub meta: Meta,
    pub body: String,
    /// The user's review decision: a trailing `## Decision` section in the file.
    pub decision: String,
    pub column: String,
    /// Hash of the raw file bytes. A writer sends it back; a mismatch means the file changed.
    pub ver: String,
    /// The frontmatter did not parse. Such a card is shown but never rewritten.
    #[serde(skip)]
    pub malformed: bool,
}

#[derive(Debug)]
pub enum Err {
    NotFound,
    Duplicate(Box<Card>),
    BadRequest(String),
    Conflict(Box<Card>),
    Claimed(String),
    Io(String),
}

impl From<std::io::Error> for Err {
    fn from(e: std::io::Error) -> Self {
        Err::Io(e.to_string())
    }
}

#[derive(Default, Debug)]
pub struct Patch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub decision: Option<String>,
    pub priority: Option<String>,
    pub tags: Option<Vec<String>>,
    pub owner: Option<String>,
}

pub fn dir(root: &Path) -> PathBuf {
    root.join("kanban")
}

/// FNV-1a over the raw file text, as 16 hex digits.
pub fn ver_of(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn render(c: &Card) -> Result<String, Err> {
    let table = toml::Value::try_from(&c.meta).map_err(|e| Err::Io(format!("frontmatter: {e}")))?;
    let meta = toml::to_string(&table).map_err(|e| Err::Io(format!("frontmatter: {e}")))?;
    let body = escape_marker(c.body.trim_end());
    let dec = c.decision.trim();
    if dec.is_empty() {
        Ok(format!("+++\n{meta}+++\n\n{body}\n"))
    } else {
        Ok(format!(
            "+++\n{meta}+++\n\n{body}\n\n{DECISION_MARK}\n\n{}\n",
            escape_marker(dec)
        ))
    }
}

const DECISION_MARK: &str = "## Decision";

/// A line that is `## Decision` (after any leading backslashes) gets one more backslash,
/// so only a real section marker is ever a bare `## Decision` line.
fn escape_marker(s: &str) -> String {
    s.split('\n')
        .map(|l| {
            if l.trim_start_matches('\\') == DECISION_MARK {
                format!("\\{l}")
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn unescape_marker(s: &str) -> String {
    s.split('\n')
        .map(|l| match l.strip_prefix('\\') {
            Some(r) if r.trim_start_matches('\\') == DECISION_MARK => r,
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Split a body into (description, decision) at the first bare `## Decision` line.
fn split_decision(body: &str) -> (String, String) {
    let mut off = 0;
    for l in body.split('\n') {
        if l == DECISION_MARK {
            let desc = body[..off].trim_end();
            let dec = body[off + l.len()..].trim();
            return (unescape_marker(desc), unescape_marker(dec));
        }
        off += l.len() + 1;
    }
    (unescape_marker(body), String::new())
}

/// Parse a card file. The id is always the file name, whatever the frontmatter says.
pub fn parse(text: &str, column: &str, stem: &str) -> Card {
    let ver = ver_of(text);
    let t = text.replace("\r\n", "\n");
    let mut malformed = false;
    let (mut meta, body) = match t.strip_prefix("+++\n") {
        Some(rest) => {
            let split = if let Some(after) = rest.strip_prefix("+++") {
                Some(("", after))
            } else {
                rest.find("\n+++").map(|i| (&rest[..i + 1], &rest[i + 4..]))
            };
            match split {
                Some((m, after)) => (
                    toml::from_str::<Meta>(m).unwrap_or_else(|_| {
                        malformed = true;
                        Meta::default()
                    }),
                    after.trim_start_matches('\n').trim_end().to_owned(),
                ),
                None => {
                    malformed = true;
                    (Meta::default(), t.trim_end().to_owned())
                }
            }
        }
        None => (Meta::default(), t.trim_end().to_owned()),
    };
    let (body, decision) = split_decision(&body);
    meta.id = stem.to_owned();
    if meta.title.is_empty() {
        meta.title = stem.to_owned();
    }
    if meta.rev == 0 {
        meta.rev = 1;
    }
    Card {
        meta,
        body,
        decision,
        column: column.to_owned(),
        ver,
        malformed,
    }
}

fn slug(title: &str) -> String {
    let mut s = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.is_empty() && !s.ends_with('-') {
            s.push('-');
        }
    }
    let s: String = s.trim_end_matches('-').chars().take(40).collect();
    let s = s.trim_end_matches('-');
    if s.is_empty() {
        "card".into()
    } else {
        s.into()
    }
}

/// `YYYYMMDD-xxxx-slug`.
pub fn make_id(secs: u64, hex: &str, title: &str) -> String {
    format!("{}-{}-{}", date8(secs), hex, slug(title))
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 120 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn check_col<'a>(cols: &'a [String], c: &str) -> Result<&'a str, Err> {
    cols.iter()
        .find(|x| x.as_str() == c)
        .map(String::as_str)
        .ok_or_else(|| {
            Err::BadRequest(format!(
                "unknown column '{c}'; columns: {}",
                cols.join(", ")
            ))
        })
}

static TMP: AtomicU64 = AtomicU64::new(0);

pub fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("x");
    let tmp = path.with_file_name(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        TMP.fetch_add(1, Ordering::Relaxed)
    ));
    let res = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

fn read_card(path: &Path, column: &str) -> Option<Card> {
    let stem = path.file_stem()?.to_str()?;
    if stem.starts_with('.') || path.extension()? != "md" {
        return None;
    }
    let text = String::from_utf8_lossy(&fs::read(path).ok()?).into_owned();
    Some(parse(&text, column, stem))
}

pub fn list(root: &Path, cols: &[String]) -> Vec<Card> {
    let mut out = vec![];
    for col in cols {
        let Ok(rd) = fs::read_dir(dir(root).join(col)) else {
            continue;
        };
        let mut cards: Vec<Card> = rd
            .flatten()
            .filter_map(|e| read_card(&e.path(), col))
            .collect();
        cards.sort_by(|a, b| a.meta.id.cmp(&b.meta.id));
        out.extend(cards);
    }
    out
}

pub fn find(root: &Path, cols: &[String], id: &str) -> Option<(PathBuf, Card)> {
    if !valid_id(id) {
        return None;
    }
    cols.iter().find_map(|col| {
        let p = dir(root).join(col).join(format!("{id}.md"));
        read_card(&p, col).map(|c| (p, c))
    })
}

pub struct NewCard {
    pub title: String,
    pub body: String,
    pub column: Option<String>,
    pub priority: Option<String>,
    pub tags: Vec<String>,
}

pub fn create(root: &Path, cols: &[String], n: NewCard) -> Result<Card, Err> {
    let title = n.title.trim();
    if title.is_empty() {
        return Err(Err::BadRequest("title is required".into()));
    }
    let col = check_col(cols, n.column.as_deref().unwrap_or(&cols[0]))?;
    let now = now_secs();
    let id = make_id(now, &rand_hex(4), title);
    let mut card = Card {
        meta: Meta {
            id: id.clone(),
            title: title.to_owned(),
            priority: n.priority.unwrap_or_else(|| "normal".into()),
            tags: n.tags,
            created: iso(now),
            updated: iso(now),
            ..Meta::default()
        },
        body: n.body.trim_end().to_owned(),
        decision: String::new(),
        column: col.to_owned(),
        ver: String::new(),
        malformed: false,
    };
    let cdir = dir(root).join(col);
    fs::create_dir_all(&cdir)?;
    // The lock dir is machine-local; the migration marker is committed.
    let gi = dir(root).join(".gitignore");
    if !gi.exists() {
        let _ = fs::write(&gi, ".locks/\n");
    }
    let marker = dir(root).join(".migrated");
    if !marker.exists() {
        let _ = fs::write(&marker, "");
    }
    let text = render(&card)?;
    atomic_write(&cdir.join(format!("{id}.md")), &text)?;
    card.ver = ver_of(&text);
    Ok(card)
}

/// The caller's view must match the file: same rev, same bytes (when it sent `ver`).
fn check(card: &Card, rev: u64, ver: Option<&str>) -> Result<(), Err> {
    if card.malformed {
        return Err(Err::BadRequest(
            "card frontmatter is malformed; fix the file by hand".into(),
        ));
    }
    if card.meta.rev != rev || ver.is_some_and(|v| v != card.ver) {
        return Err(Err::Conflict(Box::new(card.clone())));
    }
    Ok(())
}

/// Re-read right before writing: a hand edit since `seen` was read is a conflict.
fn unchanged(path: &Path, seen: &Card) -> Result<(), Err> {
    let Some(card) = read_card(path, &seen.column) else {
        return Err(Err::NotFound);
    };
    if card.ver == seen.ver {
        Ok(())
    } else {
        Err(Err::Conflict(Box::new(card)))
    }
}

fn apply(c: &mut Card, p: Patch) -> Result<(), Err> {
    if let Some(t) = p.title {
        if t.trim().is_empty() {
            return Err(Err::BadRequest("title must not be empty".into()));
        }
        c.meta.title = t.trim().to_owned();
    }
    if let Some(b) = p.body {
        c.body = b.trim_end().to_owned();
    }
    if let Some(d) = p.decision {
        c.decision = d.trim().to_owned();
    }
    if let Some(x) = p.priority {
        c.meta.priority = x;
    }
    if let Some(x) = p.tags {
        c.meta.tags = x;
    }
    if let Some(x) = p.owner {
        c.meta.owner = x;
    }
    Ok(())
}

pub fn update(
    root: &Path,
    cols: &[String],
    id: &str,
    rev: u64,
    ver: Option<&str>,
    p: Patch,
) -> Result<Card, Err> {
    let (path, card) = find(root, cols, id).ok_or(Err::NotFound)?;
    check(&card, rev, ver)?;
    let mut next = card.clone();
    apply(&mut next, p)?;
    next.meta.rev += 1;
    next.meta.updated = iso(now_secs());
    let text = render(&next)?;
    unchanged(&path, &card)?;
    atomic_write(&path, &text)?;
    next.ver = ver_of(&text);
    Ok(next)
}

pub fn move_card(
    root: &Path,
    cols: &[String],
    id: &str,
    rev: u64,
    ver: Option<&str>,
    to: &str,
) -> Result<Card, Err> {
    let to = check_col(cols, to)?;
    let (path, card) = find(root, cols, id).ok_or(Err::NotFound)?;
    check(&card, rev, ver)?;
    if card.column == to {
        return Ok(card);
    }
    let dest = dir(root).join(to).join(format!("{id}.md"));
    if let Some(other) = read_card(&dest, to) {
        return Err(Err::Duplicate(Box::new(other)));
    }
    let mut next = card.clone();
    next.meta.rev += 1;
    next.meta.updated = iso(now_secs());
    let text = render(&next)?;
    unchanged(&path, &card)?;
    atomic_write(&path, &text)?;
    fs::create_dir_all(dir(root).join(to))?;
    fs::rename(&path, &dest)?;
    next.column = to.to_owned();
    next.ver = ver_of(&text);
    Ok(next)
}

// ---- claims ----

fn lock_dir(root: &Path) -> PathBuf {
    dir(root).join(".locks")
}

fn lock_path(root: &Path, id: &str) -> PathBuf {
    lock_dir(root).join(format!("{id}.lock"))
}

/// Take the claim lock with an atomic `create_new`. A lock held by a session
/// for which `alive` is false is taken over. Err carries the holder.
pub fn try_lock(
    root: &Path,
    id: &str,
    session: &str,
    alive: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    if !valid_id(id) || session.is_empty() {
        return Err(String::new());
    }
    let _ = fs::create_dir_all(lock_dir(root));
    let path = lock_path(root, id);
    for _ in 0..3 {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                let _ = f.write_all(session.as_bytes());
                return Ok(());
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let owner = fs::read_to_string(&path).unwrap_or_default();
                let owner = owner.trim();
                if owner == session {
                    return Ok(());
                }
                // Empty = the winner has not written its id yet: not reclaimable.
                if owner.is_empty() || alive(owner) {
                    return Err(owner.to_owned());
                }
                let _ = fs::remove_file(&path);
            }
            Err(_) => return Err(String::new()),
        }
    }
    Err(String::new())
}

pub fn lock_owner(root: &Path, id: &str) -> Option<String> {
    fs::read_to_string(lock_path(root, id))
        .ok()
        .map(|s| s.trim().to_owned())
}

/// Remove the lock if `session` holds it (or `force`). True if a lock was removed.
pub fn unlock(root: &Path, id: &str, session: &str, force: bool) -> bool {
    match lock_owner(root, id) {
        Some(o) if force || o == session => fs::remove_file(lock_path(root, id)).is_ok(),
        _ => false,
    }
}

/// Ids of cards locked by `session`.
pub fn locks_of(root: &Path, session: &str) -> Vec<String> {
    let Ok(rd) = fs::read_dir(lock_dir(root)) else {
        return vec![];
    };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".lock")?.to_owned();
            (fs::read_to_string(e.path()).ok()?.trim() == session).then_some(id)
        })
        .collect()
}

/// Delete locks whose holder is not alive. Returns how many were removed.
pub fn sweep_locks(root: &Path, alive: &dyn Fn(&str) -> bool) -> usize {
    let Ok(rd) = fs::read_dir(lock_dir(root)) else {
        return 0;
    };
    let mut n = 0;
    for e in rd.flatten() {
        let owner = fs::read_to_string(e.path()).unwrap_or_default();
        let owner = owner.trim();
        if !owner.is_empty() && !alive(owner) && fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

/// SessionStart hint trigger: a git project that has no `kanban/` dir (hence no
/// `.migrated` marker either) has not been given a board yet.
pub fn needs_migration_hint(root: &Path) -> bool {
    !dir(root).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "brood-kb-{name}-{}-{}",
            std::process::id(),
            rand_hex(6)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn cols() -> Vec<String> {
        crate::config::default_columns()
    }

    fn new(title: &str) -> NewCard {
        NewCard {
            title: title.into(),
            body: "line one\n\nline two".into(),
            column: None,
            priority: None,
            tags: vec!["a".into(), "b".into()],
        }
    }

    #[test]
    fn frontmatter_round_trip() {
        let c = Card {
            meta: Meta {
                id: "20260101-abcd-x".into(),
                title: "Title: with \"quotes\" and +++".into(),
                rev: 7,
                priority: "high".into(),
                tags: vec!["t1".into()],
                owner: "sess".into(),
                created: "2026-01-01T00:00:00Z".into(),
                updated: "2026-01-02T00:00:00Z".into(),
                extra: toml::Table::new(),
            },
            body: "Body\n\n- item\n+++ not a fence".into(),
            decision: String::new(),
            column: "ready".into(),
            ver: String::new(),
            malformed: false,
        };
        let text = render(&c).unwrap();
        let mut back = parse(&text, "ready", "20260101-abcd-x");
        assert_eq!(back.ver, ver_of(&text));
        back.ver.clear();
        assert_eq!(back, c);
    }

    #[test]
    fn decision_round_trip_with_tricky_text() {
        for (body, dec) in [
            ("plain", "approved"),
            ("intro\n## Decision\nfake", "ok\n## Decision\nmore"),
            ("\\## Decision\n\\\\## Decision", "\\## Decision"),
            ("## Decision", ""),
            ("x\n\n## Decision\n\ny", "multi\n\nline\n\n- list"),
        ] {
            let mut c = parse("", "review", "a-b");
            c.body = body.into();
            c.decision = dec.into();
            let text = render(&c).unwrap();
            let back = parse(&text, "review", "a-b");
            assert_eq!(back.body, body, "{text}");
            assert_eq!(back.decision, dec, "{text}");
        }
        let mut c = parse("", "review", "a-b");
        c.body = "d".into();
        c.decision = "yes".into();
        assert!(render(&c).unwrap().ends_with("d\n\n## Decision\n\nyes\n"));
    }

    #[test]
    fn hand_written_card_gets_defaults() {
        let c = parse("just text\r\nmore", "backlog", "my-note");
        assert_eq!(c.meta.id, "my-note");
        assert_eq!(c.meta.title, "my-note");
        assert_eq!(c.meta.rev, 1);
        assert_eq!(c.body, "just text\nmore");
    }

    #[test]
    fn id_format() {
        let id = make_id(1_785_000_000, "a1b2", "Fix the Login -- bug!");
        assert_eq!(id, "20260725-a1b2-fix-the-login-bug");
        assert_eq!(make_id(0, "0000", "!!!"), "19700101-0000-card");
        let made = create(&tmp("id"), &cols(), new("Hello World"))
            .unwrap()
            .meta
            .id;
        let parts: Vec<&str> = made.splitn(3, '-').collect();
        assert_eq!(parts[0].len(), 8);
        assert!(parts[0].chars().all(|c| c.is_ascii_digit()));
        assert_eq!(parts[1].len(), 4);
        assert!(parts[1].chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(parts[2], "hello-world");
    }

    #[test]
    fn rev_conflict_and_move() {
        let root = tmp("rev");
        let c = create(&root, &cols(), new("Card")).unwrap();
        assert_eq!(c.column, "backlog");
        let id = c.meta.id.clone();
        let u = update(
            &root,
            &cols(),
            &id,
            1,
            None,
            Patch {
                title: Some("Renamed".into()),
                ..Patch::default()
            },
        )
        .unwrap();
        assert_eq!(u.meta.rev, 2);
        // Stale rev: conflict carries the current card.
        match update(&root, &cols(), &id, 1, None, Patch::default()) {
            Err(Err::Conflict(cur)) => assert_eq!(cur.meta.title, "Renamed"),
            other => panic!("expected conflict, got {other:?}"),
        }
        assert!(matches!(
            move_card(&root, &cols(), &id, 1, None, "done"),
            Err(Err::Conflict(_))
        ));
        let m = move_card(&root, &cols(), &id, 2, None, "done").unwrap();
        assert_eq!((m.column.as_str(), m.meta.rev), ("done", 3));
        assert!(dir(&root).join("done").join(format!("{id}.md")).exists());
        assert!(!dir(&root).join("backlog").join(format!("{id}.md")).exists());
        assert!(matches!(
            move_card(&root, &cols(), &id, 3, None, "nope"),
            Err(Err::BadRequest(_))
        ));
        assert_eq!(list(&root, &cols()).len(), 1);
        assert_eq!(
            fs::read_to_string(dir(&root).join(".gitignore")).unwrap(),
            ".locks/\n"
        );
    }

    #[test]
    fn hand_edit_gives_conflict() {
        let root = tmp("ver");
        let c = create(&root, &cols(), new("Card")).unwrap();
        let id = c.meta.id.clone();
        let path = dir(&root).join("backlog").join(format!("{id}.md"));
        // Same rev, same ver: fine.
        let u = update(&root, &cols(), &id, 1, Some(&c.ver), Patch::default()).unwrap();
        // A hand edit that leaves rev alone.
        let text = fs::read_to_string(&path)
            .unwrap()
            .replace("line one", "edited by hand");
        fs::write(&path, text).unwrap();
        match update(&root, &cols(), &id, 2, Some(&u.ver), Patch::default()) {
            Err(Err::Conflict(cur)) => assert!(cur.body.contains("edited by hand")),
            other => panic!("expected conflict, got {other:?}"),
        }
        assert!(matches!(
            move_card(&root, &cols(), &id, 2, Some(&u.ver), "done"),
            Err(Err::Conflict(_))
        ));
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("edited by hand")
        );
    }

    #[test]
    fn unknown_keys_survive_rewrite() {
        let root = tmp("extra");
        let c = create(&root, &cols(), new("Card")).unwrap();
        let id = c.meta.id.clone();
        let path = dir(&root).join("backlog").join(format!("{id}.md"));
        let text = fs::read_to_string(&path).unwrap().replacen(
            "+++\n\n",
            "estimate = 5\n[links]\nissue = \"#12\"\n+++\n\n",
            1,
        );
        fs::write(&path, text).unwrap();
        let cur = find(&root, &cols(), &id).unwrap().1;
        assert!(!cur.malformed);
        update(
            &root,
            &cols(),
            &id,
            1,
            Some(&cur.ver),
            Patch {
                title: Some("T2".into()),
                ..Patch::default()
            },
        )
        .unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("estimate = 5") && after.contains("issue = \"#12\""),
            "{after}"
        );
        assert!(after.contains("T2"));
    }

    #[test]
    fn malformed_frontmatter_refuses_writes() {
        let root = tmp("bad");
        let c = create(&root, &cols(), new("Card")).unwrap();
        let id = c.meta.id.clone();
        let path = dir(&root).join("backlog").join(format!("{id}.md"));
        let broken = "+++\nrev = \"oops\nid = \n+++\n\nprecious body\n";
        fs::write(&path, broken).unwrap();
        assert!(matches!(
            update(&root, &cols(), &id, 1, None, Patch::default()),
            Err(Err::BadRequest(_))
        ));
        assert!(matches!(
            move_card(&root, &cols(), &id, 1, None, "done"),
            Err(Err::BadRequest(_))
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), broken);
    }

    #[test]
    fn move_into_duplicate_id_conflicts() {
        let root = tmp("dup");
        let c = create(&root, &cols(), new("Card")).unwrap();
        let id = c.meta.id.clone();
        fs::create_dir_all(dir(&root).join("done")).unwrap();
        let dup = dir(&root).join("done").join(format!("{id}.md"));
        fs::write(&dup, "+++\n+++\n\nthe other one\n").unwrap();
        assert!(matches!(
            move_card(&root, &cols(), &id, 1, None, "done"),
            Err(Err::Duplicate(_))
        ));
        assert!(fs::read_to_string(&dup).unwrap().contains("the other one"));
        assert!(dir(&root).join("backlog").join(format!("{id}.md")).exists());
    }

    #[test]
    fn claim_race_has_one_winner() {
        let root = Arc::new(tmp("race"));
        let id = "20260101-aaaa-x";
        let handles: Vec<_> = ["s1", "s2"]
            .into_iter()
            .map(|s| {
                let root = root.clone();
                std::thread::spawn(move || try_lock(&root, id, s, &|_| true).is_ok())
            })
            .collect();
        let wins = handles
            .into_iter()
            .filter(|_| true)
            .map(|h| h.join().unwrap())
            .filter(|w| *w)
            .count();
        assert_eq!(wins, 1);
    }

    #[test]
    fn dead_lock_is_reclaimed_and_swept() {
        let root = tmp("dead");
        assert!(try_lock(&root, "c1", "old", &|_| true).is_ok());
        assert_eq!(try_lock(&root, "c1", "new", &|_| true), Err("old".into()));
        assert!(try_lock(&root, "c1", "new", &|s| s != "old").is_ok());
        assert_eq!(lock_owner(&root, "c1").as_deref(), Some("new"));
        assert!(try_lock(&root, "c2", "gone", &|_| true).is_ok());
        assert_eq!(sweep_locks(&root, &|s| s != "gone"), 1);
        assert_eq!(locks_of(&root, "new"), vec!["c1".to_string()]);
        assert!(unlock(&root, "c1", "new", false));
    }

    #[test]
    fn migration_hint_trigger() {
        let root = tmp("hint");
        assert!(needs_migration_hint(&root));
        create(&root, &cols(), new("first")).unwrap();
        assert!(dir(&root).join(".migrated").exists());
        assert!(!needs_migration_hint(&root));
        let other = tmp("hint2");
        fs::create_dir_all(dir(&other)).unwrap();
        assert!(!needs_migration_hint(&other));
    }
}
