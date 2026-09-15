//! BinaryFILE framing as used by the emi.nasdaq.com sample files: every message is preceded by a
//! 2-byte big-endian payload length (the length is not part of the ITCH message layout).
//!
//! [`Framer`] streams from any `Read` through a fixed 4 MB buffer that is allocated once; a frame
//! that straddles the buffer end is completed by compacting the unconsumed tail to the front and
//! refilling. A final message cut short by EOF is discarded and counted in `truncated`. The time
//! spent inside the source's `read` calls is accumulated in `io_ns` so a replay can report
//! messages per second both including and excluding gunzip / file I/O.
//!
//! [`SliceFrames`] frames an in-memory byte slice with the same rules and no copying.

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;
use std::time::Instant;

use flate2::read::MultiGzDecoder;

/// Streaming buffer size: 4 MB (the largest frame is 2 + 65,535 bytes).
pub const BUF_LEN: usize = 4 << 20;

/// A source of framed ITCH payloads. Both framers implement it so [`crate::Session::replay`]
/// is generic (statically dispatched) over the two.
pub trait FrameSource {
    /// The next payload (without its 2-byte length), `None` at end of input.
    fn next_frame(&mut self) -> io::Result<Option<&[u8]>>;
    /// Frames returned so far.
    fn frames(&self) -> u64;
    /// Number of incomplete trailing messages discarded (0 or 1).
    fn truncated(&self) -> u32;
    /// Bytes consumed from the source so far, including a truncated tail.
    fn bytes_in(&self) -> u64;
    /// Nanoseconds spent inside the source's `read` (0 for an in-memory slice).
    fn io_ns(&self) -> u64;
    /// True when the source itself ended early (`UnexpectedEof`, e.g. a range-downloaded gzip
    /// prefix whose deflate stream is cut); the bytes decoded before the cut were framed
    /// normally and the partial last message counted as truncated.
    fn source_cut(&self) -> bool {
        false
    }
}

/// Streaming framer over a `Read`.
#[derive(Debug)]
pub struct Framer<R: Read> {
    src: R,
    buf: Vec<u8>,
    start: usize,
    end: usize,
    eof: bool,
    cut: bool,
    frames: u64,
    truncated: u32,
    bytes_in: u64,
    io_ns: u64,
}

impl<R: Read> Framer<R> {
    /// Wrap a reader; allocates the 4 MB buffer once.
    pub fn new(src: R) -> Framer<R> {
        Framer::with_buffer(src, BUF_LEN)
    }

    /// Wrap a reader with a caller-chosen buffer size (tests use small buffers to force
    /// frames across refills). Sizes below 2 + 65,535 are raised to that minimum.
    pub fn with_buffer(src: R, buf_len: usize) -> Framer<R> {
        let buf_len = buf_len.max(2 + u16::MAX as usize);
        Framer {
            src,
            buf: vec![0u8; buf_len],
            start: 0,
            end: 0,
            eof: false,
            cut: false,
            frames: 0,
            truncated: 0,
            bytes_in: 0,
            io_ns: 0,
        }
    }

    /// Unwrap the reader.
    pub fn into_inner(self) -> R {
        self.src
    }

    /// Compact the unconsumed tail to the front and read until the buffer is full or EOF.
    fn refill(&mut self) -> io::Result<()> {
        if self.start > 0 {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        let t0 = Instant::now();
        while self.end < self.buf.len() {
            match self.src.read(&mut self.buf[self.end..]) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(n) => {
                    self.end += n;
                    self.bytes_in += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    // a cut deflate stream: everything decoded so far is valid input
                    self.eof = true;
                    self.cut = true;
                    break;
                }
                Err(e) => {
                    self.io_ns += t0.elapsed().as_nanos() as u64;
                    return Err(e);
                }
            }
        }
        self.io_ns += t0.elapsed().as_nanos() as u64;
        Ok(())
    }
}

impl<R: Read> FrameSource for Framer<R> {
    fn next_frame(&mut self) -> io::Result<Option<&[u8]>> {
        loop {
            let avail = self.end - self.start;
            if avail >= 2 {
                let len =
                    u16::from_be_bytes([self.buf[self.start], self.buf[self.start + 1]]) as usize;
                if avail >= 2 + len {
                    let s = self.start + 2;
                    self.start = s + len;
                    self.frames += 1;
                    return Ok(Some(&self.buf[s..s + len]));
                }
            }
            if self.eof {
                if avail > 0 {
                    self.truncated += 1;
                    self.start = self.end;
                }
                return Ok(None);
            }
            self.refill()?;
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }

    fn truncated(&self) -> u32 {
        self.truncated
    }

    fn bytes_in(&self) -> u64 {
        self.bytes_in
    }

    fn io_ns(&self) -> u64 {
        self.io_ns
    }

    fn source_cut(&self) -> bool {
        self.cut
    }
}

/// Framer over an in-memory slice. Also an [`Iterator`] of payload slices borrowing the input.
#[derive(Debug, Clone)]
pub struct SliceFrames<'a> {
    data: &'a [u8],
    pos: usize,
    frames: u64,
    truncated: u32,
}

impl<'a> SliceFrames<'a> {
    /// Frame `data`.
    pub fn new(data: &'a [u8]) -> SliceFrames<'a> {
        SliceFrames {
            data,
            pos: 0,
            frames: 0,
            truncated: 0,
        }
    }

    /// Rewind to the first frame.
    pub fn reset(&mut self) {
        self.pos = 0;
        self.frames = 0;
        self.truncated = 0;
    }

    fn advance(&mut self) -> Option<&'a [u8]> {
        let avail = self.data.len() - self.pos;
        if avail >= 2 {
            let len = u16::from_be_bytes([self.data[self.pos], self.data[self.pos + 1]]) as usize;
            if avail >= 2 + len {
                let s = self.pos + 2;
                self.pos = s + len;
                self.frames += 1;
                return Some(&self.data[s..s + len]);
            }
        }
        if avail > 0 {
            self.truncated += 1;
            self.pos = self.data.len();
        }
        None
    }
}

impl<'a> Iterator for SliceFrames<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        self.advance()
    }
}

impl FrameSource for SliceFrames<'_> {
    fn next_frame(&mut self) -> io::Result<Option<&[u8]>> {
        Ok(self.advance())
    }

    fn frames(&self) -> u64 {
        self.frames
    }

    fn truncated(&self) -> u32 {
        self.truncated
    }

    fn bytes_in(&self) -> u64 {
        self.pos as u64
    }

    fn io_ns(&self) -> u64 {
        0
    }
}

/// Open a file for framing: gzip (multi-member, flate2 with the zlib-rs backend) when the path
/// ends in `.gz`, plain bytes otherwise.
pub fn open(path: impl AsRef<Path>) -> io::Result<Framer<Box<dyn Read>>> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let is_gz = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gz"));
    let src: Box<dyn Read> = if is_gz {
        Box::new(MultiGzDecoder::new(BufReader::with_capacity(1 << 20, file)))
    } else {
        Box::new(file)
    };
    Ok(Framer::new(src))
}

/// Read a whole file into memory (inflating `.gz`), for in-memory benches. A gzip member cut
/// mid-stream yields the bytes decoded before the cut; the flag says so.
pub fn read_all(path: impl AsRef<Path>) -> io::Result<(Vec<u8>, bool)> {
    let mut f = open(path)?;
    let mut out = Vec::new();
    let mut cut = false;
    loop {
        let start = out.len();
        out.resize(start + (1 << 20), 0);
        match f.src.read(&mut out[start..]) {
            Ok(0) => {
                out.truncate(start);
                break;
            }
            Ok(n) => out.truncate(start + n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => out.truncate(start),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                out.truncate(start);
                cut = true;
                break;
            }
            Err(e) => return Err(e),
        }
    }
    Ok((out, cut))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn framed(payloads: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for p in payloads {
            out.extend_from_slice(&(p.len() as u16).to_be_bytes());
            out.extend_from_slice(p);
        }
        out
    }

    #[test]
    fn slice_frames_and_truncated_tail() {
        let mut bytes = framed(&[b"abc", b"", b"defgh"]);
        bytes.extend_from_slice(&[0, 9, b'x']); // announced 9, only 1 byte present
        let mut f = SliceFrames::new(&bytes);
        assert_eq!(f.next(), Some(&b"abc"[..]));
        assert_eq!(f.next(), Some(&b""[..]));
        assert_eq!(f.next(), Some(&b"defgh"[..]));
        assert_eq!(f.next(), None);
        assert_eq!(f.frames(), 3);
        assert_eq!(f.truncated(), 1);
        assert_eq!(f.bytes_in(), bytes.len() as u64);
        let mut g = SliceFrames::new(&bytes[..bytes.len() - 3]);
        assert_eq!(g.by_ref().count(), 3);
        assert_eq!(g.truncated(), 0);
    }

    #[test]
    fn stream_frames_across_refills_match_slice_frames() {
        // 3,000 frames of varying length through a minimum-size buffer: every refill compacts
        // a partial frame to the front.
        let payloads: Vec<Vec<u8>> = (0..3000u32)
            .map(|i| vec![(i % 251) as u8; (i * 37 % 1200) as usize])
            .collect();
        let refs: Vec<&[u8]> = payloads.iter().map(|v| v.as_slice()).collect();
        let mut bytes = framed(&refs);
        bytes.extend_from_slice(&[0, 5, 1, 2]); // truncated tail
        let mut stream = Framer::with_buffer(&bytes[..], 1);
        let mut slice = SliceFrames::new(&bytes);
        let mut n = 0;
        while let Some(p) = stream.next_frame().unwrap() {
            assert_eq!(Some(p), slice.next());
            n += 1;
        }
        assert_eq!(slice.next(), None);
        assert_eq!(n, 3000);
        assert_eq!(stream.frames(), 3000);
        assert_eq!(stream.truncated(), 1);
        assert_eq!(stream.bytes_in(), bytes.len() as u64);
        assert_eq!(slice.truncated(), 1);
    }

    #[test]
    fn single_dangling_length_byte_is_truncated() {
        let bytes = [0u8];
        let mut f = Framer::new(&bytes[..]);
        assert_eq!(f.next_frame().unwrap(), None);
        assert_eq!(f.truncated(), 1);
        let mut e = Framer::new(&[][..]);
        assert_eq!(e.next_frame().unwrap(), None);
        assert_eq!(e.truncated(), 0);
    }

    #[test]
    fn open_reads_gz_and_plain() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let bytes = framed(&[b"hello", b"world"]);
        let dir = std::env::temp_dir().join(format!("lob-feed-frame-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("x.itch");
        let gz = dir.join("x.itch.gz");
        std::fs::write(&plain, &bytes).unwrap();
        let mut enc = GzEncoder::new(File::create(&gz).unwrap(), Compression::fast());
        enc.write_all(&bytes).unwrap();
        enc.finish().unwrap();
        for p in [&plain, &gz] {
            let mut f = open(p).unwrap();
            assert_eq!(f.next_frame().unwrap(), Some(&b"hello"[..]));
            assert_eq!(f.next_frame().unwrap(), Some(&b"world"[..]));
            assert_eq!(f.next_frame().unwrap(), None);
            assert_eq!(f.bytes_in(), bytes.len() as u64);
            assert_eq!(read_all(p).unwrap(), (bytes.clone(), false));
        }
        // a gzip member cut mid-stream (a range-downloaded prefix) ends the input instead of
        // failing it, and is reported
        let full = std::fs::read(&gz).unwrap();
        let cut = dir.join("cut.itch.gz");
        std::fs::write(&cut, &full[..full.len() - 6]).unwrap();
        let mut f = open(&cut).unwrap();
        let mut n = 0;
        while f.next_frame().unwrap().is_some() {
            n += 1;
        }
        assert!(f.source_cut());
        assert!(n <= 2);
        assert!(read_all(&cut).unwrap().1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
