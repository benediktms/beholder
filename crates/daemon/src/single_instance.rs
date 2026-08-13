use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

#[derive(Debug)]
pub struct DaemonLock {
    file: File,
}

pub fn acquire(state_dir: &Path) -> Result<DaemonLock, std::io::Error> {
    let mut file = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state_dir.join("beholderd.pid"))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::Error(error)) => return Err(error),
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut pid = String::new();
            file.read_to_string(&mut pid)?;
            return Err(std::io::Error::other(format!(
                "beholderd already running with pid {}",
                pid.trim()
            )));
        }
    }
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    writeln!(file, "{}", std::process::id())?;
    file.sync_data()?;
    Ok(DaemonLock { file })
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
        let _ = self.file.sync_data();
    }
}
