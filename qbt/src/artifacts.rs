use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_JOURNAL_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) struct ArtifactStore {
    root: PathBuf,
    directory: cap_std::fs::Dir,
}

impl ArtifactStore {
    pub(crate) fn new(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        std::fs::create_dir_all(root.as_ref())?;
        let root = root.as_ref().canonicalize()?;
        anyhow::ensure!(
            root.to_str().is_some(),
            "artifact root must be valid UTF-8 so saved paths can be returned to clients"
        );
        Ok(Self {
            directory: cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority())?,
            root,
        })
    }

    /// Checks a caller-supplied path and returns it relative to the root.
    fn relative(&self, path: &Path) -> anyhow::Result<PathBuf> {
        anyhow::ensure!(
            !path
                .components()
                .any(|component| component == Component::ParentDir),
            "artifact path must not contain '..'"
        );
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .map_err(|_| anyhow::anyhow!("artifact path is outside the artifact root"))?
                .to_path_buf()
        } else {
            path.to_path_buf()
        };
        anyhow::ensure!(
            !relative.as_os_str().is_empty() && relative.file_name().is_some(),
            "artifact path must name a file"
        );
        Ok(relative)
    }

    fn create_parent(&self, relative: &Path) -> anyhow::Result<()> {
        if let Some(parent) = relative
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            self.directory.create_dir_all(parent)?;
        }
        Ok(())
    }

    pub(crate) fn save(&self, path: &Path, bytes: &[u8]) -> anyhow::Result<PathBuf> {
        let relative = self.relative(path)?;
        self.create_parent(&relative)?;
        // All resolution and the write are relative to the held directory
        // capability, so a concurrent symlink swap cannot escape the root.
        self.directory.write(&relative, bytes)?;
        Ok(self.root.join(relative))
    }

    pub(crate) fn read(&self, path: &Path) -> anyhow::Result<Vec<u8>> {
        Ok(self.directory.read(self.relative(path)?)?)
    }

    pub(crate) fn remove(&self, path: &Path) -> anyhow::Result<()> {
        Ok(self.directory.remove_file(self.relative(path)?)?)
    }

    /// Moves a file within the root, creating the destination's parents.
    pub(crate) fn rename(&self, from: &Path, to: &Path) -> anyhow::Result<PathBuf> {
        let to = self.relative(to)?;
        self.create_parent(&to)?;
        self.directory
            .rename(self.relative(from)?, &self.directory, &to)?;
        Ok(self.root.join(to))
    }

    /// Lists the file names directly under a directory, sorted. A missing
    /// directory lists as empty.
    pub(crate) fn list_files(&self, directory: &Path) -> anyhow::Result<Vec<String>> {
        let relative = self.relative(directory)?;
        let entries = match self.directory.read_dir(&relative) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        Ok(names)
    }

    pub(crate) fn exists(&self, path: &Path) -> anyhow::Result<bool> {
        Ok(self.directory.exists(self.relative(path)?))
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Hold a fresh directory capability for one backend's private journal files.
    pub(crate) fn journal_directory(&self) -> anyhow::Result<cap_std::fs::Dir> {
        self.directory.create_dir_all("journal")?;
        let name = format!(
            "journal/{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos(),
            NEXT_JOURNAL_ID.fetch_add(1, Ordering::Relaxed),
        );
        self.directory.create_dir(&name)?;
        Ok(self.directory.open_dir(name)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "qbt-artifacts-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn saves_relative_paths_and_creates_parents() {
        let root = temp_root("save");
        let store = ArtifactStore::new(&root).unwrap();
        let saved = store.save(Path::new("runs/x/a.png"), b"png").unwrap();
        assert_eq!(saved, root.canonicalize().unwrap().join("runs/x/a.png"));
        assert_eq!(std::fs::read(saved).unwrap(), b"png");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_parent_segments_and_absolute_paths_outside_the_root() {
        let root = temp_root("escape");
        let store = ArtifactStore::new(&root).unwrap();
        assert!(store.save(Path::new("runs/../a.png"), b"png").is_err());
        let outside = root.parent().unwrap().join("outside.png");
        assert!(store.save(&outside, b"png").is_err());
        assert!(!outside.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_parent_symlink_escapes() {
        let root = temp_root("symlink-root");
        let outside = temp_root("symlink-outside");
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let store = ArtifactStore::new(&root).unwrap();

        assert!(store.save(Path::new("link/a.png"), b"png").is_err());
        assert!(!outside.join("a.png").exists());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_roots_before_serving() {
        use std::os::unix::ffi::OsStringExt;

        let parent = temp_root("non-utf8-parent");
        let root = parent.join(std::ffi::OsString::from_vec(vec![b'r', 0xff, b't']));
        let error = match ArtifactStore::new(&root) {
            Ok(_) => panic!("non-UTF-8 root should be rejected"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("valid UTF-8"));
        std::fs::remove_dir_all(parent).unwrap();
    }
}
