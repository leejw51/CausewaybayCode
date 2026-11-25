//! Settings made while the backend runs, kept in `~/.cwbcode/settings.jsonl`: one JSON
//! object per line, appended, the last line for a setting being the one in force.
//!
//!   {"time":"2026-11-25 23:40:12","set":"wallet","value":"0x..."}    remote mode on
//!   {"time":"2026-11-26 09:02:51","set":"wallet","value":""}         remote mode off
//!
//! The file is the history of the setting as well as its value.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Value, json};

pub const FILE: &str = "settings.jsonl";

fn now() -> String {
    // SAFETY: time and localtime_r only write the values passed to them.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

/// The value in force for a setting: its last line. A line that is not JSON is skipped.
pub fn get(home: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(home.join(FILE)).ok()?;
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["set"] == name)
        .filter_map(|entry| entry["value"].as_str().map(String::from))
        .last()
}

/// Record a new value for a setting.
pub fn set(home: &Path, name: &str, value: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = home.join(FILE);
    let mut file = std::fs::OpenOptions::new().append(true).create(true).mode(0o600).open(&path).with_context(|| format!("cannot write {}", path.display()))?;
    writeln!(file, "{}", json!({"time": now(), "set": name, "value": value}))?;
    Ok(())
}

/// The wallet set up for remote mode, if it is on.
pub fn wallet(home: &Path) -> Option<String> {
    get(home, "wallet").filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_line_is_in_force_and_the_file_is_the_history() {
        let home = std::env::temp_dir().join(format!("cwbcode-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        assert_eq!(wallet(&home), None);
        set(&home, "wallet", "0x1111111111111111111111111111111111111111").unwrap();
        set(&home, "other", "x").unwrap();
        assert_eq!(wallet(&home).as_deref(), Some("0x1111111111111111111111111111111111111111"));
        set(&home, "wallet", "0x2222222222222222222222222222222222222222").unwrap();
        assert_eq!(wallet(&home).as_deref(), Some("0x2222222222222222222222222222222222222222"));
        // Off is a line too; a damaged line does not hide the ones around it.
        set(&home, "wallet", "").unwrap();
        assert_eq!(wallet(&home), None);
        std::fs::OpenOptions::new().append(true).open(home.join(FILE)).unwrap().write_all(b"not json\n").unwrap();
        set(&home, "wallet", "0x3333333333333333333333333333333333333333").unwrap();
        assert_eq!(wallet(&home).as_deref(), Some("0x3333333333333333333333333333333333333333"));
        // One JSON object per line, each with its time; private.
        let text = std::fs::read_to_string(home.join(FILE)).unwrap();
        assert_eq!(text.lines().count(), 6);
        let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!((first["set"].as_str(), first["value"].as_str()), (Some("wallet"), Some("0x1111111111111111111111111111111111111111")));
        assert_eq!(first["time"].as_str().unwrap().len(), 19);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(home.join(FILE)).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
