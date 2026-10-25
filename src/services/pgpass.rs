//! Password lookup in `~/.pgpass` (or `PGPASSFILE`) following libpq's rules.
//! The file is read only when a profile has no password, and it is ignored when
//! group or other users can read it, as libpq does.

use std::path::PathBuf;

pub fn lookup(host: &str, port: u16, database: &str, user: &str) -> Option<String> {
    let path = std::env::var_os("PGPASSFILE")
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".pgpass")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(&path).ok()?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            return None;
        }
    }
    let contents = std::fs::read_to_string(path).ok()?;
    find(&contents, host, port, database, user)
}

/// Finds the first matching entry. A host given as a Unix socket directory
/// matches `localhost`, like libpq. `*` matches any value.
pub fn find(contents: &str, host: &str, port: u16, database: &str, user: &str) -> Option<String> {
    let host = if host.starts_with('/') {
        "localhost"
    } else {
        host
    };
    let port = port.to_string();
    contents
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            let fields = split_fields(line);
            (fields.len() == 5).then_some(fields)
        })
        .find(|fields| {
            [host, port.as_str(), database, user]
                .iter()
                .zip(fields)
                .all(|(value, pattern)| pattern == "*" || pattern == value)
        })
        .map(|mut fields| fields.remove(4))
}

fn split_fields(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.trim_end_matches('\r').chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(escaped) = chars.next() {
                    fields.last_mut().expect("one field").push(escaped);
                }
            }
            ':' if fields.len() < 5 => fields.push(String::new()),
            _ => fields.last_mut().expect("one field").push(c),
        }
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_wildcards_escapes_and_socket_hosts() {
        let file = "# comment\n\
                    db.example:5432:app:alice:first\n\
                    *:*:*:bob:pa\\:ss\\\\word\n\
                    localhost:5432:*:carol:socket\n\
                    short:line\n";
        assert_eq!(
            find(file, "db.example", 5432, "app", "alice").as_deref(),
            Some("first")
        );
        assert_eq!(find(file, "db.example", 5433, "app", "alice"), None);
        assert_eq!(
            find(file, "anything", 1, "x", "bob").as_deref(),
            Some("pa:ss\\word")
        );
        assert_eq!(
            find(file, "/tmp", 5432, "x", "carol").as_deref(),
            Some("socket")
        );
        assert_eq!(find(file, "short", 5432, "x", "line"), None);
    }
}
