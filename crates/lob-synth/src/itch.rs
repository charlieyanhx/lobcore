//! Nasdaq TotalView-ITCH 5.0 message writers with the emi.nasdaq.com BinaryFILE framing
//! (2-byte big-endian payload length before every message; the length is not part of the message).
//!
//! Layouts and sizes follow the v5.0 specification: every message is `type u8 | stock locate u16 |
//! tracking number u16 | timestamp u48 (ns since midnight) | body`, all integers big-endian, alpha
//! fields left-justified and space-padded, prices u32 with four implied decimals.

/// Spec payload length (excluding the 2-byte frame) for every ITCH 5.0 message type.
pub const SPEC_LEN: [(u8, usize); 22] = [
    (b'S', 12),
    (b'R', 39),
    (b'H', 25),
    (b'Y', 20),
    (b'L', 26),
    (b'V', 35),
    (b'W', 12),
    (b'K', 28),
    (b'J', 35),
    (b'h', 21),
    (b'A', 36),
    (b'F', 40),
    (b'E', 31),
    (b'C', 36),
    (b'X', 23),
    (b'D', 19),
    (b'U', 35),
    (b'P', 44),
    (b'Q', 40),
    (b'B', 19),
    (b'I', 50),
    (b'N', 20),
];

/// Payload length of a message type, `None` for an unknown type byte.
pub fn spec_len(msg_type: u8) -> Option<usize> {
    SPEC_LEN
        .iter()
        .find(|(t, _)| *t == msg_type)
        .map(|(_, n)| *n)
}

/// Largest ITCH price: $200,000.0000.
pub const MAX_PRICE: u32 = 2_000_000_000;

/// Message header shared by every type.
#[derive(Clone, Copy, Debug)]
pub struct Header {
    /// Stock locate (0 for system events).
    pub locate: u16,
    /// Tracking number.
    pub tracking: u16,
    /// Nanoseconds since midnight; must fit in 48 bits.
    pub ts: u64,
}

/// Appends one framed message: writes the 2-byte BE length, then the payload the closure builds.
struct Frame<'a> {
    out: &'a mut Vec<u8>,
    start: usize,
}

impl<'a> Frame<'a> {
    fn open(out: &'a mut Vec<u8>, msg_type: u8, h: &Header) -> Self {
        let start = out.len();
        out.extend_from_slice(&[0, 0]);
        out.push(msg_type);
        out.extend_from_slice(&h.locate.to_be_bytes());
        out.extend_from_slice(&h.tracking.to_be_bytes());
        debug_assert!(h.ts < (1u64 << 48), "timestamp does not fit u48");
        out.extend_from_slice(&h.ts.to_be_bytes()[2..8]);
        Self { out, start }
    }

    fn u8(&mut self, v: u8) -> &mut Self {
        self.out.push(v);
        self
    }

    fn u32(&mut self, v: u32) -> &mut Self {
        self.out.extend_from_slice(&v.to_be_bytes());
        self
    }

    fn u64(&mut self, v: u64) -> &mut Self {
        self.out.extend_from_slice(&v.to_be_bytes());
        self
    }

    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.out.extend_from_slice(v);
        self
    }

    fn close(self, msg_type: u8) {
        let len = self.out.len() - self.start - 2;
        debug_assert_eq!(
            Some(len),
            spec_len(msg_type),
            "size of '{}'",
            msg_type as char
        );
        let len = u16::try_from(len).expect("message length fits u16");
        self.out[self.start..self.start + 2].copy_from_slice(&len.to_be_bytes());
    }
}

/// Symbol as the 8-byte space-padded ITCH alpha field.
pub fn symbol8(sym: &str) -> [u8; 8] {
    let mut s = [b' '; 8];
    let b = sym.as_bytes();
    assert!(b.len() <= 8, "symbol longer than 8 bytes");
    s[..b.len()].copy_from_slice(b);
    s
}

/// S: System Event (12 B). `code` is one of O S Q M E C.
pub fn system_event(out: &mut Vec<u8>, h: &Header, code: u8) {
    let mut f = Frame::open(out, b'S', h);
    f.u8(code);
    f.close(b'S');
}

/// R: Stock Directory (39 B) for a synthetic common stock, round lot 100, LULD tier 1.
pub fn stock_directory(out: &mut Vec<u8>, h: &Header, stock: &[u8; 8]) {
    let mut f = Frame::open(out, b'R', h);
    f.bytes(stock)
        .u8(b'Q') // market category: Nasdaq Global Select
        .u8(b' ') // financial status: not applicable
        .u32(100) // round lot size
        .u8(b'N') // round lots only
        .u8(b'C') // issue classification: common stock
        .bytes(b"Z ") // issue sub-type: not applicable
        .u8(b'P') // authenticity: production
        .u8(b'N') // short-sale threshold
        .u8(b'N') // IPO flag
        .u8(b'1') // LULD reference price tier
        .u8(b'N') // ETP flag
        .u32(0) // ETP leverage factor
        .u8(b'N'); // inverse indicator
    f.close(b'R');
}

/// H: Stock Trading Action (25 B).
pub fn trading_action(out: &mut Vec<u8>, h: &Header, stock: &[u8; 8], state: u8) {
    let mut f = Frame::open(out, b'H', h);
    f.bytes(stock).u8(state).u8(b' ').bytes(b"    ");
    f.close(b'H');
}

/// L: Market Participant Position (26 B).
pub fn mpid_position(out: &mut Vec<u8>, h: &Header, mpid: &[u8; 4], stock: &[u8; 8]) {
    let mut f = Frame::open(out, b'L', h);
    f.bytes(mpid).bytes(stock).u8(b'Y').u8(b'N').u8(b'A');
    f.close(b'L');
}

/// A: Add Order, no MPID (36 B).
pub fn add_order(
    out: &mut Vec<u8>,
    h: &Header,
    r: u64,
    side: u8,
    qty: u32,
    stock: &[u8; 8],
    px: u32,
) {
    let mut f = Frame::open(out, b'A', h);
    f.u64(r).u8(side).u32(qty).bytes(stock).u32(px);
    f.close(b'A');
}

/// F: Add Order with MPID attribution (40 B).
#[allow(clippy::too_many_arguments)]
pub fn add_order_mpid(
    out: &mut Vec<u8>,
    h: &Header,
    r: u64,
    side: u8,
    qty: u32,
    stock: &[u8; 8],
    px: u32,
    mpid: &[u8; 4],
) {
    let mut f = Frame::open(out, b'F', h);
    f.u64(r).u8(side).u32(qty).bytes(stock).u32(px).bytes(mpid);
    f.close(b'F');
}

/// E: Order Executed (31 B).
pub fn order_executed(out: &mut Vec<u8>, h: &Header, r: u64, qty: u32, match_no: u64) {
    let mut f = Frame::open(out, b'E', h);
    f.u64(r).u32(qty).u64(match_no);
    f.close(b'E');
}

/// C: Order Executed With Price (36 B).
pub fn order_executed_with_price(
    out: &mut Vec<u8>,
    h: &Header,
    r: u64,
    qty: u32,
    match_no: u64,
    printable: u8,
    px: u32,
) {
    let mut f = Frame::open(out, b'C', h);
    f.u64(r).u32(qty).u64(match_no).u8(printable).u32(px);
    f.close(b'C');
}

/// X: Order Cancel, partial (23 B).
pub fn order_cancel(out: &mut Vec<u8>, h: &Header, r: u64, qty: u32) {
    let mut f = Frame::open(out, b'X', h);
    f.u64(r).u32(qty);
    f.close(b'X');
}

/// D: Order Delete (19 B).
pub fn order_delete(out: &mut Vec<u8>, h: &Header, r: u64) {
    let mut f = Frame::open(out, b'D', h);
    f.u64(r);
    f.close(b'D');
}

/// U: Order Replace (35 B).
pub fn order_replace(out: &mut Vec<u8>, h: &Header, old: u64, new: u64, qty: u32, px: u32) {
    let mut f = Frame::open(out, b'U', h);
    f.u64(old).u64(new).u32(qty).u32(px);
    f.close(b'U');
}

/// P: Trade Message, non-cross (44 B); ref 0 and side 'B' as on the live feed since 2014.
pub fn trade(out: &mut Vec<u8>, h: &Header, qty: u32, stock: &[u8; 8], px: u32, match_no: u64) {
    let mut f = Frame::open(out, b'P', h);
    f.u64(0)
        .u8(b'B')
        .u32(qty)
        .bytes(stock)
        .u32(px)
        .u64(match_no);
    f.close(b'P');
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle from the research report: framed Add Order, len 36, locate 7, tracking 3,
    /// ts 34,200,000,000,123 ns, ref 123456789, side B, 100 shares, SPY, price 453.1200.
    #[test]
    fn add_order_matches_the_hex_oracle() {
        let mut out = Vec::new();
        let h = Header {
            locate: 7,
            tracking: 3,
            ts: 34_200_000_000_123,
        };
        add_order(
            &mut out,
            &h,
            123_456_789,
            b'B',
            100,
            &symbol8("SPY"),
            4_531_200,
        );
        let hex: String = out.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "002441000700031f1aced9f07b00000000075bcd154200000064535059202020202000452400"
        );
    }

    #[test]
    fn every_writer_produces_its_spec_length() {
        let h = Header {
            locate: 1,
            tracking: 0,
            ts: 1,
        };
        let s = symbol8("SYN0001");
        let m = b"SYNM";
        type Writer<'a> = Box<dyn Fn(&mut Vec<u8>) + 'a>;
        let cases: Vec<(u8, Writer<'_>)> = vec![
            (b'S', Box::new(|o| system_event(o, &h, b'O'))),
            (b'R', Box::new(|o| stock_directory(o, &h, &s))),
            (b'H', Box::new(|o| trading_action(o, &h, &s, b'T'))),
            (b'L', Box::new(|o| mpid_position(o, &h, m, &s))),
            (b'A', Box::new(|o| add_order(o, &h, 1, b'B', 1, &s, 1))),
            (
                b'F',
                Box::new(|o| add_order_mpid(o, &h, 1, b'S', 1, &s, 1, m)),
            ),
            (b'E', Box::new(|o| order_executed(o, &h, 1, 1, 1))),
            (
                b'C',
                Box::new(|o| order_executed_with_price(o, &h, 1, 1, 1, b'Y', 1)),
            ),
            (b'X', Box::new(|o| order_cancel(o, &h, 1, 1))),
            (b'D', Box::new(|o| order_delete(o, &h, 1))),
            (b'U', Box::new(|o| order_replace(o, &h, 1, 2, 1, 1))),
            (b'P', Box::new(|o| trade(o, &h, 1, &s, 1, 1))),
        ];
        for (t, w) in cases {
            let mut out = Vec::new();
            w(&mut out);
            let len = u16::from_be_bytes([out[0], out[1]]) as usize;
            assert_eq!(len, spec_len(t).unwrap(), "type {}", t as char);
            assert_eq!(out.len(), len + 2);
            assert_eq!(out[2], t);
        }
        assert_eq!(spec_len(b'Z'), None);
    }

    #[test]
    fn timestamp_is_six_big_endian_bytes() {
        let mut out = Vec::new();
        let h = Header {
            locate: 0,
            tracking: 0,
            ts: 10_953_404_452_051,
        };
        system_event(&mut out, &h, b'O');
        // first message of the real sample: 000c 53 0000 0000 09f649c80cd3 4f
        assert_eq!(
            &out[..],
            &[
                0, 12, b'S', 0, 0, 0, 0, 0x09, 0xf6, 0x49, 0xc8, 0x0c, 0xd3, b'O'
            ]
        );
    }
}
