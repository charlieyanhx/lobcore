//! The two vendored Apache-2.0 DBN stubs (see NOTICE) decoded field by field. The XNAS.ITCH
//! stub is published as `.dbn.zst`; its inflated twin (`zstd -d`) is vendored next to it so the
//! test needs no zstd decoder. Format test only: no book is built from these records.

use lob_feed::mbo::record::{F_LAST, F_SNAPSHOT, MBO_LEN, RTYPE_MBO, UNDEF_PRICE};
use lob_feed::{DbnError, DbnHeader, MboMsg, Record, records};

const GLBX: &[u8] = include_bytes!("fixtures/dbn/test_data.mbo.v3.dbn");
const XNAS: &[u8] = include_bytes!("fixtures/dbn/xnas_itch.test_data.mbo.dbn");
const XNAS_ZST: &[u8] = include_bytes!("fixtures/dbn/test_data.mbo.dbn.zst");

fn mbo(bytes: &[u8]) -> (DbnHeader, String, Vec<MboMsg>) {
    let (h, meta, recs) = records(bytes).unwrap();
    let meta = meta.expect("version-3 metadata");
    let v: Vec<MboMsg> = recs
        .map(|r| match r.unwrap() {
            Record::Mbo(m) => m,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    (h, meta.dataset_str().to_string(), v)
}

#[test]
fn xnas_itch_stub_four_adds_for_nvda() {
    assert_eq!(XNAS.len(), 584);
    assert_eq!(XNAS_ZST.len(), 249);
    assert_eq!(&XNAS_ZST[..4], &[0x28, 0xb5, 0x2f, 0xfd], "zstd magic");
    let (h, dataset, v) = mbo(XNAS);
    assert_eq!(h.version, 3);
    assert_eq!(h.metadata_len, 352);
    assert_eq!(dataset, "XNAS.ITCH");
    assert_eq!(v.len(), 4);
    let prices = [502.0, 512.01, 539.0, 510.01];
    let sizes = [2, 1, 17, 1];
    let sides = *b"BBAB";
    let order_ids = [196_851, 196_855, 198_583, 200_963];
    let seqs = [289_000, 289_001, 290_662, 293_260];
    for (i, m) in v.iter().enumerate() {
        assert_eq!(m.length as usize * 4, MBO_LEN);
        assert_eq!(m.rtype, RTYPE_MBO);
        assert_eq!(m.publisher_id, 2);
        assert_eq!(m.instrument_id, 6155);
        assert_eq!(m.action, b'A');
        assert_eq!(m.side, sides[i]);
        assert_eq!(m.flags, 0x82);
        assert_eq!(m.flags & F_LAST, F_LAST);
        assert_eq!(m.flags & F_SNAPSHOT, 0);
        assert_eq!(m.channel_id, 0);
        assert_eq!(m.price, (prices[i] * 1e9_f64).round() as i64);
        assert_eq!(m.price_f64(), Some(prices[i]));
        assert_eq!(m.size, sizes[i]);
        assert_eq!(m.order_id, order_ids[i]);
        assert_eq!(m.sequence, seqs[i]);
        assert!(m.ts_recv > m.ts_event, "capture after matching engine");
        assert!(m.ts_in_delta > 0);
        // round trip: the decoded record re-encodes to the same 56 bytes
        let off = 8 + h.metadata_len as usize + i * MBO_LEN;
        assert_eq!(&m.encode()[..], &XNAS[off..off + MBO_LEN]);
    }
    assert_eq!(v[0].ts_event, 1_609_146_088_297_098_637);
    assert_eq!(v[0].ts_recv, 1_609_146_088_297_109_209);
    assert_eq!(v[0].ts_in_delta, 10_572);
    assert_eq!(v[0].price, 502_000_000_000);
    assert_eq!(v[2].price, 539_000_000_000);
}

#[test]
fn glbx_stub_two_cancels_for_esh1() {
    assert_eq!(GLBX.len(), 472);
    let (h, dataset, v) = mbo(GLBX);
    assert_eq!(h.version, 3);
    assert_eq!(h.metadata_len, 352);
    assert_eq!(dataset, "GLBX.MDP3");
    assert_eq!(v.len(), 2);
    for m in &v {
        assert_eq!(m.publisher_id, 1);
        assert_eq!(m.instrument_id, 5482);
        assert_eq!(m.action, b'C');
        assert_eq!(m.side, b'A');
        assert_eq!(m.size, 1);
        assert_eq!(m.flags, F_LAST);
        assert_ne!(m.price, UNDEF_PRICE);
    }
    assert_eq!(v[0].price, 3_722_750_000_000);
    assert_eq!(v[1].price, 3_723_000_000_000);
    assert_eq!(v[0].price_f64(), Some(3722.75));
    assert_eq!(v[1].price_f64(), Some(3723.0));
    assert_eq!(v[0].order_id, 647_784_973_705);
    assert_eq!(v[1].order_id, 647_784_973_631);
    assert_eq!(v[0].sequence, 1_170_352);
    assert_eq!(v[1].sequence, 1_170_353);
    assert_eq!(v[0].ts_event, 1_609_160_400_000_429_831);
    assert_eq!(v[0].ts_in_delta, 22_993);
    assert_eq!(v[1].ts_in_delta, 19_621);
}

#[test]
fn truncated_record_is_an_error_and_other_rtypes_are_skipped() {
    let cut = &GLBX[..GLBX.len() - 1];
    let (_, _, recs) = records(cut).unwrap();
    let all: Vec<_> = recs.collect();
    assert_eq!(all.len(), 2);
    assert!(all[0].is_ok());
    assert!(matches!(
        all[1],
        Err(DbnError::Record {
            len: 56,
            have: 55,
            ..
        })
    ));
    // an MBP-1 header (rtype 0x01, 4 words) followed by the first GLBX record
    let mut mixed = GLBX[..8 + 352].to_vec();
    mixed.extend_from_slice(&[4, 0x01]);
    mixed.extend_from_slice(&[0u8; 14]);
    mixed.extend_from_slice(&GLBX[8 + 352..8 + 352 + 56]);
    let (_, _, recs) = records(&mixed).unwrap();
    let all: Vec<_> = recs.map(Result::unwrap).collect();
    assert_eq!(all[0], Record::Other { rtype: 1, len: 16 });
    assert!(matches!(all[1], Record::Mbo(m) if m.price == 3_722_750_000_000));
    assert_eq!(records(b"nope").unwrap_err(), DbnError::BadMagic);
}
