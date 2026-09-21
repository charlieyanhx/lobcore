"""`lobcore.replay_stats` reproduces the `lobcore replay --stats` block for the committed
fixture: the pinned counts and hashes, the deterministic lines of the README block (written by
the CLI in CI), and, when the release binary is present locally, the CLI's own output.
"""

from __future__ import annotations

import re
import shutil
import subprocess

import lobcore
import pytest
from conftest import FIXTURE, README, REPO

BEGIN, END = "<!-- lobcore:begin:stats -->", "<!-- lobcore:end:stats -->"
# Pinned by crates/lob-bench/tests/cli.rs and crates/lob-feed/tests/synth_replay.rs.
PINS = {
    "messages": 100_000,
    "truncated": 0,
    "bytes_in": 2_968_812,
    "live_hwm": 3_657,
    "live": 3_649,
    "crossed_before_q": 9_819,
    "crossed_after_q": 0,
    "crossed_after_q_trading": 0,
    "unknown_id": 0,
    "negative_qty": 0,
    "duplicate_id": 0,
    "ec_total": 668,
    "ec_at_head": 668,
    "unknown_type": 0,
    "no_directory": 0,
    "placeholder_adds": 413,  # of 41,029 A + F: the generator's 1 % placeholder_rate
    "trailing_bytes": 0,
}
BY_TYPE = {
    "S": 6,
    "R": 8,
    "H": 8,
    "A": 40_086,
    "F": 943,
    "D": 37_219,
    "X": 11_134,
    "U": 4_845,
    "E": 668,
    "P": 96,
    "L": 4_987,
}
EVENT_LOG_SHA256 = "fd52410fc6465823184edb2830ae6969fb27a5d7f2a4ea4497012a2c16245fcc"
ALL_LOCATES_SHA256 = "851e617c74e7a148336501e55073f060783565de218818a853275f69e891ae34"


def deterministic_lines(block: str) -> list[str]:
    """Non-empty lines between the markers minus the wall-clock rows (the CLI's --check-readme rule)."""
    inner = block.split(BEGIN, 1)[1].split(END, 1)[0]
    return [ln for ln in inner.splitlines() if ln.strip() and not ln.startswith("| msgs/s")]


@pytest.fixture(scope="module")
def stats():
    return lobcore.replay_stats(FIXTURE)


@pytest.fixture(scope="module")
def cli_stats():
    """Same replay with the CLI's default window (2048), which the rendered block names."""
    return lobcore.replay_stats(FIXTURE, array_window=2048)


def test_pinned_counts_and_hashes(stats):
    for k, v in PINS.items():
        assert stats[k] == v, k
    assert stats["by_type"] == BY_TYPE
    assert sum(BY_TYPE.values()) == PINS["messages"]
    assert (
        stats["event_count"]
        == 94_895
        == BY_TYPE["A"] + BY_TYPE["F"] + BY_TYPE["D"] + BY_TYPE["X"] + BY_TYPE["U"] + BY_TYPE["E"]
    )
    assert stats["event_log_hash"].hex() == EVENT_LOG_SHA256
    assert stats["all_locates_hash"].hex() == ALL_LOCATES_SHA256
    assert stats["source"] == "synth_s7_100k.itch" and stats["source_cut"] is False
    assert [c for c, _ in stats["s_events"]] == ["O", "S", "Q", "M", "E", "C"]
    assert stats["first_ts"] <= stats["last_ts"]
    assert sum(stats["by_type_hour"]["A"]) == BY_TYPE["A"]
    assert len(stats["locates"]) == 8
    assert sum(row["live"] for row in stats["locates"]) == PINS["live"]
    assert all(row["symbol"] == f"SYN{row['locate']:04d}" and row["h_state"] == "T" for row in stats["locates"])
    assert stats["msgs_per_s_excl"] > 0 and stats["wall_ns"] > 0
    assert BEGIN in stats["render"] and END in stats["render"]


def test_watchlist_moves_locates_to_the_array_book_without_changing_hashes(stats):
    w = lobcore.replay_stats(FIXTURE, symbols=["SYN0003"], locates=[5, 8], array_window=512)
    assert w["event_log_hash"] == stats["event_log_hash"]
    assert w["all_locates_hash"] == stats["all_locates_hash"]
    rows = {r["locate"]: r for r in w["locates"]}
    assert {loc for loc, r in rows.items() if r["array"]} == {3, 5, 8}
    for loc in (3, 5, 8):
        assert (
            rows[loc]["window"][1] == 512
            and rows[loc]["close_hash"] == {r["locate"]: r for r in stats["locates"]}[loc]["close_hash"]
        )
    assert "SYN0003 #5 #8" == w["watchlist"] and w["array_window"] == 512


def test_render_matches_the_readme_block(cli_stats):
    text = README.read_text()
    if BEGIN not in text or END not in text:
        pytest.skip("README has no stats block yet (written by `lobcore replay --stats --write-readme`)")
    assert deterministic_lines(text) == deterministic_lines(cli_stats["render"])


def test_render_matches_the_cli_when_the_release_binary_exists(cli_stats):
    stats = cli_stats
    exe = REPO / "target" / "release" / "lobcore"
    if not exe.exists() and shutil.which("lobcore") is None:
        pytest.skip("release binary not built (cargo build --release -p lob-bench)")
    out = subprocess.run(
        [str(exe) if exe.exists() else "lobcore", "replay", "--stats", str(FIXTURE)],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    assert deterministic_lines(out) == deterministic_lines(stats["render"])
    m = re.search(r"event log \| ([\d,]+) records, sha256 `([0-9a-f]{64})`", out)
    assert m and m.group(2) == EVENT_LOG_SHA256 and int(m.group(1).replace(",", "")) == stats["event_count"]
