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
}
