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
//! test pins the decoder lobcore ships. Also pinned, so the README real-prefix block cannot
//! drift from the code silently: the exact type histogram, 8,906 locates, live HWM / close
//! 145,701 / 145,691, crossed before Q 21, unknown-id 0, negative-qty 0, E/C at head
//! 17,789 / 17,789, the event log (4,085,484 records, `3514db25...`), the all-locates close
//! hash (`19a984f0...`) and the four watchlist rows (locate, msgs, live, overflow hits, max
//! |offset|, window, close hash). The statistics block is printed.

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
    assert_eq!(st.locates_with_book().count(), 8_906);
    assert_eq!(
        st.s_events.iter().map(|e| e.0).collect::<Vec<_>>(),
        vec![b'O', b'S']
    );
    // the README real-prefix block, literal for literal
    let hist: Vec<(u8, u64)> = (0..=255u8)
        .filter(|t| st.by_type[*t as usize] > 0)
        .map(|t| (t, st.by_type[t as usize]))
        .collect();
    assert_eq!(
        hist,
        vec![
            (b'A', 1_729_797),
            (b'D', 1_613_725),
            (b'E', 17_789),
            (b'F', 38_901),
            (b'H', 8_901),
            (b'K', 3),
            (b'L', 215_087),
            (b'P', 3_431),
            (b'R', 8_906),
            (b'S', 2),
            (b'U', 208_061),
            (b'V', 1),
            (b'X', 477_211),
            (b'Y', 8_897),
        ]
    );
    assert_eq!(
        (st.first_ts, st.last_ts),
        (Some(11_072_057_543_747), Some(32_519_442_416_600))
    );
    assert_eq!(st.no_directory, 0);
    assert_eq!(st.placeholder_adds, 15_232, "of 1,768,698 A + F: 0.86 %");
    assert_eq!(st.trailing_bytes, 0);
    assert_eq!((st.live_hwm, st.live), (145_701, 145_691));
    assert_eq!(st.crossed_before_q, 21);
    assert_eq!((st.crossed_after_q, st.crossed_after_q_trading), (0, 0));
    assert_eq!(
        (st.over_execute, st.over_cancel, st.bad_qty, st.bad_price),
        (0, 0, 0, 0)
    );
    assert_eq!((st.ec_total, st.ec_at_head), (17_789, 17_789));
    assert_eq!(st.event_count, 4_085_484);
    assert_eq!(
        lob_core::hex(&st.event_log_hash),
        "3514db25d5e09cee6e05240eaa29d88ef554670e0402cac0a6e03ce5c61362f6"
    );
    assert_eq!(
        lob_core::hex(&st.all_locates_hash),
        "19a984f0d6d7742eb9ae27185cdc006bbae7ceccef9dc96c804babe3a7cba584"
    );
    // watchlist rows: (locate, symbol, msgs, live hwm, live, H, overflow hits, max |offset| all /
    // ex-placeholder, window (base, levels, tick), close hash)
    let rows = [
        (
            13,
            "AAPL",
            6_316,
            717,
            717,
            1_841,
            71_034,
            71_034,
            (2_896_600, 2048, 100),
            "538c02205848c659cc3430338c832fec5c01c723579d7b8b02963df04affc8f6",
        ),
        (
            5291,
            "MSFT",
            4_095,
            1_018,
            1_009,
            2_446,
            19_999_998,
            25_483,
            (100, 2048, 100),
            "2431e9865f64545fce41294bcea601bae14252f333aecc3cda0797e3c61eb9bd",
        ),
        (
            6556,
            "QQQ",
            94_408,
            298,
            296,
            148,
            19_979_638,
            20_261,
            (2_036_100, 2048, 100),
            "a89c74ef8b40ff9fb0807ae9d63b75e0f73b4740064b4d676df55e35487626a1",
        ),
        (
            7451,
            "SPY",
            27_896,
            392,
            391,
            247,
            31_294,
            31_285,
            (3_129_500, 2048, 100),
            "569204099219277884c881e67ccfc058a19f0d7451a4d900b4a318216f4d7fb4",
        ),
    ];
    for (loc, sym, msgs, hwm, live, ovf, off_all, off_real, window, close) in rows {
        assert_eq!(s.locate_of(sym), Some(loc), "{sym}");
        let ls = &st.locates[loc as usize];
        assert!(ls.array && ls.watched, "{sym}");
        assert_eq!(ls.h_state, b'T', "{sym}");
        assert_eq!(
            (
                ls.msgs,
                ls.live_hwm,
                ls.live,
                ls.overflow_hits,
                ls.max_abs_offset,
                ls.max_abs_offset_real
            ),
            (msgs, hwm, live, ovf, off_all, off_real),
            "{sym}"
        );
        assert_eq!(ls.window, Some(window), "{sym}");
        assert_eq!(lob_core::hex(&ls.close_hash), close, "{sym}");
    }
    println!("{}", st.render());
}
