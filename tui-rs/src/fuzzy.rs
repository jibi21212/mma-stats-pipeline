//! Case-insensitive fuzzy name narrowing for fighter pickers.
//!
//! Backed by the `fuzzy-matcher` crate (SkimMatcherV2). An empty query returns
//! the full list unchanged; a non-empty query returns the matching subset
//! ordered best-match-first.

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

/// Filter and rank `names` by fuzzy similarity to `query` (case-insensitive).
///
/// - Empty / whitespace-only `query` -> all `names`, original order preserved.
/// - Otherwise -> only names that fuzzy-match, sorted by descending score.
///
/// Returns owned `String`s so callers needn't keep `names` borrowed.
pub fn rank(names: &[String], query: &str) -> Vec<String> {
    rank_scored(names, query)
        .into_iter()
        .map(|(name, _score)| name)
        .collect()
}

/// Like [`rank`] but returns each match paired with its score (higher = better),
/// best-first. Empty query yields every name with score 0.
pub fn rank_scored(names: &[String], query: &str) -> Vec<(String, i64)> {
    // Empty / whitespace-only query: everything passes, original order, score 0.
    if query.trim().is_empty() {
        return names.iter().map(|n| (n.clone(), 0)).collect();
    }

    let matcher = SkimMatcherV2::default().ignore_case();

    // Collect (original index, name, score) for every fuzzy match.
    let mut scored: Vec<(usize, String, i64)> = names
        .iter()
        .enumerate()
        .filter_map(|(idx, name)| {
            matcher
                .fuzzy_match(name, query)
                .map(|score| (idx, name.clone(), score))
        })
        .collect();

    // Best score first; ties broken by original list order for determinism.
    // `sort_by` is stable, but we key on the original index explicitly so the
    // ordering is unambiguous regardless of sort stability guarantees.
    scored.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

    scored
        .into_iter()
        .map(|(_idx, name, score)| (name, score))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_query_returns_all_in_order_with_zero_scores() {
        let input = names(&["Charlie", "Alpha", "Bravo"]);
        let out = rank_scored(&input, "");
        assert_eq!(
            out,
            vec![
                ("Charlie".to_string(), 0),
                ("Alpha".to_string(), 0),
                ("Bravo".to_string(), 0),
            ]
        );
    }

    #[test]
    fn whitespace_query_is_treated_as_empty() {
        let input = names(&["Charlie", "Alpha", "Bravo"]);
        assert_eq!(rank(&input, "   "), input);
    }

    #[test]
    fn rank_strips_scores_but_keeps_order() {
        let input = names(&["Jon Jones", "Junior dos Santos"]);
        let scored = rank_scored(&input, "jon");
        let plain = rank(&input, "jon");
        let from_scored: Vec<String> = scored.into_iter().map(|(n, _)| n).collect();
        assert_eq!(plain, from_scored);
    }
}
