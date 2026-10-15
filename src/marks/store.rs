//! Saved answers on disk, one file for each agent and folder, so the marks
//! and their answers are back when the chat is opened again (`claude
//! --resume`, `codex resume` and the like) in a new peekme.
//!
//! A mark read back has no place yet: the rows are new. It shows where its
//! words stand amid the same text as before, and until it has shown, its
//! words alone do not open its answer (the same words in another chat may
//! mean something else). Several peekmes in one folder each keep what the
//! others saved meanwhile.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Answer, AnswerId, Child, MAX, MAX_ANSWERS, Mark, Marks, Place};

const VERSION: u32 = 1;

/// Where one agent's answers in one folder are kept.
pub struct Store {
    path: PathBuf,
    agent: String,
    cwd: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Disk {
    version: u32,
    agent: String,
    cwd: String,
    /// Oldest first; parents before their children.
    answers: Vec<DiskAnswer>,
    marks: Vec<DiskMark>,
}

#[derive(Serialize, Deserialize, Clone)]
struct DiskAnswer {
    key: u64,
    #[serde(default)]
    parent: Option<u64>,
    text: String,
    answer: String,
    #[serde(default)]
    question: Option<String>,
    model: String,
    saved: u64,
    #[serde(default)]
    children: Vec<DiskChild>,
}

#[derive(Serialize, Deserialize, Clone)]
struct DiskChild {
    key: u64,
    ranges: Vec<(usize, usize)>,
}

#[derive(Serialize, Deserialize, Clone)]
struct DiskMark {
    answer: u64,
    alt: bool,
    before: String,
    after: String,
}

/// A key no other answer has.
pub(super) fn new_key() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNT.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    h.write_u32(std::process::id());
    h.finish()
}

pub(super) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// FNV-1a: the same file name for a folder on every run and every build.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

impl Store {
    /// The answers of `agent` (its command's name) in folder `cwd`, under
    /// `PEEKME_ANSWERS_DIR` or peekme's state folder. None for the canned
    /// answers of the end-to-end tests, unless they name a folder.
    pub fn for_session(agent: &str, cwd: &str) -> Option<Store> {
        let dir = match std::env::var_os("PEEKME_ANSWERS_DIR") {
            Some(d) => PathBuf::from(d),
            None if std::env::var_os("PEEKME_FAKE_EXPLAINER").is_some() => return None,
            None => crate::state_dir().join("answers"),
        };
        Some(Store::at(&dir, agent, cwd))
    }

    pub fn at(dir: &Path, agent: &str, cwd: &str) -> Store {
        let name: String = agent
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        Store {
            path: dir.join(format!("{name}-{:016x}.json", fnv(cwd))),
            agent: agent.into(),
            cwd: cwd.into(),
        }
    }

    fn read(&self) -> Option<Disk> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        let disk: Disk = serde_json::from_str(&text).ok()?;
        (disk.version == VERSION && disk.agent == self.agent && disk.cwd == self.cwd)
            .then_some(disk)
    }

    /// The answers saved here before, their marks waiting to show.
    pub fn load(&self) -> Marks {
        self.read().map(restore).unwrap_or_default()
    }

    /// Write `marks` down, with the trees another peekme here saved that
    /// this one never had.
    pub fn save(&self, marks: &Marks) -> std::io::Result<()> {
        let (mut answers, mut list) = export(marks);
        if let Some(disk) = self.read() {
            let roots = roots(&disk.answers);
            let theirs: HashSet<u64> = disk
                .answers
                .iter()
                .filter(|a| a.parent.is_none() && !marks.known.contains(&a.key))
                .map(|a| a.key)
                .collect();
            let mine = std::mem::take(&mut answers);
            answers = disk
                .answers
                .into_iter()
                .filter(|a| roots.get(&a.key).is_some_and(|r| theirs.contains(r)))
                .chain(mine)
                .collect();
            let mine = std::mem::take(&mut list);
            list = disk
                .marks
                .into_iter()
                .filter(|m| theirs.contains(&m.answer))
                .chain(mine)
                .collect();
        }
        // Oldest tree first, as asked; the oldest go past the limit.
        let roots = roots(&answers);
        let asked: HashMap<u64, u64> = answers
            .iter()
            .filter(|a| a.parent.is_none())
            .map(|a| (a.key, a.saved))
            .collect();
        let tree_age = |a: &DiskAnswer| roots.get(&a.key).and_then(|r| asked.get(r)).copied();
        answers.retain(|a| tree_age(a).is_some());
        answers.sort_by_key(|a| tree_age(a));
        while answers.len() > MAX_ANSWERS {
            let Some(oldest) = roots.get(&answers[0].key).copied() else {
                break;
            };
            answers.retain(|a| roots.get(&a.key) != Some(&oldest));
        }
        let tops: HashSet<u64> = answers
            .iter()
            .filter(|a| a.parent.is_none())
            .map(|a| a.key)
            .collect();
        list.retain(|m| tops.contains(&m.answer));
        if list.len() > MAX {
            list.drain(..list.len() - MAX);
        }
        let disk = Disk {
            version: VERSION,
            agent: self.agent.clone(),
            cwd: self.cwd.clone(),
            answers,
            marks: list,
        };
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = self
            .path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        {
            let mut f = open_private(&tmp)?;
            f.write_all(serde_json::to_string(&disk)?.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)
    }
}

/// A new file only its owner reads: answers quote the chat.
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)
}

/// Each answer's top answer, by key.
fn roots(answers: &[DiskAnswer]) -> HashMap<u64, u64> {
    let parent: HashMap<u64, Option<u64>> = answers.iter().map(|a| (a.key, a.parent)).collect();
    let mut out = HashMap::new();
    for a in answers {
        let mut k = a.key;
        // A parent missing, or a loop: no root.
        for _ in 0..=answers.len() {
            match parent.get(&k) {
                Some(None) => {
                    out.insert(a.key, k);
                    break;
                }
                Some(Some(p)) => k = *p,
                None => break,
            }
        }
    }
    out
}

fn export(marks: &Marks) -> (Vec<DiskAnswer>, Vec<DiskMark>) {
    let key = |id: &AnswerId| marks.answers.get(id).map(|a| a.key);
    let answers = marks
        .answers
        .values()
        .map(|a| DiskAnswer {
            key: a.key,
            parent: a.parent.as_ref().and_then(key),
            text: a.text.clone(),
            answer: a.answer.clone(),
            question: a.question.clone(),
            model: a.model.clone(),
            saved: a.saved,
            children: a
                .children
                .iter()
                .filter_map(|c| {
                    Some(DiskChild {
                        key: key(&c.id)?,
                        ranges: c.ranges.iter().map(|r| (r.start, r.end)).collect(),
                    })
                })
                .collect(),
        })
        .collect();
    let list = marks
        .list
        .iter()
        .filter_map(|m| {
            Some(DiskMark {
                answer: key(&m.answer)?,
                alt: m.place.alt,
                before: m.place.before.iter().collect(),
                after: m.place.after.iter().collect(),
            })
        })
        .collect();
    (answers, list)
}

fn restore(disk: Disk) -> Marks {
    let mut marks = Marks::default();
    let mut ids: HashMap<u64, AnswerId> = HashMap::new();
    for a in &disk.answers {
        // A parent comes first; an answer without one is dropped.
        let parent = match a.parent {
            None => None,
            Some(p) => match ids.get(&p) {
                Some(&id) => Some(id),
                None => continue,
            },
        };
        if ids.contains_key(&a.key) {
            continue;
        }
        marks.next += 1;
        let id = marks.next;
        ids.insert(a.key, id);
        if parent.is_none() {
            marks.asleep.insert(id);
            marks.known.insert(a.key);
        }
        marks.answers.insert(
            id,
            Answer {
                text: a.text.clone(),
                answer: a.answer.clone(),
                question: a.question.clone(),
                model: a.model.clone(),
                parent,
                children: Vec::new(),
                key: a.key,
                saved: a.saved,
            },
        );
    }
    for a in &disk.answers {
        let Some(id) = ids.get(&a.key) else {
            continue;
        };
        let len = marks.answers[id].answer.chars().count();
        let children: Vec<Child> = a
            .children
            .iter()
            .filter_map(|c| {
                let child = *ids.get(&c.key)?;
                (marks.answers[&child].parent == Some(*id)).then(|| Child {
                    id: child,
                    ranges: c
                        .ranges
                        .iter()
                        .filter(|(s, e)| s < e && *e <= len)
                        .map(|&(s, e)| s..e)
                        .collect(),
                })
            })
            .collect();
        if let Some(answer) = marks.answers.get_mut(id) {
            answer.children = children;
        }
    }
    for m in &disk.marks {
        let Some(&id) = ids.get(&m.answer) else {
            continue;
        };
        if marks.answers[&id].parent.is_some() {
            continue;
        }
        marks.list.push(Mark {
            answer: id,
            place: Place {
                alt: m.alt,
                anchor: (usize::MAX, usize::MAX),
                before: m.before.chars().collect(),
                after: m.after.chars().collect(),
                rows: Vec::new(),
                restored: true,
            },
            drawn: None,
        });
    }
    if marks.list.len() > MAX {
        marks.list.drain(..marks.list.len() - MAX);
    }
    marks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shadow::Shadow;

    fn screen(lines: &[&str], alt: bool) -> Shadow {
        let mut s = Shadow::new(60, 8);
        let mut b = Vec::new();
        if alt {
            b.extend(b"\x1b[?1049h");
        }
        for (i, l) in lines.iter().enumerate() {
            b.extend(format!("\x1b[{};1H{l}", i + 1).bytes());
        }
        s.advance(&b);
        s
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("peekme-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    const CHAT: [&str; 4] = [
        "> what runs the tests",
        "- the pseudo-terminal layer: tmux gives each pane one",
        "- and the job control helper stops the agent",
        "> next question",
    ];

    /// Peeked in one run, the chat opened again in the next (other rows,
    /// other width): the mark is back, a click on it opens the answer with
    /// its nested one, and the words alone open it once it has shown.
    #[test]
    fn answers_are_back_when_the_chat_is_opened_again() {
        let d = dir("back");
        let store = Store::at(&d, "kiro-cli", "/work/app");
        let mut marks = store.load();
        assert!(marks.is_empty());
        let s = screen(&CHAT, true);
        let (place, _) = Place::new(&s.snapshot(), "pseudo-terminal", (1, 6), 0, true).unwrap();
        let top = marks.save(None, "pseudo-terminal", "a fake terminal: the pty", "m");
        marks.set_question(top, Some("why?".into()));
        marks.add(top, place);
        let child = marks.save(Some(top), "pty", "the device", "m");
        marks.add_range(child, 21..24);
        assert!(marks.take_dirty());
        store.save(&marks).unwrap();
        assert!(!marks.take_dirty());

        let mut again = store.load();
        // Not on screen yet: the words alone do not open it.
        assert_eq!(again.find("pseudo-terminal"), None);
        let moved = ["", "", CHAT[0], CHAT[1], CHAT[2], CHAT[3]];
        let s = screen(&moved, true);
        let v = again.visible(&s.snapshot(), 0, true);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].cells[0], (3, 6));
        let id = again.find("pseudo-terminal").expect("on screen now");
        let a = again.answer(id).unwrap();
        assert_eq!(
            (a.answer.as_str(), a.question.as_deref()),
            ("a fake terminal: the pty", Some("why?"))
        );
        let (range, c) = again.child_ranges(id).pop().unwrap();
        assert_eq!(range, 21..24);
        assert_eq!(again.answer(c).unwrap().answer, "the device");
        assert_eq!(again.find_child(id, "pty"), Some(c));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The same words amid other text (another chat) are not the mark; nor
    /// is a copy on the other screen.
    #[test]
    fn a_mark_read_back_needs_the_text_around_it() {
        let d = dir("around");
        let store = Store::at(&d, "claude", "/work/app");
        let mut marks = Marks::default();
        let s = screen(&CHAT, false);
        let (place, _) = Place::new(
            &s.snapshot(),
            "pseudo-terminal",
            (1, 6),
            s.history_size(),
            false,
        )
        .unwrap();
        let id = marks.save(None, "pseudo-terminal", "x", "m");
        marks.add(id, place);
        store.save(&marks).unwrap();

        let mut again = store.load();
        let other = screen(
            &["- a pseudo-terminal of its own, said nobody", "", ""],
            false,
        );
        assert!(again.visible(&other.snapshot(), 0, false).is_empty());
        let s = screen(&CHAT, true);
        assert!(again.visible(&s.snapshot(), 0, true).is_empty());
        // The normal screen, scrolled: the mark is found at its new row.
        let s = screen(&["", CHAT[0], CHAT[1], CHAT[2]], false);
        assert_eq!(again.visible(&s.snapshot(), 0, false)[0].cells[0], (2, 6));
        // Another agent, or another folder, has its own answers.
        assert!(Store::at(&d, "codex", "/work/app").load().is_empty());
        assert!(Store::at(&d, "claude", "/work/other").load().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Two peekmes in one folder: each keeps what the other saved.
    #[test]
    fn two_runs_in_one_folder_keep_each_others_answers() {
        let d = dir("two");
        let store = Store::at(&d, "codex", "/work/app");
        let mut one = store.load();
        let mut two = store.load();
        one.save(None, "alpha", "first", "m");
        store.save(&one).unwrap();
        two.save(None, "bravo", "second", "m");
        store.save(&two).unwrap();
        one.save(None, "charlie", "third", "m");
        store.save(&one).unwrap();
        let mut all = store.load();
        let mut texts: Vec<_> = all.answers.values().map(|a| a.text.clone()).collect();
        texts.sort();
        assert_eq!(texts, ["alpha", "bravo", "charlie"]);
        // The oldest tree goes first past the limit, and stays gone though
        // the file still had it.
        for i in 0..MAX_ANSWERS {
            all.save(None, &format!("w{i}"), "x", "m");
        }
        store.save(&all).unwrap();
        let last = store.load();
        assert_eq!(last.answers.len(), MAX_ANSWERS);
        assert!(!last.answers.values().any(|a| a.text == "alpha"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_broken_file_is_no_answers() {
        let d = dir("broken");
        let store = Store::at(&d, "copilot", "/work/app");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(&store.path, "{not json").unwrap();
        assert!(store.load().is_empty());
        let mut marks = store.load();
        marks.save(None, "alpha", "first", "m");
        store.save(&marks).unwrap();
        assert_eq!(store.load().answers.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
