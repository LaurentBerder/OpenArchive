use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use memmap2::{Mmap, MmapOptions};

use crate::core::progress::Progress;

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

/// Files at or above this size are mapped instead of read through a buffer.
/// Mapping avoids repeated read syscalls and the associated copies for very
/// large single files. `OPENARCHIVE_MMAP_THRESHOLD_MB=0` disables mapping.
const DEFAULT_MMAP_THRESHOLD: u64 = 64 * 1024 * 1024;

pub(crate) fn mmap_threshold() -> u64 {
    match std::env::var("OPENARCHIVE_MMAP_THRESHOLD_MB")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        Some(0) => u64::MAX,
        Some(mb) => mb.saturating_mul(1024 * 1024),
        None => DEFAULT_MMAP_THRESHOLD,
    }
}

/// A read-only mapping window. Mapping the whole file would keep every touched
/// page in the process working set; windows bound that while still avoiding
/// per-buffer read syscalls.
const MMAP_WINDOW_BYTES: u64 = 64 * 1024 * 1024;

/// Read-only memory mapping used for very large sequential inputs.
pub(crate) struct MappedFile {
    map: Mmap,
    position: usize,
}

impl MappedFile {
    pub(crate) fn map_range(file: &File, offset: u64, len: u64) -> Option<Self> {
        let len = usize::try_from(len).ok()?;
        // SAFETY: the mapping is read-only and is dropped with this value. As
        // with any mmap, callers must not truncate or rewrite the file while it
        // is mapped.
        let map = unsafe { MmapOptions::new().offset(offset).len(len).map(file) }.ok()?;
        Some(Self { map, position: 0 })
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.map
    }
}

impl Read for MappedFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.map.len().saturating_sub(self.position);
        if remaining == 0 || buf.is_empty() {
            return Ok(0);
        }

        let len = remaining.min(buf.len());
        buf[..len].copy_from_slice(&self.map[self.position..self.position + len]);
        self.position += len;
        Ok(len)
    }
}

#[cfg(windows)]
const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;

thread_local! {
    /// One large copy buffer per thread, reused across files. This avoids a
    /// fresh `BufReader`/`BufWriter` allocation for every archive entry.
    static COPY_BUFFER: RefCell<Vec<u8>> = RefCell::new(vec![0u8; IO_BUFFER_SIZE]);
}

/// Copy a stream of known size, using the thread-local buffer for large
/// entries and `std::io::copy`'s stack buffer for small ones.
///
/// With `progress`, cancellation is checked before each buffer (and before
/// a small entry) and the bytes copied are reported.
fn copy_stream<R: Read + ?Sized, W: Write + ?Sized>(
    reader: &mut R,
    writer: &mut W,
    size: u64,
    progress: Option<&Progress>,
) -> io::Result<()> {
    if size < SMALL_FILE_THRESHOLD {
        if let Some(progress) = progress {
            progress.check()?;
        }
        let copied = io::copy(reader, writer)?;
        if let Some(progress) = progress {
            progress.add(copied);
        }
        return Ok(());
    }

    COPY_BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        loop {
            if let Some(progress) = progress {
                progress.check()?;
            }
            let read = reader.read(&mut buffer[..])?;
            if read == 0 {
                break;
            }
            writer.write_all(&buffer[..read])?;
            if let Some(progress) = progress {
                progress.add(read as u64);
            }
        }
        Ok(())
    })
}

/// Read a stream to the end without keeping it (archive testing): the
/// decoder still decompresses and verifies every byte.
pub(crate) fn drain<R: Read + ?Sized>(reader: &mut R, progress: Option<&Progress>) -> io::Result<u64> {
    let mut sink = io::sink();
    let mut total = 0u64;
    COPY_BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        loop {
            if let Some(progress) = progress {
                progress.check()?;
            }
            let read = reader.read(&mut buffer[..])?;
            if read == 0 {
                return Ok(total);
            }
            sink.write_all(&buffer[..read])?;
            total += read as u64;
            if let Some(progress) = progress {
                progress.add(read as u64);
            }
        }
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
    let file = open_sequential(path)?;

    if size >= mmap_threshold() {
        let mut offset = 0u64;
        while offset < size {
            let len = (size - offset).min(MMAP_WINDOW_BYTES);
            match MappedFile::map_range(&file, offset, len) {
                Some(mapping) => {
                    writer.write_all(mapping.as_slice())?;
                    offset += len;
                }
                None => break,
            }
        }

        if offset == size {
            return Ok(());
        }

        if offset == 0 {
            let mut file = file;
            return copy_stream(&mut file, writer, size, None);
        }

        return Err(io::Error::other(
            "large-file mapping failed after a partial write",
        ));
    }

    let mut file = file;
    copy_stream(&mut file, writer, size, None)
}

/// Write a decompressed entry to disk. Small entries are written directly;
/// large ones reuse the thread-local copy buffer.
///
/// If the copy fails or is cancelled, the partly written file is removed so
/// a truncated file is never left looking complete.
pub(crate) fn copy_to_file<R: Read + ?Sized>(
    reader: &mut R,
    path: &Path,
    size: u64,
    progress: Option<&Progress>,
) -> io::Result<()> {
    if let Some(progress) = progress {
        progress.check()?;
    }
    let mut file = File::create(path)?;
    let result = copy_stream(reader, &mut file, size, progress);
    if result.is_err() {
        drop(file);
        let _ = std::fs::remove_file(path);
    }
    result
}

/// Write an entry that is already fully decoded in memory. Avoids a
/// `BufWriter` allocation and an extra copy for small-file extraction.
pub(crate) fn write_buffer(path: &Path, data: &[u8], progress: Option<&Progress>) -> io::Result<()> {
    if let Some(progress) = progress {
        progress.check()?;
    }
    let mut file = File::create(path)?;
    if let Err(err) = file.write_all(data) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(err);
    }
    if let Some(progress) = progress {
        progress.add(data.len() as u64);
    }
    Ok(())
}

/// Size of each block handed from a read-ahead thread to its consumer.
const READ_AHEAD_BLOCK: usize = 1024 * 1024;

/// Blocks a read-ahead thread may decode before the consumer catches up.
const READ_AHEAD_DEPTH: usize = 4;

/// Runs a (decompressing) reader on its own thread and hands decoded blocks
/// to the consumer through a bounded queue. Decompression then overlaps with
/// whatever the consumer does with the data — parsing TAR headers and
/// writing files — instead of alternating with it. At most
/// `READ_AHEAD_DEPTH + 2` blocks are alive at once.
pub(crate) struct ReadAhead {
    blocks: std::sync::mpsc::Receiver<io::Result<Vec<u8>>>,
    current: Vec<u8>,
    pos: usize,
    done: bool,
}

impl ReadAhead {
    pub(crate) fn new(mut inner: Box<dyn Read + Send>) -> io::Result<Self> {
        let (tx, blocks) = std::sync::mpsc::sync_channel(READ_AHEAD_DEPTH);
        std::thread::Builder::new()
            .name("openarchive-read-ahead".into())
            .spawn(move || {
                loop {
                    let mut block = vec![0u8; READ_AHEAD_BLOCK];
                    let mut filled = 0;
                    let result = loop {
                        match inner.read(&mut block[filled..]) {
                            Ok(0) => break Ok(()),
                            Ok(n) => {
                                filled += n;
                                if filled == block.len() {
                                    break Ok(());
                                }
                            }
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                            Err(e) => break Err(e),
                        }
                    };
                    let eof = result.is_ok() && filled < block.len();
                    if filled > 0 {
                        block.truncate(filled);
                        if tx.send(Ok(block)).is_err() {
                            return;
                        }
                    }
                    if let Err(e) = result {
                        let _ = tx.send(Err(e));
                        return;
                    }
                    if eof {
                        return;
                    }
                }
            })?;
        Ok(Self {
            blocks,
            current: Vec::new(),
            pos: 0,
            done: false,
        })
    }
}

impl Read for ReadAhead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.pos == self.current.len() {
            if self.done {
                return Ok(0);
            }
            match self.blocks.recv() {
                Ok(Ok(block)) => {
                    self.current = block;
                    self.pos = 0;
                }
                Ok(Err(e)) => {
                    self.done = true;
                    return Err(e);
                }
                // The thread exits after the last block (or an error).
                Err(_) => {
                    self.done = true;
                    return Ok(0);
                }
            }
        }
        let n = buf.len().min(self.current.len() - self.pos);
        buf[..n].copy_from_slice(&self.current[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_ahead_preserves_stream() {
        let data: Vec<u8> = (0..READ_AHEAD_BLOCK * 3 + 12_345)
            .map(|i| (i % 253) as u8)
            .collect();
        let mut reader = ReadAhead::new(Box::new(io::Cursor::new(data.clone()))).unwrap();
        let mut out = Vec::new();
        // Odd-sized reads cross block boundaries.
        let mut buf = [0u8; 7_777];
        loop {
            let n = reader.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        assert_eq!(out, data);
    }

    #[test]
    fn read_ahead_reports_errors() {
        struct Failing(usize);
        impl Read for Failing {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0 == 0 {
                    return Err(io::Error::other("corrupt stream"));
                }
                let n = buf.len().min(self.0);
                buf[..n].fill(1);
                self.0 -= n;
                Ok(n)
            }
        }
        let mut reader = ReadAhead::new(Box::new(Failing(10))).unwrap();
        let mut out = Vec::new();
        let err = reader.read_to_end(&mut out).unwrap_err();
        assert!(err.to_string().contains("corrupt stream"));
        assert_eq!(out, vec![1u8; 10]);
    }

    #[test]
    fn buffer_capacity_is_size_aware() {
        assert_eq!(read_buffer_capacity(0), SMALL_READ_BUFFER);
        assert_eq!(
            read_buffer_capacity(SMALL_FILE_THRESHOLD - 1),
            SMALL_READ_BUFFER
        );
        assert_eq!(read_buffer_capacity(SMALL_FILE_THRESHOLD), IO_BUFFER_SIZE);
    }

    #[test]
    fn mapped_file_reads_the_same_bytes() {
        let path = std::env::temp_dir().join(format!(
            "openarchive-mmap-test-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let payload: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &payload).unwrap();

        let file = File::open(&path).unwrap();
        let mut mapped =
            MappedFile::map_range(&file, 0, payload.len() as u64).expect("mapping should succeed");
        let mut read = Vec::new();
        mapped.read_to_end(&mut read).unwrap();

        assert_eq!(read, payload);
        let _ = std::fs::remove_file(&path);
    }

    /// Reads `remaining` bytes of 0xAB and cancels `progress` after
    /// `cancel_after` bytes have been handed out.
    struct CancellingReader<'a> {
        remaining: usize,
        handed_out: usize,
        cancel_after: usize,
        progress: &'a Progress,
    }

    impl Read for CancellingReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.remaining).min(64 * 1024);
            buf[..n].fill(0xAB);
            self.remaining -= n;
            self.handed_out += n;
            if self.handed_out >= self.cancel_after {
                self.progress.cancel();
            }
            Ok(n)
        }
    }

    fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("openarchive-io-{tag}-{}.bin", std::process::id()))
    }

    #[test]
    fn copy_to_file_reports_bytes() {
        let path = temp_path("count");
        let progress = Progress::new();
        let size = 3 * 1024 * 1024;
        let mut reader = CancellingReader { remaining: size, handed_out: 0, cancel_after: usize::MAX, progress: &progress };
        copy_to_file(&mut reader, &path, size as u64, Some(&progress)).unwrap();
        assert_eq!(progress.done(), size as u64);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size as u64);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cancelled_copy_stops_and_removes_the_partial_file() {
        let path = temp_path("cancel");
        let progress = Progress::new();
        let size = 8 * 1024 * 1024;
        let mut reader = CancellingReader { remaining: size, handed_out: 0, cancel_after: 1024 * 1024, progress: &progress };

        let err = copy_to_file(&mut reader, &path, size as u64, Some(&progress)).unwrap_err();
        assert!(err.get_ref().is_some_and(|inner| inner.is::<crate::core::Cancelled>()), "{err}");
        assert!(!path.exists(), "partial file must be removed");
        // It stopped soon after the cancel, not at the end.
        assert!(reader.handed_out < size, "copy ran to the end");
        assert!(progress.done() < size as u64);
    }

    #[test]
    fn small_and_buffered_writes_respect_cancellation() {
        let progress = Progress::new();
        progress.cancel();
        let path = temp_path("small");
        assert!(write_buffer(&path, b"data", Some(&progress)).is_err());
        assert!(copy_to_file(&mut &b"data"[..], &path, 4, Some(&progress)).is_err());
        assert!(!path.exists());
        assert!(drain(&mut &b"data"[..], Some(&progress)).is_err());
    }

    #[test]
    fn drain_counts_without_writing() {
        let progress = Progress::new();
        let data = vec![7u8; 300_000];
        assert_eq!(drain(&mut &data[..], Some(&progress)).unwrap(), 300_000);
        assert_eq!(progress.done(), 300_000);
    }
}
