//! Interactive Windows editor integration. All source discovery and cook I/O stays off the render thread.
use super::project::{AssetGeneration, Project};
use renderer::pipeline::PipelineResult;
use std::{
    fs, io,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::SystemTime,
};
use windows_sys::Win32::{
    Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Storage::FileSystem::{
        FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
        FILE_NOTIFY_CHANGE_SIZE, FindCloseChangeNotification, FindFirstChangeNotificationW,
        FindNextChangeNotification,
    },
    System::Threading::WaitForMultipleObjects,
};

struct DirectoryWatch(HANDLE);

impl DirectoryWatch {
    fn open(path: &Path, recursive: bool) -> io::Result<Self> {
        let wide: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
        let mask = FILE_NOTIFY_CHANGE_FILE_NAME
            | FILE_NOTIFY_CHANGE_DIR_NAME
            | FILE_NOTIFY_CHANGE_SIZE
            | FILE_NOTIFY_CHANGE_LAST_WRITE;
        // SAFETY: wide is a NUL-terminated path valid for this call.
        let handle = unsafe { FindFirstChangeNotificationW(wide.as_ptr(), recursive.into(), mask) };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }
    fn rearm(&self) -> io::Result<()> {
        // SAFETY: the change-notification handle remains owned until Drop.
        if unsafe { FindNextChangeNotification(self.0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for DirectoryWatch {
    fn drop(&mut self) {
        // SAFETY: this object owns the notification handle and closes it exactly once.
        unsafe {
            FindCloseChangeNotification(self.0);
        }
    }
}

type Update = (PathBuf, AssetGeneration);

pub(crate) struct Preview {
    receiver: Receiver<Result<Update, String>>,
    root: PathBuf,
    stop: Arc<AtomicBool>,
}

fn index_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

// Arm both notifications before the initial cook, so an edit during startup
// or a conflict resolved only in Git's index triggers reconciliation.
fn watch(
    project: Project,
    sender: &mpsc::SyncSender<Result<Update, String>>,
    stop: &AtomicBool,
) -> PipelineResult<()> {
    let source = DirectoryWatch::open(&project.source, true)?;
    let index = DirectoryWatch::open(
        project.index.parent().ok_or("missing Git directory")?,
        false,
    )?;
    let handles = [source.0, index.0];
    let mut stamp = index_stamp(&project.index);
    let mut published = None;
    let mut changed = true;
    while !stop.load(Ordering::Relaxed) {
        if changed {
            match project.open() {
                Ok(update) => {
                    let id = *update.1.catalog.id();
                    if Some(id) != published {
                        if sender.send(Ok(update)).is_err() {
                            break;
                        }
                        published = Some(id);
                    }
                }
                Err(error) if published.is_some() => eprintln!("equipment preview: {error}"),
                Err(error) => return Err(error),
            }
        }
        // SAFETY: both registered handles remain open and the array holds two elements.
        let event = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, 250) };
        changed = match event {
            WAIT_OBJECT_0 => {
                source.rearm()?;
                true
            }
            other if other == WAIT_OBJECT_0 + 1 => {
                index.rearm()?;
                let next = index_stamp(&project.index);
                let different = next != stamp;
                stamp = next;
                different
            }
            WAIT_TIMEOUT => false,
            _ => return Err(io::Error::last_os_error().into()),
        };
    }
    Ok(())
}

impl Preview {
    pub(super) fn start(project: Project) -> PipelineResult<(Self, Update)> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::Builder::new()
            .name("equipment-preview".into())
            .spawn(move || {
                if let Err(error) = watch(project, &sender, &stopped) {
                    let _ = sender.send(Err(error.to_string()));
                }
            })?;
        let initial = receiver.recv()??;
        Ok((
            Self {
                receiver,
                stop,
                root: initial.0.clone(),
            },
            initial,
        ))
    }

    pub(super) fn update(&self) -> Option<AssetGeneration> {
        match self.receiver.try_recv().ok()? {
            Ok((root, generation)) if root == self.root => Some(generation),
            Ok(_) => {
                eprintln!("equipment store changed; restart the preview to use its new location");
                None
            }
            Err(error) => {
                eprintln!("equipment preview watcher stopped: {error}");
                None
            }
        }
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
