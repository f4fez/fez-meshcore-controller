// Copyright 2026 Florian MAZEN (F4FEZ)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Single-instance guard: an exclusive `flock` on a lock file, held for the
//! lifetime of the process. Acquired before daemonizing (`--daemon` forks
//! into the background), so the lock is inherited by the surviving child
//! and a second instance started against the same config fails fast,
//! whether it's run in the foreground or the background.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

/// Holds the daemon's exclusive lock file for as long as it's alive. The
/// underlying `flock` is released automatically by the OS when the process
/// exits (even if it crashes), so no stale lock can survive it.
pub struct SingleInstanceLock {
    file: File,
}

impl SingleInstanceLock {
    /// Acquires the lock at `path`, creating the file if needed and
    /// writing the current process ID into it. Fails immediately (rather
    /// than blocking) if another process already holds it.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // `truncate(false)`: if another instance already holds the lock,
        // opening the file must not wipe out its recorded PID.
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;

        // SAFETY: `file.as_raw_fd()` is a valid, open file descriptor for
        // the duration of this call.
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if locked != 0 {
            let err = io::Error::last_os_error();
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "another instance is already running (lock held on {}): {err}",
                    path.display()
                ),
            ));
        }

        let mut lock = Self { file };
        lock.write_current_pid()?;
        Ok(lock)
    }

    /// (Re)writes the current process ID into the lock file. Needed after
    /// `--daemon` forks into the background: the PID recorded at
    /// [`Self::acquire`] time (the pre-fork process, which then exits) is
    /// stale once the surviving child takes over — the flock itself is
    /// still correctly inherited across the fork, only the *recorded* PID
    /// needs refreshing.
    pub fn write_current_pid(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        write!(self.file, "{}", std::process::id())?;
        self.file.sync_all()?;
        Ok(())
    }
}
