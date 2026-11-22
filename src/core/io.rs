use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

/// Buffer size for large sequential reads and writes. Large enough to keep
/// syscall overhead low without making per-worker memory noticeable.
pub(crate) const IO_BUFFER_SIZE: usize = 256 * 1024;

/// Buffer size for the archive file itself. Writes are often small (headers,
/// central-directory records), so a larger buffer pays for itself.
pub(crate) const ARCHIVE_BUFFER_SIZE: usize = 1024 * 1024;

/// Small files are copied directly: allocating a 256 KB buffer to move 1 KB
/// costs more than the copy itself.
pub(crate) const SMALL_FILE_THRESHOLD: u64 = 64 * 1024;

/// A small buffer still amortizes the 4 KB reads used by the 7z encoder.
pub(crate) const SMALL_READ_BUFFER: usize = 8 * 1024;

#[cfg(windows)]
const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;

thread_local! {
    /// One large copy buffer per thread, reused across files. This avoids a
    /// fresh `BufReader`/`BufWriter` allocation for every archive entry.
    static COPY_BUFFER: RefCell<Vec<u8>> = RefCell::new(vec![0u8; IO_BUFFER_SIZE]);
}

/// Copy a stream of known size, using the thread-local buffer for large
/// entries and `std::io::copy`'s stack buffer for small ones.
fn copy_stream<R: Read + ?Sized, W: Write + ?Sized>(
    reader: &mut R,
    writer: &mut W,
    size: u64,
) -> io::Result<()> {
    if size < SMALL_FILE_THRESHOLD {
        io::copy(reader, writer)?;
        return Ok(());
    }

    COPY_BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        loop {
            let read = reader.read(&mut buffer[..])?;
            if read == 0 {
                break;
            }
            writer.write_all(&buffer[..read])?;
        }
        Ok(())
    })
}

/// Open a file for sequential reading. On Windows this sets
/// `FILE_FLAG_SEQUENTIAL_SCAN`, letting the cache manager tune read-ahead for
/// streaming compression instead of random access.
pub(crate) fn open_sequential(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(FILE_FLAG_SEQUENTIAL_SCAN);
    }

    options.open(path)
}

pub(crate) fn read_buffer_capacity(size: u64) -> usize {
    if size < SMALL_FILE_THRESHOLD {
        SMALL_READ_BUFFER
    } else {
        IO_BUFFER_SIZE
    }
}

/// Copy a file into a writer, choosing the buffer from its size.
pub(crate) fn copy_file_to<W: Write>(path: &Path, size: u64, writer: &mut W) -> io::Result<()> {
    let mut file = open_sequential(path)?;
    copy_stream(&mut file, writer, size)
}

/// Write a decompressed entry to disk. Small entries are written directly;
/// large ones reuse the thread-local copy buffer.
pub(crate) fn copy_to_file<R: Read + ?Sized>(
    reader: &mut R,
    path: &Path,
    size: u64,
) -> io::Result<()> {
    let mut file = File::create(path)?;
    copy_stream(reader, &mut file, size)
}

/// Write an entry that is already fully decoded in memory. Avoids a
/// `BufWriter` allocation and an extra copy for small-file extraction.
pub(crate) fn write_buffer(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_capacity_is_size_aware() {
        assert_eq!(read_buffer_capacity(0), SMALL_READ_BUFFER);
        assert_eq!(
            read_buffer_capacity(SMALL_FILE_THRESHOLD - 1),
            SMALL_READ_BUFFER
        );
        assert_eq!(read_buffer_capacity(SMALL_FILE_THRESHOLD), IO_BUFFER_SIZE);
    }
}
