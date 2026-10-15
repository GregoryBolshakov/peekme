//! How far a turn of the wheel moves Kiro CLI's full-screen chat (2.28.0,
//! its transcript scroll hook): `chat.fullscreenWheelRows` rows (1, 2 or 3,
//! default 2), up to either end, where a turn draws nothing. PageUp and
//! PageDown move the chat's rows less 2.

use std::path::Path;

/// Kiro's setting of rows a turn: the workspace's `.kiro/settings/cli.json`
/// over the user's (`$KIRO_HOME` or `~/.kiro`), else 2.
pub fn wheel_rows(cwd: &Path) -> isize {
    let read = |p: &Path| -> Option<isize> {
        let text = std::fs::read_to_string(p).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        v.get("chat.fullscreenWheelRows")?
            .as_i64()
            .filter(|r| (1..=3).contains(r))
            .map(|r| r as isize)
    };
    let user = super::transcript::config_dir().map(|d| d.join("settings/cli.json"));
    read(&cwd.join(".kiro/settings/cli.json"))
        .or_else(|| user.as_deref().and_then(read))
        .unwrap_or(2)
}

/// Rows a PageUp or PageDown moves the chat that shows `rows` rows.
pub fn page_rows(rows: usize) -> isize {
    rows.saturating_sub(2).max(1) as isize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_workspace_setting_wins() {
        let dir = std::env::temp_dir().join(format!("peekme-kiro-wheel-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".kiro/settings")).unwrap();
        std::fs::write(
            dir.join(".kiro/settings/cli.json"),
            r#"{"chat.fullscreenWheelRows": 3}"#,
        )
        .unwrap();
        assert_eq!(wheel_rows(&dir), 3);
        std::fs::write(
            dir.join(".kiro/settings/cli.json"),
            r#"{"chat.fullscreenWheelRows": 9}"#,
        )
        .unwrap();
        assert!(
            (1..=3).contains(&wheel_rows(&dir)),
            "out of range: the user's or 2"
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(page_rows(32), 30);
    }
}
