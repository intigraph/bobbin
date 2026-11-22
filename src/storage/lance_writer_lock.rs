use super::VectorStore;

impl VectorStore {
    /// Try the index wrapper's outer lock before opportunistic maintenance.
    /// A configured missing, replaced or non-regular lock fails closed: stats
    /// remain readable, but this reader must not become an uncoordinated writer.
    pub(super) fn try_read_path_writer_lock(&self) -> std::io::Result<Option<std::fs::File>> {
        use fs4::FileExt;
        let Some(path) = self.read_path_writer_lock.as_ref() else {
            return Ok(None);
        };
        let named = std::fs::symlink_metadata(path)?;
        if !named.file_type().is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "index writer lock is not a regular file",
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(path)?;
        file.try_lock_exclusive()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let opened = file.metadata()?;
            let current = std::fs::symlink_metadata(path)?;
            if !current.file_type().is_file()
                || (opened.dev(), opened.ino()) != (current.dev(), current.ino())
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "index writer lock path changed",
                ));
            }
        }
        Ok(Some(file))
    }
}
