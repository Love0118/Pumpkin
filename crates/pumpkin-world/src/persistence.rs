//! Crash-resistant file replacement primitives shared by world persistence paths.

use std::fs::{File, OpenOptions};
use std::io::{self, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Instant;

use tokio::io::{AsyncSeek, AsyncWrite};

use pumpkin_nbt::compound::NbtCompound;

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Builds a bounded top-level residual compound after removing every key owned
/// by the authoritative codec. Unknown nested values remain byte-for-byte
/// representable through the normal NBT value model.
pub fn bounded_residual_nbt<'a>(
    source: &NbtCompound,
    authoritative_keys: impl IntoIterator<Item = &'a str>,
    max_entries: usize,
    max_bytes: usize,
) -> Result<NbtCompound, String> {
    let mut residual = source.clone();
    for key in authoritative_keys {
        residual.child_tags.remove(key);
    }

    if residual.child_tags.len() > max_entries {
        return Err(format!(
            "{} residual entries exceeds {max_entries}",
            residual.child_tags.len()
        ));
    }

    let encoded_len = pumpkin_nbt::Nbt::from(residual.clone())
        .write_unnamed()
        .map_err(|error| format!("failed to size residual NBT: {error}"))?
        .len();
    if encoded_len > max_bytes {
        return Err(format!("{encoded_len} residual bytes exceeds {max_bytes}"));
    }
    Ok(residual)
}

fn create_parent(path: &Path) -> io::Result<&Path> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("persistence path has no parent: {}", path.display()),
        )
    })?;
    std::fs::create_dir_all(parent)?;
    Ok(parent)
}

fn next_temp_path(path: &Path) -> io::Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("persistence path has no file name: {}", path.display()),
        )
    })?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp_name = format!(
        ".{}.pumpkin-tmp-{}-{sequence}",
        file_name.to_string_lossy(),
        std::process::id()
    );
    Ok(path.with_file_name(temp_name))
}

fn open_unique_temp(path: &Path) -> io::Result<(PathBuf, File)> {
    create_parent(path)?;
    for _ in 0..16 {
        let temp_path = next_temp_path(path)?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("could not allocate a temporary file for {}", path.display()),
    ))
}

#[cfg(windows)]
fn replace_file(temp_path: &Path, target_path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let existing: Vec<u16> = temp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let new: Vec<u16> = target_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: both paths are encoded as owned, null-terminated UTF-16 buffers
    // that remain alive for the duration of the call.
    let result = unsafe {
        MoveFileExW(
            existing.as_ptr(),
            new.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, target_path: &Path) -> io::Result<()> {
    std::fs::rename(temp_path, target_path)?;
    if let Some(parent) = target_path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Writes a file through a same-directory temporary file and durably replaces
/// the target only after the new contents have reached stable storage.
pub fn atomic_write<E>(
    path: &Path,
    write_contents: impl FnOnce(&mut File) -> Result<(), E>,
) -> Result<(), E>
where
    E: From<io::Error>,
{
    atomic_write_inner(path, write_contents, replace_file)
}

fn atomic_write_inner<E>(
    path: &Path,
    write_contents: impl FnOnce(&mut File) -> Result<(), E>,
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> Result<(), E>
where
    E: From<io::Error>,
{
    let (temp_path, mut file) = open_unique_temp(path).map_err(E::from)?;
    let result = (|| {
        let write_started = Instant::now();
        write_contents(&mut file)?;
        file.flush().map_err(E::from)?;
        crate::serialization_metrics::record_duration(
            crate::serialization_metrics::SerializationStage::Write,
            write_started.elapsed(),
        );
        let written_bytes = file.metadata().map_err(E::from)?.len();
        let fsync_started = Instant::now();
        file.sync_all().map_err(E::from)?;
        crate::serialization_metrics::record_duration(
            crate::serialization_metrics::SerializationStage::Fsync,
            fsync_started.elapsed(),
        );
        crate::serialization_metrics::record_written_bytes(written_bytes);
        drop(file);
        replace(&temp_path, path).map_err(E::from)
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// Async writer backed by a same-directory temporary file. Call [`Self::commit`]
/// after all bytes have been written; dropping it before commit removes the
/// temporary file and leaves the target untouched.
pub struct AsyncAtomicFile {
    target_path: PathBuf,
    temp_path: PathBuf,
    file: Option<tokio::fs::File>,
    bytes_written: u64,
    committed: bool,
}

impl AsyncAtomicFile {
    /// Creates a unique temporary file next to `target_path`.
    pub async fn create(target_path: impl Into<PathBuf>) -> io::Result<Self> {
        let target_path = target_path.into();
        create_parent(&target_path)?;

        for _ in 0..16 {
            let temp_path = next_temp_path(&target_path)?;
            match tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .await
            {
                Ok(file) => {
                    return Ok(Self {
                        target_path,
                        temp_path,
                        file: Some(file),
                        bytes_written: 0,
                        committed: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "could not allocate a temporary file for {}",
                target_path.display()
            ),
        ))
    }

    /// Flushes and syncs the temporary file, then atomically replaces the target.
    pub async fn commit(mut self) -> io::Result<()> {
        let file = self.file.take().ok_or_else(|| {
            io::Error::other("atomic persistence file was already closed before commit")
        })?;
        let fsync_started = Instant::now();
        file.sync_all().await?;
        crate::serialization_metrics::record_duration(
            crate::serialization_metrics::SerializationStage::Fsync,
            fsync_started.elapsed(),
        );
        drop(file);

        let temp_path = self.temp_path.clone();
        let target_path = self.target_path.clone();
        tokio::task::spawn_blocking(move || replace_file(&temp_path, &target_path))
            .await
            .map_err(io::Error::other)??;
        self.committed = true;
        crate::serialization_metrics::record_written_bytes(self.bytes_written);
        Ok(())
    }
}

impl AsyncWrite for AsyncAtomicFile {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let Some(file) = self.file.as_mut() else {
            return Poll::Ready(Err(closed_file_error()));
        };
        let result = Pin::new(file).poll_write(context, buffer);
        if let Poll::Ready(Ok(written)) = &result {
            self.bytes_written = self.bytes_written.saturating_add(*written as u64);
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let Some(file) = self.file.as_mut() else {
            return Poll::Ready(Err(closed_file_error()));
        };
        Pin::new(file).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.file.as_mut().map_or(Poll::Ready(Ok(())), |file| {
            Pin::new(file).poll_shutdown(context)
        })
    }
}

impl AsyncSeek for AsyncAtomicFile {
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        let Some(file) = self.file.as_mut() else {
            return Err(closed_file_error());
        };
        Pin::new(file).start_seek(position)
    }

    fn poll_complete(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<u64>> {
        let Some(file) = self.file.as_mut() else {
            return Poll::Ready(Err(closed_file_error()));
        };
        Pin::new(file).poll_complete(context)
    }
}

fn closed_file_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "atomic persistence file is closed",
    )
}

impl Drop for AsyncAtomicFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.temp_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, SeekFrom, Write};

    use tempfile::tempdir;
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    use pumpkin_nbt::compound::NbtCompound;

    use super::{AsyncAtomicFile, atomic_write, atomic_write_inner, bounded_residual_nbt};

    #[test]
    fn bounded_residual_removes_authoritative_keys_and_preserves_unknown_values() {
        let mut source = NbtCompound::new();
        source.put_int("xPos", 3);
        source.put_string("plugin:marker", "kept");

        let residual = bounded_residual_nbt(&source, ["xPos"], 8, 1024).unwrap();

        assert!(residual.get("xPos").is_none());
        assert_eq!(residual.get_string("plugin:marker"), Some("kept"));
        assert!(bounded_residual_nbt(&source, std::iter::empty(), 1, 1024).is_err());
        assert!(bounded_residual_nbt(&source, std::iter::empty(), 8, 4).is_err());
    }

    #[test]
    fn atomic_write_replaces_an_existing_file() {
        let directory = tempdir().expect("temporary directory should be created");
        let path = directory.path().join("level.dat");
        std::fs::write(&path, b"old").expect("old file should be written");

        atomic_write(&path, |file| file.write_all(b"new"))
            .expect("atomic replacement should succeed");

        assert_eq!(
            std::fs::read(&path).expect("replacement should be readable"),
            b"new"
        );
    }

    #[test]
    fn failed_atomic_write_preserves_the_old_file() {
        let directory = tempdir().expect("temporary directory should be created");
        let path = directory.path().join("player.dat");
        std::fs::write(&path, b"old").expect("old file should be written");

        let result = atomic_write(&path, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected failure"))
        });

        assert!(result.is_err());
        assert_eq!(
            std::fs::read(&path).expect("old file should remain readable"),
            b"old"
        );
        let remaining_files = std::fs::read_dir(directory.path())
            .expect("temporary directory should be readable")
            .count();
        assert_eq!(remaining_files, 1, "failed write must clean its temp file");
    }

    #[test]
    fn replace_failure_after_sync_preserves_old_file_and_cleans_temp() {
        let directory = tempdir().expect("temporary directory should be created");
        let path = directory.path().join("level.dat");
        std::fs::write(&path, b"old").expect("old file should be written");

        let result: io::Result<()> = atomic_write_inner(
            &path,
            |file| file.write_all(b"new"),
            |temp_path, target_path| {
                assert_eq!(std::fs::read(target_path)?, b"old");
                assert_eq!(std::fs::read(temp_path)?, b"new");
                Err(io::Error::other("injected replace failure"))
            },
        );

        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn permission_failure_preserves_existing_target() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().unwrap();
        let path = directory.path().join("level.dat");
        std::fs::write(&path, b"old").unwrap();
        let original_permissions = std::fs::metadata(directory.path()).unwrap().permissions();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o500)).unwrap();

        let result = atomic_write(&path, |file| file.write_all(b"new"));

        std::fs::set_permissions(directory.path(), original_permissions).unwrap();
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
    }

    #[tokio::test]
    async fn async_atomic_file_replaces_an_existing_file() {
        let directory = tempdir().expect("temporary directory should be created");
        let path = directory.path().join("region.mca");
        std::fs::write(&path, b"old").expect("old file should be written");

        let mut file = AsyncAtomicFile::create(path.clone())
            .await
            .expect("async temp file should be created");
        file.write_all(b"new")
            .await
            .expect("async temp file should accept bytes");
        file.commit()
            .await
            .expect("async atomic replacement should succeed");

        assert_eq!(
            tokio::fs::read(path)
                .await
                .expect("replacement should be readable"),
            b"new"
        );
    }

    #[tokio::test]
    async fn async_atomic_file_supports_header_backfill_before_commit() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("region.linear");
        let mut file = AsyncAtomicFile::create(path.clone()).await.unwrap();
        file.write_all(b"0000payload").await.unwrap();
        file.seek(SeekFrom::Start(0)).await.unwrap();
        file.write_all(b"head").await.unwrap();
        file.commit().await.unwrap();

        assert_eq!(tokio::fs::read(path).await.unwrap(), b"headpayload");
    }

    #[tokio::test]
    async fn dropping_uncommitted_async_file_removes_temp() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("region.linear");
        let mut file = AsyncAtomicFile::create(path).await.unwrap();
        file.write_all(b"partial").await.unwrap();
        drop(file);

        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn async_replace_failure_cleans_temp_after_sync() {
        let directory = tempdir().unwrap();
        let target = directory.path().join("region.linear");
        std::fs::create_dir(&target).unwrap();
        let mut file = AsyncAtomicFile::create(target.clone()).await.unwrap();
        file.write_all(b"new").await.unwrap();

        assert!(file.commit().await.is_err());
        assert!(target.is_dir());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
