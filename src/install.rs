//! `peekme install`, `uninstall` and `doctor`: make typing `codex` or `claude`
//! open the agent with peekme, without replacing or patching it.
//!
//! Two layers (see the knowledge base notes on PATH managers):
//! - a shell function per agent (`codex`, `claude`) in the shell's startup
//!   file. Functions win over PATH, so nvm, mise, Homebrew or an agent's own
//!   installer changing PATH later can't bypass them, and the user's aliases
//!   built on them keep working;
//! - a link per agent to peekme in our own directory, put first in PATH, so
//!   scripts that run the agent get it too (best effort: something that edits
//!   PATH later can come before it; `doctor` reports that).
//!
//! A function calls its link by the full path, so it works even when peekme
//! itself is not on PATH, and falls back to the plain agent if the link is gone.
//! Both agents are always set up: one that is installed later just works.
//!
//! `install --shell` adds a second block at the top of the startup file: an
//! interactive shell in a terminal replaces itself with `peekme -- <shell>`
//! (through a `peekme` link in the same directory), so Alt+P works on any
//! text in the terminal from the first line. At the top, so the shell that is
//! replaced has not run the rest of the file, which the new one runs.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};

use crate::agent::Agent;
use crate::launch;

pub const BEGIN: &str = "# >>> peekme >>>";
pub const END: &str = "# <<< peekme <<<";
pub const SHELL_BEGIN: &str = "# >>> peekme shell >>>";
pub const SHELL_END: &str = "# <<< peekme shell <<<";
/// The link the shell block starts, next to the agent links.
const SHELL_LINK: &str = "peekme";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        }
    }
}

/// A startup file we manage.
#[derive(Debug)]
pub struct Target {
    pub shell: Shell,
    pub path: PathBuf,
}

/// `dir` as the shell should write it: under `$HOME` when it is there, so the
/// block keeps working if the home directory moves.
fn shell_path(dir: &Path) -> String {
    match launch::home() {
        Some(h) if dir.starts_with(&h) => {
            format!("$HOME/{}", dir.strip_prefix(&h).unwrap().display())
        }
        _ => dir.display().to_string(),
    }
}

fn header() -> String {
    format!(
        "# Opens {} with peekme when you type {}. Remove with: peekme uninstall",
        Agent::all_names(),
        Agent::all_commands("or")
    )
}

/// The block for bash and zsh startup files.
pub fn posix_block(shim_dir: &Path) -> String {
    let dir = shell_path(shim_dir);
    let header = header();
    let functions: String = Agent::ALL
        .iter()
        .map(|a| {
            let c = a.command();
            format!(
                "function {c} {{\n\
                 \x20 if [ -x \"{dir}/{c}\" ]; then \"{dir}/{c}\" \"$@\"; else command {c} \"$@\"; fi\n\
                 }}\n"
            )
        })
        .collect();
    format!(
        "{BEGIN}\n\
         {header}\n\
         {functions}\
         case \":$PATH:\" in\n\
         \x20 *\":{dir}:\"*) ;;\n\
         \x20 *) export PATH=\"{dir}:$PATH\" ;;\n\
         esac\n\
         {END}\n"
    )
}

const SHELL_HEADER: &str = "# Starts this shell with peekme, so Alt+P works on any text in the terminal. \
     Stop with: peekme uninstall --shell";

/// The block that starts bash or zsh with peekme. Only an interactive shell in
/// a terminal, not over SSH (the selection is on the other machine), not in
/// VS Code (its shell integration comes with the shell it started), and not
/// under a peekme already. A peekme that does not run leaves the shell as it is.
pub fn shell_block(shell: Shell, shim_dir: &Path) -> String {
    let p = format!("{}/{SHELL_LINK}", shell_path(shim_dir));
    let (interactive, login, me) = match shell {
        Shell::Zsh => ("[[ -o interactive ]]", "[[ -o login ]]", "zsh"),
        _ => ("[[ $- == *i* ]]", "shopt -q login_shell", "\"$BASH\""),
    };
    format!(
        "{SHELL_BEGIN}\n\
         {SHELL_HEADER}\n\
         if {interactive} && [ -z \"${{PEEKME_ACTIVE-}}\" ] && [ -z \"${{SSH_CONNECTION-}}\" ] \\\n\
         \x20 && [ \"${{TERM-dumb}}\" != dumb ] && [ \"${{TERM_PROGRAM-}}\" != vscode ] \\\n\
         \x20 && [ -t 0 ] && [ -t 1 ] \\\n\
         \x20 && \"{p}\" --version >/dev/null 2>&1; then\n\
         \x20 if {login}; then exec \"{p}\" -- {me} -l; fi\n\
         \x20 exec \"{p}\" -- {me}\n\
         fi\n\
         {SHELL_END}\n"
    )
}

/// The same for fish, at the top of our conf.d file.
fn fish_shell_block(shim_dir: &Path) -> String {
    let p = format!("{}/{SHELL_LINK}", shell_path(shim_dir));
    format!(
        "{SHELL_BEGIN}\n\
         {SHELL_HEADER}\n\
         if status is-interactive; and not set -q PEEKME_ACTIVE; and not set -q SSH_CONNECTION \\\n\
         \x20   ; and test \"$TERM\" != dumb; and test \"$TERM_PROGRAM\" != vscode \\\n\
         \x20   ; and isatty stdin; and isatty stdout \\\n\
         \x20   ; and \"{p}\" --version >/dev/null 2>&1\n\
         \x20   if status is-login\n\
         \x20       exec \"{p}\" -- fish -l\n\
         \x20   end\n\
         \x20   exec \"{p}\" -- fish\n\
         end\n\
         {SHELL_END}\n"
    )
}

/// The whole file we own in fish's conf.d, with the shell block when `shell`.
pub fn fish_file(shim_dir: &Path, shell: bool) -> String {
    let start = if shell {
        format!("{}\n", fish_shell_block(shim_dir))
    } else {
        String::new()
    };
    format!("{start}{}", fish_agents(shim_dir))
}

fn fish_agents(shim_dir: &Path) -> String {
    let dir = shell_path(shim_dir);
    let header = header();
    let functions: String = Agent::ALL
        .iter()
        .map(|a| {
            let (c, name) = (a.command(), a.name());
            format!(
                "function {c} --description '{name}, opened with peekme'\n\
                 \x20   if test -x \"{dir}/{c}\"\n\
                 \x20       \"{dir}/{c}\" $argv\n\
                 \x20   else\n\
                 \x20       command {c} $argv\n\
                 \x20   end\n\
                 end\n"
            )
        })
        .collect();
    format!(
        "{BEGIN}\n\
         {header}\n\
         {functions}\
         if not contains \"{dir}\" $PATH\n\
         \x20   set -gx PATH \"{dir}\" $PATH\n\
         end\n\
         {END}\n"
    )
}

/// Insert our block, or replace it where it already is.
pub fn upsert_block(text: &str, block: &str) -> String {
    if let Some((start, end)) = block_span(text, BEGIN, END) {
        return format!("{}{}{}", &text[..start], block, &text[end..]);
    }
    let mut out = text.to_string();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(block);
    out
}

/// Remove our block and the blank line we put before it.
pub fn remove_block(text: &str) -> String {
    let Some((mut start, end)) = block_span(text, BEGIN, END) else {
        return text.to_string();
    };
    if text[..start].ends_with("\n\n") {
        start -= 1;
    }
    format!("{}{}", &text[..start], &text[end..])
}

/// Put the shell block first in the file, or replace it where it already is
/// (the user may have moved it).
pub fn upsert_shell_block(text: &str, block: &str) -> String {
    if let Some((start, end)) = block_span(text, SHELL_BEGIN, SHELL_END) {
        return format!("{}{}{}", &text[..start], block, &text[end..]);
    }
    if text.is_empty() {
        return block.to_string();
    }
    format!("{block}\n{text}")
}

/// Remove the shell block and the blank line we put after it.
pub fn remove_shell_block(text: &str) -> String {
    let Some((start, mut end)) = block_span(text, SHELL_BEGIN, SHELL_END) else {
        return text.to_string();
    };
    if start == 0 && text[end..].starts_with('\n') {
        end += 1;
    }
    format!("{}{}", &text[..start], &text[end..])
}

/// Byte range of a block, including its final newline.
fn block_span(text: &str, begin: &str, end_mark: &str) -> Option<(usize, usize)> {
    let start = text.find(begin)?;
    let end_marker = start + text[start..].find(end_mark)?;
    let mut end = end_marker + end_mark.len();
    if text[end..].starts_with('\n') {
        end += 1;
    }
    Some((start, end))
}

/// Startup files for the shells this user has (or uses).
pub fn targets() -> Result<Vec<Target>> {
    let home = launch::home().ok_or_else(|| anyhow!("HOME is not set"))?;
    let user_shell = std::env::var("SHELL").unwrap_or_default();
    let uses = |name: &str| user_shell.rsplit('/').next() == Some(name);
    let mut out = Vec::new();

    let bashrc = if cfg!(target_os = "macos") {
        home.join(".bash_profile")
    } else {
        home.join(".bashrc")
    };
    if bashrc.exists() || uses("bash") {
        out.push(Target {
            shell: Shell::Bash,
            path: bashrc,
        });
    }
    let zdotdir = std::env::var_os("ZDOTDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.clone());
    let zshrc = zdotdir.join(".zshrc");
    if zshrc.exists() || uses("zsh") {
        out.push(Target {
            shell: Shell::Zsh,
            path: zshrc,
        });
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let fish_dir = config.join("fish");
    if fish_dir.exists() || uses("fish") {
        out.push(Target {
            shell: Shell::Fish,
            path: fish_dir.join("conf.d").join("peekme.fish"),
        });
    }
    Ok(out)
}

/// Is our block (or fish file) present anywhere?
pub fn installed() -> bool {
    has_anywhere(BEGIN)
}

/// Does some startup file start the shell with peekme?
pub fn shell_installed() -> bool {
    has_anywhere(SHELL_BEGIN)
}

fn has_anywhere(marker: &str) -> bool {
    targets().is_ok_and(|ts| {
        ts.iter()
            .any(|t| std::fs::read_to_string(&t.path).is_ok_and(|s| s.contains(marker)))
    })
}

/// The startup file with our blocks as this peekme writes them: the agent
/// block, and the shell block where `shell` or where there is one already.
fn with_blocks(t: &Target, old: &str, shim_dir: &Path, shell: bool) -> String {
    let shell = shell || old.contains(SHELL_BEGIN);
    match t.shell {
        Shell::Fish => fish_file(shim_dir, shell),
        _ => {
            let new = upsert_block(old, &posix_block(shim_dir));
            if shell {
                upsert_shell_block(&new, &shell_block(t.shell, shim_dir))
            } else {
                new
            }
        }
    }
}

/// Write `contents` to `path` atomically. A startup file that is a symlink
/// (dotfile managers) is written through, so the link stays a link.
fn write_file(path: &Path, contents: &str) -> Result<()> {
    let real = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let dir = real
        .parent()
        .ok_or_else(|| anyhow!("no parent for {}", real.display()))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".peekme-tmp-{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    if let Ok(meta) = std::fs::metadata(&real) {
        std::fs::set_permissions(&tmp, meta.permissions())?;
    }
    std::fs::rename(&tmp, &real).with_context(|| format!("could not write {}", real.display()))?;
    Ok(())
}

fn tilde(p: &Path) -> String {
    match launch::home() {
        Some(h) if p.starts_with(&h) => format!("~/{}", p.strip_prefix(&h).unwrap().display()),
        _ => p.display().to_string(),
    }
}

/// Create or refresh the `codex` and `claude` links to this binary, and with
/// `shell` the `peekme` link the shell block starts.
fn install_shim(shim_dir: &Path, shell: bool) -> Result<()> {
    let me = std::env::current_exe()?.canonicalize()?;
    std::fs::create_dir_all(shim_dir)?;
    let names = Agent::ALL.iter().map(|a| a.command());
    for name in names.chain(shell.then_some(SHELL_LINK)) {
        let link = shim_dir.join(name);
        if std::fs::symlink_metadata(&link).is_ok() {
            std::fs::remove_file(&link)?;
        }
        std::os::unix::fs::symlink(&me, &link)
            .with_context(|| format!("could not create {}", link.display()))?;
    }
    Ok(())
}

/// Bring an existing setup up to date: a newer peekme may know more agents
/// than the block and links an older `peekme install` wrote (updating the
/// binary through cargo, npm or Homebrew never runs peekme). Only touches a
/// setup the user made: our marked block, our fish file, our links. Returns a
/// line to show when something changed.
pub fn refresh() -> Option<String> {
    if !installed() {
        return None;
    }
    let shim_dir = launch::shim_dir()?;
    let me = std::env::current_exe().ok()?.canonicalize().ok()?;
    let (added, moved) = relink(&shim_dir, &me, shell_installed());
    let mut rewrote = false;
    for t in targets().ok()? {
        let Ok(old) = std::fs::read_to_string(&t.path) else {
            continue;
        };
        if !old.contains(BEGIN) {
            continue;
        }
        let new = with_blocks(&t, &old, &shim_dir, false);
        // The same lines in another order (0.3.3 lists Claude first) are not
        // worth touching the user's file for.
        if !same_lines(&old, &new) && write_file(&t.path, &new).is_ok() {
            rewrote = true;
        }
    }
    let mut notes = Vec::new();
    if !added.is_empty() {
        notes.push(format!(
            "peekme: updated your shell setup, so {} now open with peekme too \
             (in new terminals).",
            added.join(" and ")
        ));
    } else if rewrote {
        notes.push("peekme: updated your shell setup.".into());
    }
    if let Some((old, _)) = moved.first() {
        let names: Vec<String> = moved.iter().map(|(_, a)| a.clone()).collect();
        let (who, starts) = if names.len() == 1 {
            ("It", "starts")
        } else {
            ("They", "start")
        };
        notes.push(format!(
            "peekme: {} started an older peekme ({old}). {who} now {starts} this one ({}).",
            names.join(" and "),
            env!("CARGO_PKG_VERSION")
        ));
    }
    (!notes.is_empty()).then(|| notes.join("\n"))
}

/// Point the agent links at `me` where they are missing or broken, or lead to
/// an older peekme. Two copies of peekme happen (cargo and then npm, say), and
/// `peekme install` from the first one leaves links to it that nothing else
/// updates: `codex` kept running the old copy after the new one was
/// installed. Returns the agents added, and for the moved ones the old
/// version and the agent.
fn relink(shim_dir: &Path, me: &Path, shell: bool) -> (Vec<String>, Vec<(String, String)>) {
    let mine = env!("CARGO_PKG_VERSION");
    let mut added = Vec::new();
    let mut moved = Vec::new();
    let names = Agent::ALL.iter().map(|a| a.command());
    for command in names.chain(shell.then_some(SHELL_LINK)) {
        let link = shim_dir.join(command);
        let name = format!("`{command}`");
        match std::fs::canonicalize(&link) {
            Err(_) => {
                let _ = std::fs::remove_file(&link);
                let _ = std::fs::create_dir_all(shim_dir);
                // The `peekme` link is no news: `peekme` itself is what runs.
                if std::os::unix::fs::symlink(me, &link).is_ok() && command != SHELL_LINK {
                    added.push(name);
                }
            }
            Ok(target) if target != me => {
                let Some(theirs) = peekme_version(&target) else {
                    continue;
                };
                if newer(mine, &theirs)
                    && std::fs::remove_file(&link).is_ok()
                    && std::os::unix::fs::symlink(me, &link).is_ok()
                {
                    moved.push((theirs, name));
                }
            }
            Ok(_) => {}
        }
    }
    (added, moved)
}

/// The version of the peekme binary at `path`; None for anything else.
fn peekme_version(path: &Path) -> Option<String> {
    if path.file_name()? != "peekme" {
        return None;
    }
    let out = std::process::Command::new(path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let v = text.trim().strip_prefix("peekme ")?;
    Some(v.to_string())
}

/// Whether version `a` is newer than `b` (`1.2.3` or `1.2.3-beta.1`; a
/// prerelease is older than its release).
fn newer(a: &str, b: &str) -> bool {
    fn key(v: &str) -> Option<(Vec<u64>, bool, &str)> {
        let (nums, pre) = v.split_once('-').unwrap_or((v, ""));
        let nums = nums
            .split('.')
            .map(|n| n.parse().ok())
            .collect::<Option<Vec<u64>>>()?;
        Some((nums, pre.is_empty(), pre))
    }
    match (key(a), key(b)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Whether two versions of a startup file differ only in the order of their
/// lines and in comments.
fn same_lines(a: &str, b: &str) -> bool {
    fn sorted(s: &str) -> Vec<&str> {
        let mut v: Vec<&str> = s.lines().filter(|l| !l.starts_with('#')).collect();
        v.sort_unstable();
        v
    }
    sorted(a) == sorted(b)
}

/// `peekme install`, and with `shell` also start new terminals' shells with peekme.
pub fn install(shell: bool) -> Result<()> {
    let shim_dir = launch::shim_dir().ok_or_else(|| anyhow!("HOME is not set"))?;
    let shell = shell || shell_installed();
    install_shim(&shim_dir, shell)?;
    let targets = targets()?;
    if targets.is_empty() {
        println!("No bash, zsh or fish setup found. Add this to your shell's startup file:\n");
        if shell {
            println!("{}", shell_block(Shell::Bash, &shim_dir));
        }
        print!("{}", posix_block(&shim_dir));
        return Ok(());
    }
    for t in &targets {
        let old = std::fs::read_to_string(&t.path).unwrap_or_default();
        let new = with_blocks(t, &old, &shim_dir, shell);
        if new != old {
            write_file(&t.path, &new)?;
        }
        println!("Set up {} in {}", t.shell.name(), tilde(&t.path));
    }
    println!();
    if shell {
        println!(
            "New terminals start the shell with peekme: select any text there and press Alt+P."
        );
    }
    for agent in Agent::ALL {
        let (c, name) = (agent.command(), agent.name());
        if launch::find_real(c).is_some() {
            println!("Typing `{c}` now opens {name} with peekme, in new terminals.");
        } else {
            println!("{name} is not installed. Once it is, typing `{c}` opens it with peekme.");
        }
    }
    let current = targets.iter().find(|t| {
        std::env::var("SHELL")
            .unwrap_or_default()
            .rsplit('/')
            .next()
            == Some(t.shell.name())
    });
    if let Some(t) = current {
        println!("In this terminal, run: source {}", tilde(&t.path));
    }
    let plain: Vec<String> = Agent::ALL
        .iter()
        .map(|a| format!("command {}", a.command()))
        .collect();
    println!(
        "To run an agent without peekme once: {}. To undo: peekme uninstall.",
        plain.join(", ")
    );
    Ok(())
}

/// `peekme uninstall`; with `shell` only the shell block goes.
pub fn uninstall(shell: bool) -> Result<()> {
    if shell {
        return uninstall_shell();
    }
    for t in targets()? {
        match t.shell {
            Shell::Fish => {
                if t.path.exists() {
                    std::fs::remove_file(&t.path)?;
                    println!("Removed {}", tilde(&t.path));
                }
            }
            _ => {
                let Ok(old) = std::fs::read_to_string(&t.path) else {
                    continue;
                };
                if old.contains(BEGIN) || old.contains(SHELL_BEGIN) {
                    write_file(&t.path, &remove_shell_block(&remove_block(&old)))?;
                    println!("Removed the peekme block from {}", tilde(&t.path));
                }
            }
        }
    }
    if let Some(dir) = launch::shim_dir() {
        let names = Agent::ALL.iter().map(|a| a.command());
        for name in names.chain([SHELL_LINK]) {
            let link = dir.join(name);
            if std::fs::symlink_metadata(&link).is_ok() {
                std::fs::remove_file(&link)?;
                println!("Removed {}", tilde(&link));
            }
        }
        let _ = std::fs::remove_dir(&dir);
    }
    let names: Vec<&str> = Agent::ALL.iter().map(|a| a.command()).collect();
    println!(
        "Done. New terminals run the agents without peekme. In open ones, run: unset -f {}",
        names.join(" ")
    );
    Ok(())
}

fn uninstall_shell() -> Result<()> {
    let shim_dir = launch::shim_dir().ok_or_else(|| anyhow!("HOME is not set"))?;
    let mut removed = false;
    for t in targets()? {
        let Ok(old) = std::fs::read_to_string(&t.path) else {
            continue;
        };
        if !old.contains(SHELL_BEGIN) {
            continue;
        }
        let new = match t.shell {
            Shell::Fish => fish_file(&shim_dir, false),
            _ => remove_shell_block(&old),
        };
        write_file(&t.path, &new)?;
        println!("Removed the peekme shell start from {}", tilde(&t.path));
        removed = true;
    }
    let link = shim_dir.join(SHELL_LINK);
    if std::fs::symlink_metadata(&link).is_ok() {
        std::fs::remove_file(&link)?;
    }
    if removed {
        println!("Done. New terminals start the shell without peekme.");
    } else {
        println!("No shell start with peekme found.");
    }
    Ok(())
}

/// What `name` means in an interactive shell: its kind, the first `name` on
/// PATH, and whether the function is ours.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Probe {
    kind: String,
    path: String,
    ours: bool,
}

/// Ask an interactive shell what each agent command means there (one shell
/// start for all of them: it can take a second).
fn probe(shell: Shell) -> Option<Vec<Probe>> {
    let one = |c: &str| match shell {
        Shell::Bash => format!(
            "echo \"K_{c}=$(type -t {c})\"; echo \"P_{c}=$(type -P {c})\"; \
             declare -f {c} | grep -q peekme && echo O_{c}=1; "
        ),
        Shell::Zsh => format!(
            "echo \"K_{c}=$(whence -w {c})\"; echo \"P_{c}=$(whence -p {c})\"; \
             functions {c} 2>/dev/null | grep -q peekme && echo O_{c}=1; "
        ),
        Shell::Fish => format!(
            "echo K_{c}=(type -t {c}); echo P_{c}=(command -s {c}); \
             functions {c} 2>/dev/null | string match -q '*peekme*'; and echo O_{c}=1; "
        ),
    };
    let script: String = std::iter::once("echo __peekme__; ".to_string())
        .chain(Agent::ALL.iter().map(|a| one(a.command())))
        .collect();
    let mut child = Command::new(shell.name())
        .args(["-i", "-c", &script])
        .env_remove(launch::ACTIVE_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().ok()?.is_some() {
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // Startup files may print things; only what follows our marker counts.
    let after = text.split("__peekme__").nth(1).unwrap_or_default();
    let field = |key: &str| {
        after
            .lines()
            .find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_string()))
            .unwrap_or_default()
    };
    Some(
        Agent::ALL
            .iter()
            .map(|a| {
                let c = a.command();
                Probe {
                    kind: field(&format!("K_{c}=")),
                    path: field(&format!("P_{c}=")),
                    ours: field(&format!("O_{c}=")) == "1",
                }
            })
            .collect(),
    )
}

/// Check the setup and say what, if anything, is in the way. Returns an exit code.
pub fn doctor() -> i32 {
    let mut ok = true;
    let mut line = |good: bool, msg: String| {
        println!("{} {msg}", if good { "ok  " } else { "FAIL" });
        if !good {
            ok = false;
        }
    };
    let me = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());
    println!(
        "peekme {} at {}",
        env!("CARGO_PKG_VERSION"),
        me.as_deref().map(tilde).unwrap_or_default()
    );
    let found: Vec<bool> = Agent::ALL
        .iter()
        .map(|a| launch::find_real(a.command()).is_some())
        .collect();
    for (agent, found) in Agent::ALL.iter().zip(&found) {
        match launch::find_real(agent.command()) {
            Some(p) if *found => println!("ok   {} found: {}", agent.name(), tilde(&p)),
            _ => println!(
                "note {} not found on PATH (install: {})",
                agent.name(),
                agent.install_hint()
            ),
        }
    }
    if !found.contains(&true) {
        line(
            false,
            format!("none of {} is installed", Agent::all_names()),
        );
    }
    let Some(shim_dir) = launch::shim_dir() else {
        line(false, "HOME is not set".into());
        return 1;
    };
    for agent in Agent::ALL {
        let link = shim_dir.join(agent.command());
        let target = std::fs::canonicalize(&link).ok();
        match (&target, &me) {
            (Some(t), Some(m)) if t == m => {
                line(true, format!("{} points to this peekme", tilde(&link)))
            }
            (Some(t), _) => line(
                false,
                format!(
                    "{} points to {}. Run: peekme install",
                    tilde(&link),
                    tilde(t)
                ),
            ),
            (None, _) => line(
                false,
                format!("{} is missing. Run: peekme install", tilde(&link)),
            ),
        }
    }
    let targets = targets().unwrap_or_default();
    if targets.is_empty() {
        line(false, "no bash, zsh or fish setup found".into());
    }
    for t in &targets {
        let sh = t.shell.name();
        let text = std::fs::read_to_string(&t.path).unwrap_or_default();
        if text.contains(SHELL_BEGIN) {
            let link = shim_dir.join(SHELL_LINK);
            match std::fs::canonicalize(&link) {
                Ok(target) if Some(&target) == me.as_ref() => line(
                    true,
                    format!("{sh}: new terminals start the shell with this peekme"),
                ),
                Ok(target) => line(
                    false,
                    format!(
                        "{sh}: new terminals start the shell with {}. Run: peekme install --shell",
                        tilde(&target)
                    ),
                ),
                Err(_) => line(
                    false,
                    format!(
                        "{sh}: {} is missing, so the shell starts without peekme. \
                         Run: peekme install --shell",
                        tilde(&link)
                    ),
                ),
            }
        }
        let has = text.contains(BEGIN);
        if !has {
            line(
                false,
                format!(
                    "{sh}: no peekme block in {}. Run: peekme install",
                    tilde(&t.path)
                ),
            );
            continue;
        }
        let Some(probes) = probe(t.shell) else {
            println!("     {sh}: could not start an interactive {sh} to check");
            continue;
        };
        for (agent, pr) in Agent::ALL.iter().zip(&probes) {
            let c = agent.command();
            let is_fn = (pr.kind == "function" || pr.kind.ends_with(": function")) && pr.ours;
            if is_fn {
                line(true, format!("{sh}: typing `{c}` opens peekme"));
            } else {
                let what = if pr.kind.contains("function") {
                    "another function"
                } else if pr.kind.contains("alias") {
                    "an alias that does not lead to peekme"
                } else {
                    "not our function"
                };
                line(
                    false,
                    format!(
                        "{sh}: `{c}` is {what}. Something defines {c} after the peekme block in {}, \
                         or later in your shell setup. Or the block is from an older peekme: run peekme install",
                        tilde(&t.path)
                    ),
                );
            }
            let first_is_ours = Path::new(&pr.path)
                .parent()
                .and_then(|p| p.canonicalize().ok())
                == shim_dir.canonicalize().ok();
            if first_is_ours {
                println!("ok   {sh}: scripts that run `{c}` get peekme too");
            } else {
                // Not a failure: typing it still works, only scripts miss out.
                println!(
                    "note {sh}: scripts that run `{c}` get it without peekme, because {} comes \
                     first in PATH (something adds it after the peekme block)",
                    if pr.path.is_empty() {
                        "nothing".to_string()
                    } else {
                        tilde(Path::new(&pr.path))
                    }
                );
            }
        }
    }
    if cfg!(target_os = "macos") && std::env::var_os("SSH_CONNECTION").is_none() {
        accessibility_note();
    }
    if ok { 0 } else { 1 }
}

/// On a Mac: can peekme read the terminal's own selection? Run with text
/// selected in the terminal to see what it would get.
fn accessibility_note() {
    let p = crate::macax::probe();
    if !p.trusted {
        println!(
            "note Accessibility is off for this terminal app: in Terminal, text selected while \
             the agent leaves the mouse to the terminal (Kiro inline, Claude classic) can't be \
             peeked. Option+P offers to turn it on."
        );
        return;
    }
    match (&p.text, &p.error) {
        (Some(t), _) if !t.trim().is_empty() => println!(
            "ok   Accessibility on, the terminal's selection reads ({} chars, {})",
            t.chars().count(),
            p.role.as_deref().unwrap_or("no role")
        ),
        (_, Some(e)) => println!(
            "note Accessibility on, but the selection could not be read ({e}, {})",
            p.role.as_deref().unwrap_or("no role")
        ),
        _ => println!(
            "ok   Accessibility on (select text before `peekme doctor` to check reading it)"
        ),
    }
}

/// Ask once, on the first plain `peekme` run, whether to set things up.
pub fn first_run_question() {
    let Some(marker) =
        std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state/peekme/asked-install"))
    else {
        return;
    };
    if marker.exists() || installed() {
        return;
    }
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&marker, b"");
    print!(
        "Open {} with peekme every time you type {}? This adds a few lines to your shell setup. [Y/n] ",
        Agent::all_names(),
        Agent::all_commands("or")
    );
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    let answer = answer.trim().to_lowercase();
    if answer.is_empty() || answer == "y" || answer == "yes" {
        if let Err(e) = install(false) {
            eprintln!("peekme: {e:#}");
        }
        println!();
    } else {
        println!("Okay. Run `peekme install` any time.\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_round_trip_leaves_the_file_as_it_was() {
        let dir = Path::new("/home/u/.local/share/peekme/bin");
        for original in [
            "",
            "alias ll='ls -l'\n",
            "export A=1\n\n",
            "no newline at end",
        ] {
            let with = upsert_block(original, &posix_block(dir));
            assert!(with.contains(BEGIN) && with.ends_with(&format!("{END}\n")));
            // Installing twice changes nothing.
            assert_eq!(upsert_block(&with, &posix_block(dir)), with);
            let back = remove_block(&with);
            if original.ends_with('\n') || original.is_empty() {
                assert_eq!(back, original);
            } else {
                assert_eq!(back, format!("{original}\n"));
            }
        }
    }

    #[test]
    fn reordered_block_is_left_alone() {
        let dir = Path::new("/home/u/.local/share/peekme/bin");
        let new = format!("export A=1\n{}", posix_block(dir));
        // What 0.3.2 wrote: Codex first, in the header and in the functions.
        let (head, rest) = new.split_once("function claude").unwrap();
        let (claude, rest) = rest.split_once("function codex").unwrap();
        let (codex, rest) = rest.split_once("function copilot").unwrap();
        let old =
            format!("{head}function codex{codex}function claude{claude}function copilot{rest}")
                .replace(
                    &header(),
                    "# Opens Codex, Claude Code and GitHub Copilot CLI with peekme",
                );
        assert_ne!(old, new);
        assert!(same_lines(&old, &new));
        // A block without copilot still gets updated.
        let (without, _) = old.split_once("function copilot").unwrap();
        let without = format!("{without}{}", &rest[rest.find("case").unwrap()..]);
        assert!(!same_lines(&without, &new));
    }

    #[test]
    fn versions_compare() {
        assert!(newer("0.3.4", "0.3.1"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("0.4.0-beta.1", "0.3.3"));
        assert!(newer("0.4.0", "0.4.0-beta.1"));
        assert!(!newer("0.3.3", "0.3.3"));
        assert!(!newer("0.3.1", "0.3.4"));
        assert!(!newer("0.3.4", "junk"));
    }

    #[test]
    fn links_to_an_older_peekme_are_moved_to_this_one() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = std::env::temp_dir().join(format!("peekme-relink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = |sub: &str, version: &str| {
            let d = dir.join(sub);
            std::fs::create_dir_all(&d).unwrap();
            let p = d.join("peekme");
            std::fs::write(&p, format!("#!/bin/sh\necho 'peekme {version}'\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let old = fake("cargo", "0.1.0");
        let future = fake("future", "99.0.0");
        let other = dir.join("other").join("codex-wrapper");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, "").unwrap();
        let me = fake("npm", env!("CARGO_PKG_VERSION"));
        let shims = dir.join("bin");
        std::fs::create_dir_all(&shims).unwrap();
        symlink(&old, shims.join("codex")).unwrap();
        symlink(&future, shims.join("claude")).unwrap();
        symlink(dir.join("gone"), shims.join("copilot")).unwrap();

        let (added, moved) = relink(&shims, &me, false);
        assert_eq!(added, vec!["`copilot`", "`kiro-cli`"]);
        assert_eq!(moved, vec![("0.1.0".to_string(), "`codex`".to_string())]);
        let target = |a: &str| std::fs::canonicalize(shims.join(a)).unwrap();
        assert_eq!(target("codex"), me.canonicalize().unwrap());
        assert_eq!(target("copilot"), me.canonicalize().unwrap());
        // A newer peekme and anything that is not peekme stay as they are.
        assert_eq!(target("claude"), future.canonicalize().unwrap());
        std::fs::remove_file(shims.join("codex")).unwrap();
        symlink(&other, shims.join("codex")).unwrap();
        assert_eq!(relink(&shims, &me, false), (vec![], vec![]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_shell_link_is_made_quietly() {
        let dir = std::env::temp_dir().join(format!("peekme-shell-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let me = std::env::current_exe().unwrap().canonicalize().unwrap();
        let (added, _) = relink(&dir, &me, true);
        assert_eq!(added.len(), Agent::ALL.len());
        assert!(!added.contains(&"`peekme`".to_string()));
        assert_eq!(std::fs::canonicalize(dir.join(SHELL_LINK)).unwrap(), me);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_block_goes_first_and_comes_out_again() {
        let dir = Path::new("/home/u/.local/share/peekme/bin");
        for original in ["", "# ~/.bashrc\nalias ll='ls -l'\n", "no newline at end"] {
            let agents = upsert_block(original, &posix_block(dir));
            let with = upsert_shell_block(&agents, &shell_block(Shell::Bash, dir));
            assert!(with.starts_with(SHELL_BEGIN), "{with}");
            assert!(with.contains(BEGIN));
            assert_eq!(
                upsert_shell_block(&with, &shell_block(Shell::Bash, dir)),
                with
            );
            assert_eq!(remove_shell_block(&with), agents);
            // A full uninstall removes both.
            assert_eq!(
                remove_shell_block(&remove_block(&with)),
                remove_block(&agents)
            );
        }
        // Moved down by the user: replaced where it is.
        let moved = format!("export A=1\n{SHELL_BEGIN}\nold\n{SHELL_END}\n");
        let new = upsert_shell_block(&moved, &shell_block(Shell::Zsh, dir));
        assert!(new.starts_with("export A=1\n# >>> peekme shell >>>"));
    }

    #[test]
    fn shell_blocks_parse() {
        let dir = Path::new("/home/u/.local/share/peekme/bin");
        let check = |shell: &str, text: String| {
            let Ok(out) = Command::new(shell)
                .args(["-n", "-c", &text])
                .stderr(Stdio::piped())
                .output()
            else {
                return; // not installed here
            };
            assert!(
                out.status.success(),
                "{shell}: {}\n{text}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        check("bash", shell_block(Shell::Bash, dir));
        check("zsh", shell_block(Shell::Zsh, dir));
        check("fish", fish_file(dir, true));
    }

    #[test]
    fn upsert_replaces_in_place() {
        let text = format!("a\n\n{BEGIN}\nold\n{END}\nb\n");
        let new = upsert_block(&text, &format!("{BEGIN}\nnew\n{END}\n"));
        assert_eq!(new, format!("a\n\n{BEGIN}\nnew\n{END}\nb\n"));
    }
}
