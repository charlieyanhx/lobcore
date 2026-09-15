//! Local-only replay of the first 52,428,800 bytes of the public Nasdaq sample
//! `12302019.NASDAQ_ITCH50.gz` (fetched by `scripts/fetch_itch.sh` into the gitignored
//! `data/`). Ignored by default: the file is not redistributable and emi.nasdaq.com removes
//! files without notice, so CI never downloads it. Skips cleanly when absent.
//!
//! Pins: md5 of the head; inflated bytes and message count as zlib decodes them (flate2 with the
//! zlib-rs backend, identical to Python's zlib): 128,451,559 bytes = 4,330,712 complete messages
//! plus a 13-byte truncated tail. `gzip -dc` on the same prefix yields 128,450,560 bytes =
//! 4,330,679 messages (the research pass's oracle) because it discards the output of the
//! incomplete final deflate block; zlib emits the 999 bytes it had already decoded from that
//! block and they frame as 33 more valid messages. Both figures are stated in the README; the
//! test pins the decoder lobcore ships. Also pinned: unknown-id 0, negative-qty 0, no C / Q
//! before 09:30, the pre-open A / D / X shares within 2 pp of the measured histogram. The
//! statistics block is printed.

use md5::{Digest, Md5};

use lob_feed::{Session, SessionConfig, Watchlist, open};

const HEAD: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/itch_12302019_head50m.gz"
);
const MD5: &str = "8bd91e6f5b4a31d4d50dd6ac8a8fe7e2";

#[test]
#[ignore = "needs data/itch_12302019_head50m.gz from scripts/fetch_itch.sh (local, not CI)"]
fn real_head_replay_pins() {
    let Ok(bytes) = std::fs::read(HEAD) else {
        eprintln!("skipped: {HEAD} absent (run scripts/fetch_itch.sh)");
        return;
    };
    assert_eq!(bytes.len(), 52_428_800);
    let digest = Md5::digest(&bytes);
    assert_eq!(
        lob_core::hex(&digest),
        MD5,
        "the sample changed or the download was partial"
    );

    let mut s = Session::new(SessionConfig {
        watchlist: Watchlist::symbols(["SPY", "AAPL", "MSFT", "QQQ"]),
        source: "itch_12302019_head50m.gz".into(),
        ..Default::default()
    });
    let mut src = open(HEAD).unwrap();
    s.replay(&mut src).unwrap();
    let st = s.stats();
    assert_eq!(st.bytes_in, 128_451_559);
    assert_eq!(st.messages, 4_330_712);
    assert_eq!(st.truncated, 1);
    assert!(st.source_cut, "the 50 MiB head cuts a gzip member");
    assert_eq!(st.unknown_type, 0);
    assert_eq!(st.unknown_id, 0);
    assert_eq!(st.negative_qty(), 0);
    assert_eq!(st.duplicate_id, 0);
    assert_eq!(st.bad_side, 0);
    assert_eq!(st.by_type[b'C' as usize], 0, "no C before 09:30");
    assert_eq!(st.by_type[b'Q' as usize], 0, "no cross before 09:30");
    let share = |t: u8| st.by_type[t as usize] as f64 / st.messages as f64;
    assert!(
        (share(b'A') - 0.399).abs() < 0.02,
        "A share {}",
        share(b'A')
    );
    assert!(
        (share(b'D') - 0.373).abs() < 0.02,
        "D share {}",
        share(b'D')
    );
    assert!(
        (share(b'X') - 0.110).abs() < 0.02,
        "X share {}",
        share(b'X')
    );
    assert!(st.locates_with_book().count() > 8_000);
    assert!(s.locate_of("SPY").is_some());
    assert_eq!(
        st.s_events.iter().map(|e| e.0).collect::<Vec<_>>(),
        vec![b'O', b'S']
    );
    println!("{}", st.render());
}
