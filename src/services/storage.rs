use crate::models::{ConnectionProfile, QueryDocument};
use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The data directory and Keychain names used before the rename to Postern.
const FORMER_ORGANIZATION: &str = "pgmanager";
const FORMER_APPLICATION: &str = "PG Manager";
const KEYCHAIN_SERVICE: &str = "dev.postern.database";
const FORMER_KEYCHAIN_SERVICE: &str = "dev.pgmanager.database";

/// Moves the former data directory to `current` when only the former one exists.
/// If the move fails, the former directory keeps being used, so no saved
/// servers or queries disappear.
fn adopt_former_directory(former: &Path, current: PathBuf) -> PathBuf {
    if current.exists() || !former.is_dir() {
        return current;
    }
    if let Some(parent) = current.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::rename(former, &current) {
        Ok(()) => current,
        Err(_) => former.to_path_buf(),
    }
}

pub struct Store {
    directory: PathBuf,
}

impl Store {
    pub fn open() -> Result<Self> {
        Self::at(Self::directory()?)
    }

    /// The application data directory, or `POSTERN_DATA_DIR` when set. Data
    /// saved under the application's former name, PG Manager, is moved here the
    /// first time.
    pub fn directory() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os("POSTERN_DATA_DIR") {
            return Ok(PathBuf::from(path));
        }
        let directory = |organization: &str, application: &str| {
            ProjectDirs::from("dev", organization, application)
                .map(|dirs| dirs.data_dir().to_path_buf())
                .context("Cannot locate the application data directory.")
        };
        Ok(adopt_former_directory(
            &directory(FORMER_ORGANIZATION, FORMER_APPLICATION)?,
            directory("postern", "Postern")?,
        ))
    }

    pub fn at(directory: PathBuf) -> Result<Self> {
        fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self { directory })
    }

    fn load<T: DeserializeOwned + Default>(&self, name: &str) -> Result<T> {
        match fs::read(self.directory.join(name)) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| {
                format!("Cannot read {name}. The original file has been preserved.")
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
            Err(error) => Err(error.into()),
        }
    }

    fn save<T: Serialize + ?Sized>(&self, name: &str, value: &T) -> Result<()> {
        atomic_write(
            &self.directory.join(name),
            &serde_json::to_vec_pretty(value)?,
        )
    }

    pub fn profiles(&self) -> Result<Vec<ConnectionProfile>> {
        self.load("connections.json")
    }
    pub fn save_profiles(&self, value: &[ConnectionProfile]) -> Result<()> {
        self.save("connections.json", value)
    }
    pub fn documents(&self) -> Result<Vec<QueryDocument>> {
        self.load("queries.json")
    }
    pub fn save_documents(&self, value: &[QueryDocument]) -> Result<()> {
        self.save("queries.json", value)
    }
    pub fn history(&self) -> Result<Vec<String>> {
        self.load("history.json")
    }
    pub fn save_history(&self, value: &[String]) -> Result<()> {
        self.save("history.json", value)
    }
    pub fn ui_state(&self) -> Result<crate::models::UiState> {
        self.load("ui_state.json")
    }
    pub fn save_ui_state(&self, value: &crate::models::UiState) -> Result<()> {
        self.save("ui_state.json", value)
    }
}

/// Atomically writes application data with owner-only permissions.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    write_with_mode(path, bytes, 0o600, false)
}

/// Atomically writes a file the user chose, such as a SQL file or an export.
/// A symbolic link is followed so the link stays in place, and an existing file
/// keeps its permissions. New files get the conventional 0644 mode.
pub fn write_user_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = AtomicFile::create_user(path)?;
    file.write_all(bytes)?;
    file.commit()
}

fn write_with_mode(path: &Path, bytes: &[u8], mode: u32, exact: bool) -> Result<()> {
    use std::io::Write;
    let mut file = AtomicFile::create(path.to_path_buf(), mode, exact)?;
    file.write_all(bytes)?;
    file.commit()
}

/// A file written through a temporary file in the same directory. `commit`
/// replaces the target; dropping without committing removes the temporary file,
/// so a failed or cancelled export never leaves a partial file behind.
pub struct AtomicFile {
    temporary: PathBuf,
    target: PathBuf,
    file: Option<std::io::BufWriter<fs::File>>,
}

impl AtomicFile {
    /// Prepares a user-chosen path: follows symbolic links and keeps an existing
    /// file's permissions.
    pub fn create_user(path: &Path) -> Result<Self> {
        let target = match fs::canonicalize(path) {
            Ok(resolved) => resolved,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
            Err(error) => return Err(error.into()),
        };
        #[cfg(unix)]
        let existing = {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(&target)
                .ok()
                .map(|metadata| metadata.permissions().mode() & 0o7777)
        };
        #[cfg(not(unix))]
        let existing: Option<u32> = None;
        Self::create(target, existing.unwrap_or(0o644), existing.is_some())
    }

    /// With `exact`, the mode is set regardless of the umask, which preserves an
    /// existing file's mode.
    fn create(target: PathBuf, mode: u32, exact: bool) -> Result<Self> {
        let temporary = target.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = (mode, exact);
        let file = options.open(&temporary)?;
        let atomic = Self {
            temporary,
            target,
            file: Some(std::io::BufWriter::new(file)),
        };
        #[cfg(unix)]
        if exact {
            use std::os::unix::fs::PermissionsExt;
            atomic
                .file
                .as_ref()
                .expect("open file")
                .get_ref()
                .set_permissions(fs::Permissions::from_mode(mode))?;
        }
        Ok(atomic)
    }

    pub fn commit(mut self) -> Result<()> {
        let file = self.file.take().expect("file is open until commit");
        let file = file.into_inner().map_err(|error| error.into_error())?;
        file.sync_all()?;
        fs::rename(&self.temporary, &self.target)?;
        Ok(())
    }
}

impl std::io::Write for AtomicFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.file.as_mut().expect("file is open").write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.as_mut().expect("file is open").flush()
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = fs::remove_file(&self.temporary);
        }
    }
}

/// The saved password, or an empty string. A password saved under the former
/// Keychain service is moved to the current one when it is first read.
pub fn password(profile: &ConnectionProfile) -> Result<String> {
    let account = profile.id.to_string();
    match keyring::Entry::new(KEYCHAIN_SERVICE, &account)?.get_password() {
        Ok(value) => return Ok(value),
        Err(keyring::Error::NoEntry) => {}
        Err(error) => return Err(error.into()),
    }
    let former = keyring::Entry::new(FORMER_KEYCHAIN_SERVICE, &account)?;
    match former.get_password() {
        Ok(value) => {
            // Keep the former entry if the copy fails, so the password is not lost.
            if keyring::Entry::new(KEYCHAIN_SERVICE, &account)
                .and_then(|entry| entry.set_password(&value))
                .is_ok()
            {
                let _ = former.delete_credential();
            }
            Ok(value)
        }
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

/// Saves the password, or removes it when `value` is empty. Any copy under the
/// former Keychain service is removed as well.
pub fn save_password(profile: &ConnectionProfile, value: &str) -> Result<()> {
    let account = profile.id.to_string();
    match keyring::Entry::new(FORMER_KEYCHAIN_SERVICE, &account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(error) => return Err(error.into()),
    }
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, &account)?;
    if value.is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    } else {
        entry.set_password(value).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_the_former_data_directory_once() {
        let root = tempfile::tempdir().unwrap();
        let former = root.path().join("dev.pgmanager.PG-Manager");
        let current = root.path().join("nested/dev.postern.Postern");
        // Nothing to move.
        assert_eq!(adopt_former_directory(&former, current.clone()), current);
        assert!(!current.exists());
        fs::create_dir_all(&former).unwrap();
        fs::write(former.join("connections.json"), b"[]").unwrap();
        assert_eq!(adopt_former_directory(&former, current.clone()), current);
        assert_eq!(fs::read(current.join("connections.json")).unwrap(), b"[]");
        assert!(!former.exists());
        // An existing current directory is never replaced.
        fs::create_dir_all(&former).unwrap();
        fs::write(former.join("connections.json"), b"[1]").unwrap();
        assert_eq!(adopt_former_directory(&former, current.clone()), current);
        assert_eq!(fs::read(current.join("connections.json")).unwrap(), b"[]");
        assert!(former.exists());
    }
    #[test]
    fn preserves_corrupt_profiles_and_roundtrips_sql_drafts() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::at(directory.path().to_path_buf()).unwrap();
        store
            .save_profiles(&[ConnectionProfile::default()])
            .unwrap();
        assert_eq!(store.profiles().unwrap().len(), 1);
        fs::write(directory.path().join("connections.json"), b"{broken").unwrap();
        assert!(store.profiles().is_err());
        assert_eq!(
            fs::read(directory.path().join("connections.json")).unwrap(),
            b"{broken"
        );
        let document = QueryDocument::default();
        store
            .save_documents(std::slice::from_ref(&document))
            .unwrap();
        assert_eq!(store.documents().unwrap()[0].sql, document.sql);
    }

    #[cfg(unix)]
    #[test]
    fn user_files_keep_permissions_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("query.sql");
        fs::write(&file, b"old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        let link = directory.path().join("link.sql");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        write_user_file(&link, b"new").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&file).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let fresh = directory.path().join("export.csv");
        write_user_file(&fresh, b"a").unwrap();
        assert_eq!(
            fs::metadata(&fresh).unwrap().permissions().mode() & 0o600,
            0o600
        );
        atomic_write(&fresh, b"b").unwrap();
        assert_eq!(
            fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
