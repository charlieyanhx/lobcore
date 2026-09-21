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
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;
use std::time::Instant;

use flate2::bufread::GzDecoder;

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
    /// Bytes after the last gzip member that were not another member (padding, a concatenated
    /// non-gzip file, garbage); they end the input and are counted, as `gzip -dc` warns and
    /// ignores them. Always 0 for a plain source.
    fn trailing_bytes(&self) -> u64 {
        0
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
    trailing: u64,
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
            trailing: 0,
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
                Err(e) if e.get_ref().is_some_and(|i| i.is::<TrailingBytes>()) => {
                    // non-member bytes after the last gzip member: end of input, counted
                    self.trailing = e
                        .into_inner()
                        .and_then(|i| i.downcast::<TrailingBytes>().ok())
                        .map_or(0, |t| t.0);
                    self.eof = true;
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

    fn trailing_bytes(&self) -> u64 {
        self.trailing
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

/// The two gzip magic bytes (RFC 1952).
const GZ_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// The payload of the `io::Error` a [`GzMembers`] returns when the bytes after the last member
/// are not another member: how many such bytes there were. [`Framer::refill`] turns it into
/// end-of-input plus `trailing_bytes()`; any other consumer sees an ordinary error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrailingBytes(u64);

impl std::fmt::Display for TrailingBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} trailing bytes after the last gzip member", self.0)
    }
}

impl std::error::Error for TrailingBytes {}

/// gzip members decoded one after another (like `MultiGzDecoder`), except that bytes after a
/// member that do not start another member end the stream with a [`TrailingBytes`] error
/// instead of `invalid gzip header`, so the members already decoded are not lost. A member cut
/// mid-stream still surfaces as `UnexpectedEof` (the range-downloaded prefix case).
struct GzMembers {
    dec: Option<GzDecoder<Box<dyn BufRead>>>,
}

impl GzMembers {
    fn new(src: Box<dyn BufRead>) -> GzMembers {
        GzMembers {
            dec: Some(GzDecoder::new(src)),
        }
    }

    /// After a member ended: start the next one, or count what follows and stop.
    fn next_member(&mut self) -> io::Result<bool> {
        let dec = self.dec.take().expect("decoder present");
        let mut src = dec.into_inner();
        let head = src.fill_buf()?;
        if head.is_empty() {
            return Ok(false);
        }
        if head.len() >= 2 {
            if head[..2] == GZ_MAGIC {
                self.dec = Some(GzDecoder::new(src));
                return Ok(true);
            }
            return Err(Self::drain(src, 0));
        }
        // one buffered byte: consume it, look at the next, and if together they are the magic
        // put the consumed byte back in front of the reader
        let first = head[0];
        src.consume(1);
        let second = src.fill_buf()?.first().copied();
        if first == GZ_MAGIC[0] && second == Some(GZ_MAGIC[1]) {
            let src: Box<dyn BufRead> = Box::new(io::Cursor::new([first]).chain(src));
            self.dec = Some(GzDecoder::new(src));
            return Ok(true);
        }
        Err(Self::drain(src, 1))
    }

    /// Count the rest of the input (already `taken` bytes consumed) into a `TrailingBytes` error.
    fn drain(mut src: Box<dyn BufRead>, taken: u64) -> io::Error {
        let mut n = taken;
        loop {
            match src.fill_buf() {
                Ok([]) => break,
                Ok(b) => {
                    let k = b.len();
                    n += k as u64;
                    src.consume(k);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return e,
            }
        }
        io::Error::other(TrailingBytes(n))
    }
}

impl Read for GzMembers {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            let Some(dec) = self.dec.as_mut() else {
                return Ok(0);
            };
            match dec.read(out)? {
                0 => {
                    if !self.next_member()? {
                        return Ok(0);
                    }
                }
                n => return Ok(n),
            }
        }
    }
}

/// Open a file for framing: gzip (every member, flate2 with the zlib-rs backend) when the
/// file starts with the gzip magic `1f 8b`, plain bytes otherwise; the extension is not
/// consulted, so a gzip file named `.itch` and a plain file named `.gz` both frame correctly.
pub fn open(path: impl AsRef<Path>) -> io::Result<Framer<Box<dyn Read>>> {
    let path = path.as_ref();
    let mut file = BufReader::with_capacity(1 << 20, File::open(path)?);
    let is_gz = file.fill_buf()?.starts_with(&GZ_MAGIC);
    let src: Box<dyn Read> = if is_gz {
        Box::new(GzMembers::new(Box::new(file)))
    } else {
        Box::new(file)
    };
    Ok(Framer::new(src))
}

/// Read a whole file into memory (inflating gzip), for in-memory benches. A gzip member cut
/// mid-stream yields the bytes decoded before the cut; the flag says so. Bytes after the last
/// member that are not another member are ignored, as in [`open`].
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
            Err(e) if e.get_ref().is_some_and(|i| i.is::<TrailingBytes>()) => {
                out.truncate(start);
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

    fn gz_bytes(payload: &[u8]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(payload).unwrap();
        enc.finish().unwrap()
    }

    /// Frames, truncated count, `source_cut`, `trailing_bytes` of `path`, or the error text.
    type Framed = (Vec<Vec<u8>>, u32, bool, u64);

    fn frames_of(path: &Path) -> Result<Framed, String> {
        let mut f = open(path).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        loop {
            match f.next_frame() {
                Ok(Some(p)) => out.push(p.to_vec()),
                Ok(None) => break,
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok((out, f.truncated(), f.source_cut(), f.trailing_bytes()))
    }

    #[test]
    fn open_sniffs_the_gzip_magic_and_tolerates_trailing_bytes() {
        let a = framed(&[b"hello", b"world"]);
        let b = framed(&[b"second", b"member"]);
        let want_a = vec![b"hello".to_vec(), b"world".to_vec()];
        let want_ab = [want_a.clone(), vec![b"second".to_vec(), b"member".to_vec()]].concat();
        let dir = std::env::temp_dir().join(format!("lob-feed-sniff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        // the extension is not consulted: plain bytes named .gz, gzip named .itch
        assert_eq!(
            frames_of(&write("plain.gz", &a)).unwrap(),
            (want_a.clone(), 0, false, 0)
        );
        assert_eq!(
            frames_of(&write("gz.itch", &gz_bytes(&a))).unwrap(),
            (want_a.clone(), 0, false, 0)
        );
        // two members concatenated (pigz / cat a.gz b.gz) decode as one stream
        let two = [gz_bytes(&a), gz_bytes(&b)].concat();
        assert_eq!(
            frames_of(&write("two.gz", &two)).unwrap(),
            (want_ab.clone(), 0, false, 0)
        );
        // non-member bytes after the last member end the input, counted, nothing lost
        let trail = [gz_bytes(&a), b"not a gzip member".to_vec()].concat();
        assert_eq!(
            frames_of(&write("trail.gz", &trail)).unwrap(),
            (want_a.clone(), 0, false, 17)
        );
        let two_trail = [two.clone(), vec![0u8; 3]].concat();
        assert_eq!(
            frames_of(&write("two_trail.gz", &two_trail)).unwrap(),
            (want_ab.clone(), 0, false, 3)
        );
        // a single 0x1f byte after a member is trailing garbage, not a member
        let lone = [gz_bytes(&a), vec![0x1f]].concat();
        assert_eq!(
            frames_of(&write("lone.gz", &lone)).unwrap(),
            (want_a.clone(), 0, false, 1)
        );
        // a member following a large one still decodes, and a member cut mid-stream is `source_cut`
        let mut big = framed(&[&vec![7u8; 4096][..]]);
        for i in 0..2_000u32 {
            big.extend_from_slice(&framed(&[&i.to_be_bytes()[..]]));
        }
        let big_two = [gz_bytes(&big), gz_bytes(&a)].concat();
        let (frames, t, c, tr) = frames_of(&write("big_two.gz", &big_two)).unwrap();
        assert_eq!((frames.len(), t, c, tr), (2_003, 0, false, 0));
        assert_eq!(frames[2001..], want_a[..]);
        let full = gz_bytes(&big);
        let (_, _, c, tr) = frames_of(&write("cut.gz", &full[..full.len() / 2])).unwrap();
        assert!(c);
        assert_eq!(tr, 0);
        // an empty file is an empty plain input, not a cut gzip stream
        assert_eq!(
            frames_of(&write("empty.gz", b"")).unwrap(),
            (vec![], 0, false, 0)
        );
        assert_eq!(
            read_all(write("trail2.gz", &trail)).unwrap(),
            (a.clone(), false)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gz_members_peek_across_a_read_buffer_boundary() {
        // the read buffer ends exactly one byte into the second member's magic: the peek must
        // consume that byte, look at the next, and put it back in front of the decoder
        let a = framed(&[b"hello", b"world"]);
        let b = framed(&[b"second", b"member"]);
        let (ga, gb) = (gz_bytes(&a), gz_bytes(&b));
        let two = [ga.clone(), gb.clone()].concat();
        for cap in [ga.len() + 1, ga.len() + 2, ga.len(), 16] {
            let src: Box<dyn BufRead> =
                Box::new(BufReader::with_capacity(cap, io::Cursor::new(two.clone())));
            let mut out = Vec::new();
            GzMembers::new(src).read_to_end(&mut out).unwrap();
            assert_eq!(out, [a.clone(), b.clone()].concat(), "capacity {cap}");
        }
        // same boundary, but the byte after the first member is a lone 0x1f followed by garbage
        let junk = [ga.clone(), vec![0x1f, 0x00, 0x00]].concat();
        let src: Box<dyn BufRead> = Box::new(BufReader::with_capacity(
            ga.len() + 1,
            io::Cursor::new(junk.clone()),
        ));
        let mut out = Vec::new();
        let err = GzMembers::new(src).read_to_end(&mut out).unwrap_err();
        assert_eq!(out, a);
        assert_eq!(
            err.to_string(),
            "3 trailing bytes after the last gzip member"
        );
        let mut f = Framer::new(GzMembers::new(Box::new(BufReader::with_capacity(
            ga.len() + 1,
            io::Cursor::new(junk),
        ))));
        let mut n = 0;
        while f.next_frame().unwrap().is_some() {
            n += 1;
        }
        assert_eq!((n, f.trailing_bytes(), f.source_cut()), (2, 3, false));
    }
}
