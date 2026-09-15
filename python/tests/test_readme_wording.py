"""README wording guard: no marketing claims the repository cannot back with a measurement.

Forbidden anywhere: "HFT-grade", "zero-copy" (field decode copies), "low latency" /
"low-latency" (no wire path exists). "production" may appear only in a sentence that negates it
("not a production system"); "nanosecond" only in a sentence that also names a percentile
("median" or "p99"), because a bare nanosecond figure on Apple Silicon is timer quantisation.
"""

from __future__ import annotations

import re

from conftest import README

FORBIDDEN = ["hft-grade", "zero-copy", "zero copy", "low latency", "low-latency"]


def sentences(text: str) -> list[str]:
    # split on sentence ends and on table / list boundaries; keep it crude but deterministic
    return [s.strip() for s in re.split(r"(?<=[.!?])\s+|\n", text) if s.strip()]


def test_readme_has_no_unbacked_claims():
    text = README.read_text().lower()
    for word in FORBIDDEN:
        assert word not in text, f"README contains {word!r}"
    for s in sentences(text):
        if "production" in s:
            assert re.search(r"\bnot\b", s), f"'production' without a negation: {s!r}"
        if "nanosecond" in s:
            assert "median" in s or "p99" in s, f"'nanosecond' without a percentile: {s!r}"


def test_readme_has_the_conventions_sections():
    text = README.read_text()
    for heading in ["## Run it", "## Replay statistics", "## Throughput", "## Design rules", "## What is where",
                    "## Roadmap", "## Data and privacy", "## Companion repos"]:  # fmt: skip
        assert heading in text, heading
    assert "MIT" in text and "Hanxiong (Charlie) Yan" in text
