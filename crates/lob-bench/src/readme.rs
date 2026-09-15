//! README block check and rewrite: the deterministic lines (counts, hashes, histograms) must
//! match; msgs/s rows never take part.

use std::path::Path;

use lob_feed::stats::{deterministic_lines, splice_block};

use crate::Error;

/// Outcome of a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Every deterministic line matches.
    Same,
    /// The README has no block.
    Missing,
    /// First differing line (1-based within the deterministic lines).
    Drift {
        line: usize,
        readme: String,
        now: String,
    },
}

/// Compare the README's block with a freshly rendered one.
pub fn compare(readme_text: &str, block: &str) -> Check {
    let Some(have) = deterministic_lines(readme_text) else {
        return Check::Missing;
    };
    let want = deterministic_lines(block).expect("rendered block carries both markers");
    let n = have.len().max(want.len());
    for i in 0..n {
        let a = have.get(i).map(String::as_str).unwrap_or("<end>");
        let b = want.get(i).map(String::as_str).unwrap_or("<end>");
        if a != b {
            return Check::Drift {
                line: i + 1,
                readme: a.to_string(),
                now: b.to_string(),
            };
        }
    }
    Check::Same
}

/// Check the block in the file at `path`.
pub fn check(path: &Path, block: &str) -> Result<Check, Error> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error(format!("cannot read {}: {e}", path.display())))?;
    Ok(compare(&text, block))
}

/// Replace (or append) the block in the file at `path`.
pub fn write(path: &Path, block: &str) -> Result<(), Error> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    std::fs::write(path, splice_block(&text, block))
        .map_err(|e| Error(format!("cannot write {}: {e}", path.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_detects_missing_same_and_drift() {
        let block = "<!-- lobcore:begin:stats -->\n| a | 1 |\n| msgs/s x | 9 |\n| b | 2 |\n<!-- lobcore:end:stats -->\n";
        assert_eq!(compare("nothing", block), Check::Missing);
        let same = splice_block("# r\n", block);
        assert_eq!(compare(&same, block), Check::Same);
        let faster = same.replace("| msgs/s x | 9 |", "| msgs/s x | 99 |");
        assert_eq!(compare(&faster, block), Check::Same);
        let drift = same.replace("| b | 2 |", "| b | 3 |");
        assert_eq!(
            compare(&drift, block),
            Check::Drift {
                line: 2,
                readme: "| b | 3 |".into(),
                now: "| b | 2 |".into()
            }
        );
        let short = same.replace("| b | 2 |\n", "");
        assert!(matches!(
            compare(&short, block),
            Check::Drift { line: 2, .. }
        ));
    }
}
