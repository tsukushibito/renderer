//! Versioned JSON saves and settings. Writers serialize their read-modify-write
//! operation with an OS file lock. A successful update publishes complete bytes.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

pub trait SaveData: Serialize + DeserializeOwned + Default {
    const VERSION: u32;
    /// Called after loading and before publication, including after migration.
    fn validate(&self) -> io::Result<()> {
        Ok(())
    }
    /// Applications can migrate earlier versions while retaining the original save on error.
    fn migrate(version: u32, _: serde_json::Value) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported save version {version}"),
        ))
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    version: u32,
    data: T,
}

/// Each directory stores one logical slot. Concurrent writers observe one another's edits.
pub struct SaveSlot {
    dir: PathBuf,
}

impl SaveSlot {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            dir: directory.into(),
        }
    }

    pub fn read<T: SaveData>(&self) -> io::Result<T> {
        let data = match fs::read(self.dir.join("data.json")) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
            Err(err) => return Err(err),
        };
        let envelope: Envelope<serde_json::Value> = serde_json::from_slice(&data)?;
        let value: T = if envelope.version == T::VERSION {
            serde_json::from_value(envelope.data)?
        } else if envelope.version < T::VERSION {
            T::migrate(envelope.version, envelope.data)?
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "save version {} requires a newer application",
                    envelope.version
                ),
            ));
        };
        value.validate()?;
        Ok(value)
    }

    pub fn update<T: SaveData>(&self, edit: impl FnOnce(&mut T)) -> io::Result<T> {
        fs::create_dir_all(&self.dir)?;
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join("write.lock"))?;
        lock.lock()?;
        let mut value: T = self.read()?;
        edit(&mut value);
        value.validate()?;
        let bytes = serde_json::to_vec_pretty(&Envelope {
            version: T::VERSION,
            data: &value,
        })?;
        let temporary = self.dir.join("pending.json");
        let mut file = File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(temporary, self.dir.join("data.json"))?;
        #[cfg(unix)]
        File::open(&self.dir)?.sync_all()?;
        Ok(value)
    }
}

/// Stable per-user storage root for a single application.
pub fn user_directory(app: &str) -> io::Result<PathBuf> {
    if app.is_empty()
        || !app
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid application identifier",
        ));
    }
    #[cfg(windows)]
    let root = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let root = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|p| Path::new(&p).join(".local/share")));
    root.filter(|p| Path::new(p).is_absolute())
        .map(|p| p.join("sashiko").join(app))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no user data directory; specify a storage directory",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    #[derive(Default, Deserialize, Serialize)]
    struct Score(u32);
    impl SaveData for Score {
        const VERSION: u32 = 1;
    }

    #[test]
    fn simultaneous_writers_preserve_updates_and_future_versions() {
        let path = std::env::temp_dir().join(format!(
            "sashiko-save-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let room = Arc::new(Barrier::new(4));
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let path = &path;
                let room = Arc::clone(&room);
                scope.spawn(move || {
                    room.wait();
                    SaveSlot::new(path)
                        .update(|count: &mut Score| count.0 += 1)
                        .unwrap();
                });
            }
        });
        let slot = SaveSlot::new(&path);
        assert_eq!(slot.read::<Score>().unwrap().0, 4);
        fs::write(path.join("pending.json"), b"interrupted").unwrap();
        assert_eq!(slot.read::<Score>().unwrap().0, 4);
        let future = br#"{"version":2,"data":99}"#;
        fs::write(path.join("data.json"), future).unwrap();
        assert!(slot.update(|_: &mut Score| {}).is_err());
        assert_eq!(fs::read(path.join("data.json")).unwrap(), future);
        fs::remove_dir_all(path).unwrap();
    }
}
