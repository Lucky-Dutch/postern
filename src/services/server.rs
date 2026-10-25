//! Restarting a PostgreSQL server that runs on this computer.
//!
//! Postern restarts a server only after proving that it is the server it is
//! connected to: the data directory reported by the server is readable, its
//! `postmaster.pid` names the parent process of the connected backend, and
//! launchd does not manage the process. The restart uses the server's own
//! `pg_ctl` and the command line recorded in `postmaster.opts`, so the port and
//! other options stay the same. Postgres.app starts servers this way and passes
//! `shared_preload_libraries` on the command line, so a library is added there
//! as well as in the configuration.

use crate::sql;
use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

/// How long `pg_ctl` may wait for the server to stop or start.
const PG_CTL_SECONDS: u64 = 120;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalServer {
    pub data_directory: PathBuf,
    pub postmaster_pid: i32,
    /// The `postgres` binary running the server.
    program: PathBuf,
    /// Server arguments from `postmaster.opts`, without the data directory,
    /// which `pg_ctl` passes itself.
    arguments: Vec<String>,
}

impl LocalServer {
    /// Finds the server behind a connection, or explains why Postern cannot
    /// restart it.
    pub async fn detect(data_directory: &str, backend_pid: i32) -> Result<Self, String> {
        const NOT_LOCAL: &str =
            "Postern can restart only a server that runs on this Mac under your user account.";
        let directory = PathBuf::from(data_directory);
        let pid_file = std::fs::read_to_string(directory.join("postmaster.pid"))
            .map_err(|_| NOT_LOCAL.to_owned())?;
        let postmaster_pid: i32 = pid_file
            .lines()
            .next()
            .and_then(|line| line.trim().parse().ok())
            .ok_or_else(|| NOT_LOCAL.to_owned())?;
        if parent_pid(backend_pid).await != Some(postmaster_pid) {
            return Err(NOT_LOCAL.to_owned());
        }
        if let Some(label) = launchd_label(postmaster_pid).await {
            let hint = label
                .strip_prefix("homebrew.mxcl.")
                .map(|formula| format!("brew services restart {formula}"))
                .unwrap_or_else(|| "the tool that manages it".to_owned());
            return Err(format!(
                "launchd manages this server ({label}). Restart it with {hint}."
            ));
        }
        let options = std::fs::read_to_string(directory.join("postmaster.opts"))
            .map_err(|_| NOT_LOCAL.to_owned())?;
        let (program, arguments) = parse_postmaster_opts(&options)
            .ok_or_else(|| "The server's postmaster.opts could not be read.".to_owned())?;
        let server = Self {
            data_directory: directory,
            postmaster_pid,
            program,
            arguments,
        };
        if !server.pg_ctl().is_file() {
            return Err(format!(
                "pg_ctl was not found at {}.",
                server.pg_ctl().display()
            ));
        }
        Ok(server)
    }

    pub fn pg_ctl(&self) -> PathBuf {
        self.program.with_file_name("pg_ctl")
    }

    /// The file that receives the server's output after a restart. Postgres.app
    /// uses the same file.
    pub fn log_file(&self) -> PathBuf {
        self.data_directory.join("postgresql.log")
    }

    /// True when the command line sets `shared_preload_libraries`, which then
    /// overrides the configuration files.
    pub fn preload_on_command_line(&self) -> bool {
        preload_argument(&self.arguments).is_some()
    }

    /// Stops the server with a fast shutdown and starts it with `library` added
    /// to a `shared_preload_libraries` value on its command line; otherwise the
    /// configuration files must already list it. If the server does not start,
    /// it is started again with its previous command line, and with `running`
    /// as `shared_preload_libraries` so a changed configuration cannot stop it.
    pub async fn restart_with_library(&self, library: &str, running: &[String]) -> Result<()> {
        let mut arguments = self.arguments.clone();
        if let Some(index) = preload_argument(&arguments) {
            let (prefix, value) = arguments[index]
                .split_once('=')
                .expect("preload arguments contain '='");
            let libraries = sql::with_library(&sql::parse_library_list(value), library);
            arguments[index] = format!("{prefix}={}", sql::format_library_list(&libraries));
        }
        self.run_pg_ctl(&[
            "stop".into(),
            "-D".into(),
            self.data_directory.display().to_string(),
            "-m".into(),
            "fast".into(),
            "-w".into(),
            "-t".into(),
            PG_CTL_SECONDS.to_string(),
        ])
        .await
        .context("The server could not be stopped")?;
        let Err(error) = self.start(&arguments).await else {
            return Ok(());
        };
        let mut previous = self.arguments.clone();
        if preload_argument(&previous).is_none() {
            previous.push("-c".into());
            previous.push(format!(
                "shared_preload_libraries={}",
                sql::format_library_list(running)
            ));
        }
        let log = self.log_tail();
        match self.start(&previous).await {
            Ok(()) => bail!(
                "The server did not start with {library}, so it was started again with its previous settings.\n\n{error:#}{log}"
            ),
            Err(again) => bail!(
                "The server did not start with {library}, and starting it with its previous settings failed too. Start it from the tool that manages it.\n\n{error:#}\n\n{again:#}{log}"
            ),
        }
    }

    async fn start(&self, arguments: &[String]) -> Result<()> {
        let mut command = vec![
            "start".to_owned(),
            "-D".into(),
            self.data_directory.display().to_string(),
            "-w".into(),
            "-t".into(),
            PG_CTL_SECONDS.to_string(),
            "-l".into(),
            self.log_file().display().to_string(),
        ];
        if !arguments.is_empty() {
            command.push("-o".into());
            command.push(shell_words(arguments));
        }
        self.run_pg_ctl(&command).await
    }

    async fn run_pg_ctl(&self, arguments: &[String]) -> Result<()> {
        // The server reads PGPORT and PGDATA at startup; keep this process's values
        // from changing a server that takes them from its configuration.
        let output = tokio::time::timeout(
            Duration::from_secs(PG_CTL_SECONDS + 30),
            tokio::process::Command::new(self.pg_ctl())
                .args(arguments)
                .env_remove("PGPORT")
                .env_remove("PGDATA")
                .stdin(Stdio::null())
                .output(),
        )
        .await
        .context("pg_ctl did not finish")??;
        if !output.status.success() {
            let mut message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let stdout = String::from_utf8_lossy(&output.stdout);
            if message.is_empty() {
                message = stdout.trim().to_owned();
            }
            bail!("pg_ctl {}: {message}", arguments[0]);
        }
        Ok(())
    }

    /// The last lines of the server log, for error messages.
    fn log_tail(&self) -> String {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut file) = std::fs::File::open(self.log_file()) else {
            return String::new();
        };
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::End(0)).unwrap_or(0) > 8192 {
            let _ = file.seek(SeekFrom::End(-8192));
        } else {
            let _ = file.seek(SeekFrom::Start(0));
        }
        if file.read_to_end(&mut bytes).is_err() {
            return String::new();
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let tail = lines[lines.len().saturating_sub(8)..].join("\n");
        format!("\n\nServer log ({}):\n{tail}", self.log_file().display())
    }
}

/// True when `name` exists as a shared library in one of the directories.
pub fn library_installed(directories: &[String], name: &str) -> bool {
    directories.iter().any(|directory| {
        ["so", "dylib"].iter().any(|extension| {
            Path::new(directory)
                .join(format!("{name}.{extension}"))
                .is_file()
        })
    })
}

/// Splits the single line of `postmaster.opts`: the program path, written
/// unquoted, then each argument in double quotes. The data directory argument
/// is dropped.
pub fn parse_postmaster_opts(text: &str) -> Option<(PathBuf, Vec<String>)> {
    let line = text.lines().next()?.trim_end();
    let (program, rest) = match line.find(" \"") {
        Some(index) => (&line[..index], &line[index + 1..]),
        None => (line, ""),
    };
    if program.is_empty() {
        return None;
    }
    let mut parsed: Vec<String> = Vec::new();
    if !rest.is_empty() {
        let inner = rest.strip_prefix('"')?.strip_suffix('"')?;
        parsed = inner.split("\" \"").map(str::to_owned).collect();
    }
    let mut arguments = Vec::new();
    let mut items = parsed.into_iter();
    while let Some(argument) = items.next() {
        if argument == "-D" {
            items.next();
        } else if !argument.starts_with("-D") {
            arguments.push(argument);
        }
    }
    Some((PathBuf::from(program), arguments))
}

/// The index of the argument holding `shared_preload_libraries=...`, written as
/// `-c name=value`, `-cname=value`, or `--name=value`.
fn preload_argument(arguments: &[String]) -> Option<usize> {
    let is_preload = |text: &str| {
        text.split_once('=').is_some_and(|(name, _)| {
            name.replace('-', "_")
                .eq_ignore_ascii_case("shared_preload_libraries")
        })
    };
    arguments.iter().enumerate().find_map(|(index, argument)| {
        let setting = if index > 0 && arguments[index - 1] == "-c" {
            argument.as_str()
        } else if let Some(rest) = argument.strip_prefix("--") {
            rest
        } else {
            argument.strip_prefix("-c")?
        };
        is_preload(setting).then_some(index)
    })
}

/// Quotes arguments for the shell that `pg_ctl` uses to start the server.
pub fn shell_words(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|argument| format!("'{}'", argument.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

async fn parent_pid(pid: i32) -> Option<i32> {
    let ps = if cfg!(target_os = "macos") {
        "/bin/ps"
    } else {
        "ps"
    };
    let output = tokio::process::Command::new(ps)
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// The launchd job running the process, if any.
async fn launchd_label(pid: i32) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = tokio::process::Command::new("/bin/launchctl")
        .arg("list")
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            let mut fields = line.split('\t');
            let job_pid = fields.next()?.trim().parse::<i32>().ok()?;
            let label = fields.nth(1)?.trim();
            (job_pid == pid).then(|| label.to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_postmaster_opts_without_the_data_directory() {
        let text = "/Applications/Postgres.app/Contents/Versions/18/bin/postgres \"-D\" \"/Users/me/Library/Application Support/Postgres/var-18\" \"-p\" \"5432\" \"-c\" \"shared_preload_libraries=auth_permission_dialog\"\n";
        let (program, arguments) = parse_postmaster_opts(text).unwrap();
        assert_eq!(
            program,
            PathBuf::from("/Applications/Postgres.app/Contents/Versions/18/bin/postgres")
        );
        assert_eq!(
            arguments,
            [
                "-p",
                "5432",
                "-c",
                "shared_preload_libraries=auth_permission_dialog"
            ]
        );
        assert_eq!(preload_argument(&arguments), Some(3));
        let (_, bare) = parse_postmaster_opts("/usr/lib/postgresql/16/bin/postgres\n").unwrap();
        assert!(bare.is_empty());
        let (_, joined) =
            parse_postmaster_opts("/bin/postgres \"-D/data\" \"--shared-preload-libraries=x\"")
                .unwrap();
        assert_eq!(joined, ["--shared-preload-libraries=x"]);
        assert_eq!(preload_argument(&joined), Some(0));
        assert_eq!(
            preload_argument(&["-cshared_preload_libraries=y".into()]),
            Some(0)
        );
        assert_eq!(
            preload_argument(&["-c".into(), "work_mem=4MB".into()]),
            None
        );
        assert!(parse_postmaster_opts("").is_none());
    }

    #[test]
    fn quotes_arguments_for_the_shell() {
        assert_eq!(
            shell_words(&[
                "-p".into(),
                "5432".into(),
                "-c".into(),
                "a=it's here".into()
            ]),
            "'-p' '5432' '-c' 'a=it'\\''s here'"
        );
    }
}
