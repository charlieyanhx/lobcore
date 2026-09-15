//! Borrowed views over Nasdaq TotalView-ITCH 5.0 payloads (the bytes after the 2-byte frame).
//!
//! Every message is `type u8 @0 | stock locate u16 @1 | tracking number u16 @3 | timestamp u48 @5
//! (ns since midnight) | body @11`; all integers big-endian, alpha fields left-justified and
//! space-padded, `Price(4)` a u32 with four implied decimals (max 200,000.0000 = 2,000,000,000).
//! A view is a `&[u8]` of exactly the spec length; every accessor is a fixed-offset
//! `from_be_bytes` on a sub-slice, so decoding copies fields but allocates nothing.
//!
//! [`parse`] checks the payload length against the size table for all 23 types and returns
//! [`ParseError::Length`] on a mismatch (the caller stops; there is no resynchronisation). A type
//! byte outside the table yields [`Msg::Unknown`] so the caller can skip it by its framed length
//! and count it.

use lob_core::Side;

/// Spec payload length (excluding the frame) of every ITCH 5.0 message type.
pub const SPEC_LEN: [(u8, usize); 23] = [
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
    (b'O', 48),
];

/// Payload length of a message type; `None` for a type byte outside the table.
#[inline]
pub const fn spec_len(ty: u8) -> Option<usize> {
    Some(match ty {
        b'S' => 12,
        b'R' => 39,
        b'H' => 25,
        b'Y' => 20,
        b'L' => 26,
        b'V' => 35,
        b'W' => 12,
        b'K' => 28,
        b'J' => 35,
        b'h' => 21,
        b'A' => 36,
        b'F' => 40,
        b'E' => 31,
        b'C' => 36,
        b'X' => 23,
        b'D' => 19,
        b'U' => 35,
        b'P' => 44,
        b'Q' => 40,
        b'B' => 19,
        b'I' => 50,
        b'N' => 20,
        b'O' => 48,
        _ => return None,
    })
}

/// Length of the common header (type, locate, tracking, timestamp).
pub const HEADER_LEN: usize = 11;

/// Largest ITCH `Price(4)`: $200,000.0000.
pub const MAX_PRICE: u32 = 2_000_000_000;

/// Fatal framing / layout errors. The parser does not resynchronise after either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// A zero-length frame (no type byte).
    Empty,
    /// The framed length of a known type differs from the spec table.
    Length { ty: u8, got: usize },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Empty => write!(f, "empty frame"),
            ParseError::Length { ty, got } => write!(
                f,
                "message type '{}' (0x{ty:02x}) framed with {got} bytes, spec says {}",
                if ty.is_ascii_graphic() {
                    *ty as char
                } else {
                    '?'
                },
                spec_len(*ty).unwrap_or(0)
            ),
        }
    }
}

impl std::error::Error for ParseError {}

#[inline]
fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

#[inline]
fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

#[inline]
fn be64(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_be_bytes(a)
}

/// 6-byte big-endian timestamp, ns since midnight.
#[inline]
pub fn u48(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a[2..].copy_from_slice(&b[at..at + 6]);
    u64::from_be_bytes(a)
}

#[inline]
fn alpha8(b: &[u8], at: usize) -> [u8; 8] {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    a
}

#[inline]
fn alpha4(b: &[u8], at: usize) -> [u8; 4] {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[at..at + 4]);
    a
}

/// ITCH side byte to a book side (`'B'` buy, `'S'` sell).
#[inline]
pub const fn side_of(b: u8) -> Option<Side> {
    match b {
        b'B' => Some(Side::Bid),
        b'S' => Some(Side::Ask),
        _ => None,
    }
}

macro_rules! view {
    ($(#[$doc:meta])* $name:ident, $ty:expr, $len:expr) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name<'a>(&'a [u8]);

        impl<'a> $name<'a> {
            /// Message type byte.
            pub const TYPE: u8 = $ty;
            /// Spec payload length.
            pub const LEN: usize = $len;

            /// View a payload; `None` unless it is exactly `LEN` bytes of type `TYPE`.
            #[inline]
            pub fn new(payload: &'a [u8]) -> Option<$name<'a>> {
                (payload.len() == $len && payload[0] == $ty).then_some($name(payload))
            }

            /// The underlying bytes.
            #[inline]
            pub fn raw(&self) -> &'a [u8] {
                self.0
            }

            /// Stock locate.
            #[inline]
            pub fn locate(&self) -> u16 {
                be16(self.0, 1)
            }

            /// Tracking number.
            #[inline]
            pub fn tracking(&self) -> u16 {
                be16(self.0, 3)
            }

            /// Timestamp, ns since midnight (48-bit).
            #[inline]
            pub fn ts(&self) -> u64 {
                u48(self.0, 5)
            }
        }
    };
}

view!(
    /// `S` System Event (12 B): event code `O S Q M E C` at 11.
    SystemEvent, b'S', 12
);
impl SystemEvent<'_> {
    /// `'O'` start of messages, `'S'` start of system hours, `'Q'` start of market hours,
    /// `'M'` end of market hours, `'E'` end of system hours, `'C'` end of messages.
    #[inline]
    pub fn event_code(&self) -> u8 {
        self.0[11]
    }
}

view!(
    /// `R` Stock Directory (39 B).
    StockDirectory, b'R', 39
);
impl StockDirectory<'_> {
    /// Stock symbol, space padded.
    #[inline]
    pub fn stock(&self) -> [u8; 8] {
        alpha8(self.0, 11)
    }
    /// Market category at 19.
    #[inline]
    pub fn market_category(&self) -> u8 {
        self.0[19]
    }
    /// Financial status indicator at 20.
    #[inline]
    pub fn financial_status(&self) -> u8 {
        self.0[20]
    }
    /// Round lot size at 21.
    #[inline]
    pub fn round_lot_size(&self) -> u32 {
        be32(self.0, 21)
    }
    /// Round lots only at 25.
    #[inline]
    pub fn round_lots_only(&self) -> u8 {
        self.0[25]
    }
    /// Issue classification at 26.
    #[inline]
    pub fn issue_classification(&self) -> u8 {
        self.0[26]
    }
    /// Issue sub-type at 27.
    #[inline]
    pub fn issue_subtype(&self) -> [u8; 2] {
        [self.0[27], self.0[28]]
    }
    /// Authenticity at 29.
    #[inline]
    pub fn authenticity(&self) -> u8 {
        self.0[29]
    }
    /// Short sale threshold indicator at 30.
    #[inline]
    pub fn short_sale_threshold(&self) -> u8 {
        self.0[30]
    }
    /// IPO flag at 31.
    #[inline]
    pub fn ipo_flag(&self) -> u8 {
        self.0[31]
    }
    /// LULD reference price tier at 32.
    #[inline]
    pub fn luld_tier(&self) -> u8 {
        self.0[32]
    }
    /// ETP flag at 33.
    #[inline]
    pub fn etp_flag(&self) -> u8 {
        self.0[33]
    }
    /// ETP leverage factor at 34.
    #[inline]
    pub fn etp_leverage(&self) -> u32 {
        be32(self.0, 34)
    }
    /// Inverse indicator at 38.
    #[inline]
    pub fn inverse(&self) -> u8 {
        self.0[38]
    }
}

view!(
    /// `H` Stock Trading Action (25 B).
    TradingAction, b'H', 25
);
impl TradingAction<'_> {
    /// Stock symbol.
    #[inline]
    pub fn stock(&self) -> [u8; 8] {
        alpha8(self.0, 11)
    }
    /// `'H'` halted, `'P'` paused, `'Q'` quotation only, `'T'` trading.
    #[inline]
    pub fn trading_state(&self) -> u8 {
        self.0[19]
    }
    /// Reserved byte at 20.
    #[inline]
    pub fn reserved(&self) -> u8 {
        self.0[20]
    }
    /// Reason code at 21.
    #[inline]
    pub fn reason(&self) -> [u8; 4] {
        alpha4(self.0, 21)
    }
}

view!(
    /// `A` Add Order, no MPID (36 B).
    AddOrder, b'A', 36
);
impl AddOrder<'_> {
    /// Order reference number at 11.
    #[inline]
    pub fn order_ref(&self) -> u64 {
        be64(self.0, 11)
    }
    /// Buy/sell indicator byte at 19.
    #[inline]
    pub fn side_byte(&self) -> u8 {
        self.0[19]
    }
    /// Book side; `None` for a byte other than `'B'` / `'S'`.
    #[inline]
    pub fn side(&self) -> Option<Side> {
        side_of(self.0[19])
    }
    /// Shares at 20.
    #[inline]
    pub fn shares(&self) -> u32 {
        be32(self.0, 20)
    }
    /// Stock symbol at 24.
    #[inline]
    pub fn stock(&self) -> [u8; 8] {
        alpha8(self.0, 24)
    }
    /// Price(4) at 32.
    #[inline]
    pub fn price(&self) -> u32 {
        be32(self.0, 32)
    }
}

view!(
    /// `F` Add Order with MPID attribution (40 B).
    AddOrderMpid, b'F', 40
);
impl AddOrderMpid<'_> {
    /// Order reference number at 11.
    #[inline]
    pub fn order_ref(&self) -> u64 {
        be64(self.0, 11)
    }
    /// Buy/sell indicator byte at 19.
    #[inline]
    pub fn side_byte(&self) -> u8 {
        self.0[19]
    }
    /// Book side; `None` for a byte other than `'B'` / `'S'`.
    #[inline]
    pub fn side(&self) -> Option<Side> {
        side_of(self.0[19])
    }
    /// Shares at 20.
    #[inline]
    pub fn shares(&self) -> u32 {
        be32(self.0, 20)
    }
    /// Stock symbol at 24.
    #[inline]
    pub fn stock(&self) -> [u8; 8] {
        alpha8(self.0, 24)
    }
    /// Price(4) at 32.
    #[inline]
    pub fn price(&self) -> u32 {
        be32(self.0, 32)
    }
    /// Market participant id at 36.
    #[inline]
    pub fn attribution(&self) -> [u8; 4] {
        alpha4(self.0, 36)
    }
}

view!(
    /// `E` Order Executed (31 B): reduces the order by id at its display price.
    OrderExecuted, b'E', 31
);
impl OrderExecuted<'_> {
    /// Order reference number at 11.
    #[inline]
    pub fn order_ref(&self) -> u64 {
        be64(self.0, 11)
    }
    /// Executed shares at 19.
    #[inline]
    pub fn executed(&self) -> u32 {
        be32(self.0, 19)
    }
    /// Match number at 23.
    #[inline]
    pub fn match_number(&self) -> u64 {
        be64(self.0, 23)
    }
}

view!(
    /// `C` Order Executed With Price (36 B): identical to `E` for book state; `printable`
    /// only governs time-and-sales.
    OrderExecutedWithPrice, b'C', 36
);
impl OrderExecutedWithPrice<'_> {
    /// Order reference number at 11.
    #[inline]
    pub fn order_ref(&self) -> u64 {
        be64(self.0, 11)
    }
    /// Executed shares at 19.
    #[inline]
    pub fn executed(&self) -> u32 {
        be32(self.0, 19)
    }
    /// Match number at 23.
    #[inline]
    pub fn match_number(&self) -> u64 {
        be64(self.0, 23)
    }
    /// `'Y'` / `'N'` at 31.
    #[inline]
    pub fn printable(&self) -> u8 {
        self.0[31]
    }
    /// Execution Price(4) at 32.
    #[inline]
    pub fn execution_price(&self) -> u32 {
        be32(self.0, 32)
    }
}

view!(
    /// `X` Order Cancel (23 B): partial reduce in place, priority kept.
    OrderCancel, b'X', 23
);
impl OrderCancel<'_> {
    /// Order reference number at 11.
    #[inline]
    pub fn order_ref(&self) -> u64 {
        be64(self.0, 11)
    }
    /// Cancelled shares at 19.
    #[inline]
    pub fn cancelled(&self) -> u32 {
        be32(self.0, 19)
    }
}

view!(
    /// `D` Order Delete (19 B).
    OrderDelete, b'D', 19
);
impl OrderDelete<'_> {
    /// Order reference number at 11.
    #[inline]
    pub fn order_ref(&self) -> u64 {
        be64(self.0, 11)
    }
}

view!(
    /// `U` Order Replace (35 B): the original is removed and the new reference rests at the
    /// tail of its level (spec 1.4.5); side, stock and MPID are carried from the original.
    OrderReplace, b'U', 35
);
impl OrderReplace<'_> {
    /// Original order reference number at 11.
    #[inline]
    pub fn original_ref(&self) -> u64 {
        be64(self.0, 11)
    }
    /// New order reference number at 19.
    #[inline]
    pub fn new_ref(&self) -> u64 {
        be64(self.0, 19)
    }
    /// Shares at 27.
    #[inline]
    pub fn shares(&self) -> u32 {
        be32(self.0, 27)
    }
    /// Price(4) at 31.
    #[inline]
    pub fn price(&self) -> u32 {
        be32(self.0, 31)
    }
}

view!(
    /// `P` Trade, non-cross (44 B): never touches the book.
    Trade, b'P', 44
);
impl Trade<'_> {
    /// Order reference number at 11 (zero since 2010-12-06).
    #[inline]
    pub fn order_ref(&self) -> u64 {
        be64(self.0, 11)
    }
    /// Side byte at 19 (always `'B'` since 2014-07-14).
    #[inline]
    pub fn side_byte(&self) -> u8 {
        self.0[19]
    }
    /// Shares at 20.
    #[inline]
    pub fn shares(&self) -> u32 {
        be32(self.0, 20)
    }
    /// Stock symbol at 24.
    #[inline]
    pub fn stock(&self) -> [u8; 8] {
        alpha8(self.0, 24)
    }
    /// Price(4) at 32.
    #[inline]
    pub fn price(&self) -> u32 {
        be32(self.0, 32)
    }
    /// Match number at 36.
    #[inline]
    pub fn match_number(&self) -> u64 {
        be64(self.0, 36)
    }
}

view!(
    /// `Q` Cross Trade (40 B): bulk print, no book change.
    CrossTrade, b'Q', 40
);
impl CrossTrade<'_> {
    /// Shares (u64) at 11.
    #[inline]
    pub fn shares(&self) -> u64 {
        be64(self.0, 11)
    }
    /// Stock symbol at 19.
    #[inline]
    pub fn stock(&self) -> [u8; 8] {
        alpha8(self.0, 19)
    }
    /// Cross Price(4) at 27.
    #[inline]
    pub fn cross_price(&self) -> u32 {
        be32(self.0, 27)
    }
    /// Match number at 31.
    #[inline]
    pub fn match_number(&self) -> u64 {
        be64(self.0, 31)
    }
    /// `'O'` opening, `'C'` closing, `'H'` halt/IPO cross at 39.
    #[inline]
    pub fn cross_type(&self) -> u8 {
        self.0[39]
    }
}

view!(
    /// `B` Broken Trade (19 B): no impact on the book.
    BrokenTrade, b'B', 19
);
impl BrokenTrade<'_> {
    /// Match number at 11.
    #[inline]
    pub fn match_number(&self) -> u64 {
        be64(self.0, 11)
    }
}

/// A message the session does not decode beyond its header: a known type without a view
/// (`Y L V W K J h I N O`) or a type byte outside the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawMsg<'a>(&'a [u8]);

impl<'a> RawMsg<'a> {
    /// The bytes.
    #[inline]
    pub fn raw(&self) -> &'a [u8] {
        self.0
    }
    /// Type byte.
    #[inline]
    pub fn ty(&self) -> u8 {
        self.0[0]
    }
    /// Stock locate, if the payload carries a full header.
    #[inline]
    pub fn locate(&self) -> Option<u16> {
        (self.0.len() >= HEADER_LEN).then(|| be16(self.0, 1))
    }
    /// Timestamp, if the payload carries a full header.
    #[inline]
    pub fn ts(&self) -> Option<u64> {
        (self.0.len() >= HEADER_LEN).then(|| u48(self.0, 5))
    }
}

/// One parsed payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msg<'a> {
    System(SystemEvent<'a>),
    Directory(StockDirectory<'a>),
    Action(TradingAction<'a>),
    Add(AddOrder<'a>),
    AddMpid(AddOrderMpid<'a>),
    Exec(OrderExecuted<'a>),
    ExecPx(OrderExecutedWithPrice<'a>),
    Cancel(OrderCancel<'a>),
    Delete(OrderDelete<'a>),
    Replace(OrderReplace<'a>),
    Trade(Trade<'a>),
    Cross(CrossTrade<'a>),
    Broken(BrokenTrade<'a>),
    /// A type in the size table with no dedicated view (length already checked).
    Other(RawMsg<'a>),
    /// A type byte outside the size table (skipped by its framed length).
    Unknown(RawMsg<'a>),
}

impl<'a> Msg<'a> {
    /// Type byte.
    #[inline]
    pub fn ty(&self) -> u8 {
        self.raw()[0]
    }

    /// The payload bytes.
    #[inline]
    pub fn raw(&self) -> &'a [u8] {
        match self {
            Msg::System(m) => m.0,
            Msg::Directory(m) => m.0,
            Msg::Action(m) => m.0,
            Msg::Add(m) => m.0,
            Msg::AddMpid(m) => m.0,
            Msg::Exec(m) => m.0,
            Msg::ExecPx(m) => m.0,
            Msg::Cancel(m) => m.0,
            Msg::Delete(m) => m.0,
            Msg::Replace(m) => m.0,
            Msg::Trade(m) => m.0,
            Msg::Cross(m) => m.0,
            Msg::Broken(m) => m.0,
            Msg::Other(m) | Msg::Unknown(m) => m.0,
        }
    }

    /// Stock locate; `None` only for an unknown type shorter than the header.
    #[inline]
    pub fn locate(&self) -> Option<u16> {
        let b = self.raw();
        (b.len() >= HEADER_LEN).then(|| be16(b, 1))
    }

    /// Timestamp; `None` only for an unknown type shorter than the header.
    #[inline]
    pub fn ts(&self) -> Option<u64> {
        let b = self.raw();
        (b.len() >= HEADER_LEN).then(|| u48(b, 5))
    }
}

/// Classify a payload: length-check known types against the table, wrap in the matching view.
#[inline]
pub fn parse(payload: &[u8]) -> Result<Msg<'_>, ParseError> {
    let Some(&ty) = payload.first() else {
        return Err(ParseError::Empty);
    };
    let Some(want) = spec_len(ty) else {
        return Ok(Msg::Unknown(RawMsg(payload)));
    };
    if payload.len() != want {
        return Err(ParseError::Length {
            ty,
            got: payload.len(),
        });
    }
    Ok(match ty {
        b'S' => Msg::System(SystemEvent(payload)),
        b'R' => Msg::Directory(StockDirectory(payload)),
        b'H' => Msg::Action(TradingAction(payload)),
        b'A' => Msg::Add(AddOrder(payload)),
        b'F' => Msg::AddMpid(AddOrderMpid(payload)),
        b'E' => Msg::Exec(OrderExecuted(payload)),
        b'C' => Msg::ExecPx(OrderExecutedWithPrice(payload)),
        b'X' => Msg::Cancel(OrderCancel(payload)),
        b'D' => Msg::Delete(OrderDelete(payload)),
        b'U' => Msg::Replace(OrderReplace(payload)),
        b'P' => Msg::Trade(Trade(payload)),
        b'Q' => Msg::Cross(CrossTrade(payload)),
        b'B' => Msg::Broken(BrokenTrade(payload)),
        _ => Msg::Other(RawMsg(payload)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_table_matches_spec_len_and_has_23_entries() {
        assert_eq!(SPEC_LEN.len(), 23);
        for (t, n) in SPEC_LEN {
            assert_eq!(spec_len(t), Some(n), "type {}", t as char);
        }
        assert_eq!(spec_len(b'Z'), None);
        assert_eq!(spec_len(0), None);
    }

    #[test]
    fn length_mismatch_is_an_error_unknown_type_is_not() {
        let mut a = [0u8; 35];
        a[0] = b'A';
        assert_eq!(parse(&a), Err(ParseError::Length { ty: b'A', got: 35 }));
        assert_eq!(parse(&[]), Err(ParseError::Empty));
        let z = [b'Z', 1, 2];
        match parse(&z) {
            Ok(Msg::Unknown(r)) => {
                assert_eq!(r.ty(), b'Z');
                assert_eq!(r.ts(), None);
                assert_eq!(r.locate(), None);
            }
            other => panic!("{other:?}"),
        }
        let mut y = [0u8; 20];
        y[0] = b'Y';
        assert!(matches!(parse(&y), Ok(Msg::Other(_))));
        let e = format!("{}", ParseError::Length { ty: b'A', got: 35 });
        assert!(e.contains("'A'") && e.contains("35") && e.contains("36"));
    }

    #[test]
    fn u48_reads_six_bytes() {
        let b = [0xff, 0x09, 0xf6, 0x49, 0xc8, 0x0c, 0xd3, 0xff];
        assert_eq!(u48(&b, 1), 10_953_404_452_051);
        assert_eq!(u48(&[0xff; 6], 0), (1u64 << 48) - 1);
    }
}
