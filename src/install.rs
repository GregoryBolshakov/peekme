//! `peekme install`, `uninstall` and `doctor`: make typing `codex` open Codex
//! with peekme, without replacing or patching Codex.
//!
//! Two layers (see the knowledge base notes on PATH managers):
//! - a shell function `codex` in the shell's startup file. Functions win over
//!   PATH, so nvm, mise, Homebrew or Codex's own installer changing PATH later
//!   can't bypass it, and the user's aliases built on `codex` keep working;
//! - a `codex` link to peekme in our own directory, put first in PATH, so
//!   scripts that run `codex` get it too (best effort: something that edits
//!   PATH later can come before it; `doctor` reports that).
//!
//! The function calls the link by its full path, so it works even when peekme
//! itself is not on PATH, and falls back to plain `codex` if the link is gone.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};

use crate::launch;

pub const BEGIN: &str = "# >>> peekme >>>";
pub const END: &str = "# <<< peekme <<<";

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

/// The block for bash and zsh startup files.
pub fn posix_block(shim_dir: &Path) -> String {
    let dir = shell_path(shim_dir);
    format!(
        "{BEGIN}\n\
         # Opens Codex with peekme when you type `codex`. Remove with: peekme uninstall\n\
         function codex {{\n\
         \x20 if [ -x \"{dir}/codex\" ]; then \"{dir}/codex\" \"$@\"; else command codex \"$@\"; fi\n\
         }}\n\
         case \":$PATH:\" in\n\
         \x20 *\":{dir}:\"*) ;;\n\
         \x20 *) export PATH=\"{dir}:$PATH\" ;;\n\
         esac\n\
         {END}\n"
    )
}

/// The whole file we own in fish's conf.d.
pub fn fish_file(shim_dir: &Path) -> String {
    let dir = shell_path(shim_dir);
    format!(
        "{BEGIN}\n\
         # Opens Codex with peekme when you type `codex`. Remove with: peekme uninstall\n\
         function codex --description 'Codex, opened with peekme'\n\
         \x20   if test -x \"{dir}/codex\"\n\
         \x20       \"{dir}/codex\" $argv\n\
         \x20   else\n\
         \x20       command codex $argv\n\
         \x20   end\n\
         end\n\
         if not contains \"{dir}\" $PATH\n\
         \x20   set -gx PATH \"{dir}\" $PATH\n\
         end\n\
         {END}\n"
    )
}

/// Insert our block, or replace it where it already is.
pub fn upsert_block(text: &str, block: &str) -> String {
    if let Some((start, end)) = block_span(text) {
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
    let Some((mut start, end)) = block_span(text) else {
        return text.to_string();
    };
    if text[..start].ends_with("\n\n") {
        start -= 1;
    }
    format!("{}{}", &text[..start], &text[end..])
}

/// Byte range of the block, including its final newline.
fn block_span(text: &str) -> Option<(usize, usize)> {
    let start = text.find(BEGIN)?;
    let end_marker = start + text[start..].find(END)?;
    let mut end = end_marker + END.len();
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
    targets().is_ok_and(|ts| {
        ts.iter()
            .any(|t| std::fs::read_to_string(&t.path).is_ok_and(|s| s.contains(BEGIN)))
    })
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

/// Create or refresh the `codex` link to this binary.
fn install_shim(shim_dir: &Path) -> Result<()> {
    let me = std::env::current_exe()?.canonicalize()?;
    std::fs::create_dir_all(shim_dir)?;
    let link = shim_dir.join("codex");
    if std::fs::symlink_metadata(&link).is_ok() {
        std::fs::remove_file(&link)?;
    }
    std::os::unix::fs::symlink(&me, &link)
        .with_context(|| format!("could not create {}", link.display()))?;
    Ok(())
}

pub fn install() -> Result<()> {
    let shim_dir = launch::shim_dir().ok_or_else(|| anyhow!("HOME is not set"))?;
    install_shim(&shim_dir)?;
    let targets = targets()?;
    if targets.is_empty() {
        println!("No bash, zsh or fish setup found. Add this to your shell's startup file:\n");
        print!("{}", posix_block(&shim_dir));
        return Ok(());
    }
    for t in &targets {
        match t.shell {
            Shell::Fish => write_file(&t.path, &fish_file(&shim_dir))?,
            _ => {
                let old = std::fs::read_to_string(&t.path).unwrap_or_default();
                let new = upsert_block(&old, &posix_block(&shim_dir));
                if new != old {
                    write_file(&t.path, &new)?;
                }
            }
        }
        println!("Set up {} in {}", t.shell.name(), tilde(&t.path));
    }
    println!("\nTyping `codex` now opens Codex with peekme, in new terminals.");
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
    println!("To run plain Codex once: command codex. To undo: peekme uninstall.");
    Ok(())
}

pub fn uninstall() -> Result<()> {
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
                if old.contains(BEGIN) {
                    write_file(&t.path, &remove_block(&old))?;
                    println!("Removed the peekme block from {}", tilde(&t.path));
                }
            }
        }
    }
    if let Some(dir) = launch::shim_dir() {
        let link = dir.join("codex");
        if std::fs::symlink_metadata(&link).is_ok() {
            std::fs::remove_file(&link)?;
            let _ = std::fs::remove_dir(&dir);
            println!("Removed {}", tilde(&link));
        }
    }
    println!("Done. New terminals run plain Codex. In open ones, run: unset -f codex");
    Ok(())
}

/// Ask an interactive shell what `codex` means there.
/// What an interactive shell says about `codex`: its kind, the first `codex`
/// on PATH, and whether the function is ours.
fn probe(shell: Shell) -> Option<(String, String, bool)> {
    let (bin, script) = match shell {
        Shell::Bash => (
            "bash",
            "echo __peekme__; echo \"K=$(type -t codex)\"; echo \"P=$(type -P codex)\"; \
             declare -f codex | grep -q peekme && echo O=1",
        ),
        Shell::Zsh => (
            "zsh",
            "echo __peekme__; echo \"K=$(whence -w codex)\"; echo \"P=$(whence -p codex)\"; \
             functions codex 2>/dev/null | grep -q peekme && echo O=1",
        ),
        Shell::Fish => (
            "fish",
            "echo __peekme__; echo K=(type -t codex); echo P=(command -s codex); \
             functions codex 2>/dev/null | string match -q '*peekme*'; and echo O=1",
        ),
    };
    let mut child = Command::new(bin)
        .args(["-i", "-c", script])
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
    Some((field("K="), field("P="), field("O=") == "1"))
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
    match launch::find_real_codex() {
        Some(p) => line(true, format!("Codex found: {}", tilde(&p))),
        None => line(
            false,
            "Codex not found on PATH. Install it first: npm i -g @openai/codex".into(),
        ),
    }
    let Some(shim_dir) = launch::shim_dir() else {
        line(false, "HOME is not set".into());
        return 1;
    };
    let link = shim_dir.join("codex");
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
    let targets = targets().unwrap_or_default();
    if targets.is_empty() {
        line(false, "no bash, zsh or fish setup found".into());
    }
    for t in &targets {
        let has = std::fs::read_to_string(&t.path).is_ok_and(|s| s.contains(BEGIN));
        if !has {
            line(
                false,
                format!(
                    "{}: no peekme block in {}. Run: peekme install",
                    t.shell.name(),
                    tilde(&t.path)
                ),
            );
            continue;
        }
        let Some((kind, path, ours)) = probe(t.shell) else {
            println!(
                "     {}: could not start an interactive {} to check",
                t.shell.name(),
                t.shell.name()
            );
            continue;
        };
        let is_fn = (kind == "function" || kind.ends_with(": function")) && ours;
        line(
            is_fn,
            if is_fn {
                format!("{}: typing `codex` opens peekme", t.shell.name())
            } else {
                let what = if kind.contains("function") {
                    "another function"
                } else if kind.contains("alias") {
                    "an alias that does not lead to peekme"
                } else {
                    "not our function"
                };
                format!(
                    "{}: `codex` is {what}. Something defines codex after the peekme block in {}, \
                     or later in your shell setup",
                    t.shell.name(),
                    tilde(&t.path)
                )
            },
        );
        let first_is_ours = Path::new(&path)
            .parent()
            .and_then(|p| p.canonicalize().ok())
            == shim_dir.canonicalize().ok();
        if first_is_ours {
            println!(
                "ok   {}: scripts that run `codex` get peekme too",
                t.shell.name()
            );
        } else {
            // Not a failure: typing codex still works, only scripts miss out.
            println!(
                "note {}: scripts that run `codex` get plain Codex, because {} comes first in PATH \
                 (something adds it after the peekme block)",
                t.shell.name(),
                if path.is_empty() {
                    "nothing".to_string()
                } else {
                    tilde(Path::new(&path))
                }
            );
        }
    }
    if ok { 0 } else { 1 }
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
        "Open Codex with peekme every time you type `codex`? This adds a few lines to your shell setup. [Y/n] "
    );
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    let answer = answer.trim().to_lowercase();
    if answer.is_empty() || answer == "y" || answer == "yes" {
        if let Err(e) = install() {
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
    fn upsert_replaces_in_place() {
        let text = format!("a\n\n{BEGIN}\nold\n{END}\nb\n");
        let new = upsert_block(&text, &format!("{BEGIN}\nnew\n{END}\n"));
        assert_eq!(new, format!("a\n\n{BEGIN}\nnew\n{END}\nb\n"));
    }
}
