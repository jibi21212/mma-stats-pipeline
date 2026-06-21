//! Offline, deterministic tests for the fuzzy name-narrowing helpers.
//!
//! These exercise `src/fuzzy.rs`'s `rank` / `rank_scored` against small, fixed
//! name lists so behaviour is fully reproducible without the DB or sidecar.
//!
//! The crate is a pure binary (no `[lib]` target and `main.rs` declares its
//! modules privately), so an external test cannot `use mma_tui::fuzzy`. We
//! instead `include!` the module source directly into this test crate, which
//! compiles and tests the exact same code. `src/fuzzy.rs` already carries its
//! own `#[cfg(test)] mod tests`; we wrap the include in a private module and
//! re-export only the two public fns so those inner unit tests don't collide
//! with the integration cases below.

#[path = "../src/fuzzy.rs"]
mod fuzzy_impl;

use fuzzy_impl::{rank, rank_scored};

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn empty_query_returns_all_in_original_order() {
    let input = names(&["Zulu", "Alpha", "Mike", "Bravo"]);
    let out = rank(&input, "");
    assert_eq!(
        out, input,
        "empty query must return every name, order preserved"
    );
}

#[test]
fn whitespace_only_query_returns_all() {
    let input = names(&["Zulu", "Alpha", "Mike"]);
    assert_eq!(rank(&input, "\t  \n"), input);
}

#[test]
fn empty_query_against_empty_list_is_empty() {
    let input: Vec<String> = Vec::new();
    assert!(rank(&input, "").is_empty());
}

#[test]
fn no_match_returns_empty() {
    let input = names(&["Jon Jones", "Israel Adesanya", "Alex Pereira"]);
    let out = rank(&input, "zzzqxw");
    assert!(
        out.is_empty(),
        "a query that matches nothing yields no results"
    );
}

#[test]
fn case_insensitive_matching() {
    let input = names(&["Conor McGregor"]);
    // Wildly different casing in the query must still match.
    assert_eq!(rank(&input, "CONOR"), names(&["Conor McGregor"]));
    assert_eq!(rank(&input, "mcgregor"), names(&["Conor McGregor"]));
    assert_eq!(rank(&input, "McGrEgOr"), names(&["Conor McGregor"]));
}

#[test]
fn case_does_not_change_match_set() {
    let input = names(&["Khabib Nurmagomedov", "Justin Gaethje", "Dustin Poirier"]);
    let lower = rank(&input, "dustin");
    let upper = rank(&input, "DUSTIN");
    assert_eq!(lower, upper);
    assert_eq!(lower, names(&["Dustin Poirier"]));
}

#[test]
fn prefix_match_beats_scattered_match() {
    // "jon" is a contiguous prefix of "Jon Jones" but only scattered across
    // "Junior dos Santos" (J..o..n). The prefix match must rank first.
    let input = names(&["Junior dos Santos", "Jon Jones"]);
    let out = rank(&input, "jon");
    assert_eq!(out.first().map(String::as_str), Some("Jon Jones"));

    let scored = rank_scored(&input, "jon");
    let jon = scored.iter().find(|(n, _)| n == "Jon Jones").unwrap().1;
    let junior = scored
        .iter()
        .find(|(n, _)| n == "Junior dos Santos")
        .unwrap()
        .1;
    assert!(
        jon > junior,
        "prefix score ({jon}) should exceed scattered score ({junior})"
    );
}

#[test]
fn exact_match_holds_top_score_and_beats_partial() {
    // A full-name query: the candidate that contains the query contiguously
    // ("Jon Jones") must rank at the very top, strictly above a candidate that
    // only matches part of the query ("Jon Smith" shares "Jon " but not the
    // surname, so it scores lower).
    let input = names(&["Jon Smith", "Jon Jones"]);
    let scored = rank_scored(&input, "Jon Jones");

    assert_eq!(
        scored.first().map(|(n, _)| n.as_str()),
        Some("Jon Jones"),
        "the contiguous full match must be first"
    );

    let exact = scored.iter().find(|(n, _)| n == "Jon Jones").unwrap().1;
    if let Some((_, partial)) = scored.iter().find(|(n, _)| n == "Jon Smith") {
        assert!(
            exact > *partial,
            "exact full match ({exact}) must beat partial match ({partial})"
        );
    }
    // Whether or not the partial even matches, the exact match is the maximum.
    let best = scored.iter().map(|(_, s)| *s).max().unwrap();
    assert_eq!(exact, best, "exact full match should hold the top score");
}

#[test]
fn results_are_sorted_descending_by_score() {
    let input = names(&[
        "Alexander Volkanovski",
        "Max Holloway",
        "Alex Volkov",
        "Brian Ortega",
    ]);
    let scored = rank_scored(&input, "vol");
    // Every adjacent pair must be non-increasing in score.
    for pair in scored.windows(2) {
        assert!(
            pair[0].1 >= pair[1].1,
            "scores not in descending order: {scored:?}"
        );
    }
}

#[test]
fn adesa_surfaces_israel_adesanya() {
    let roster = names(&[
        "Israel Adesanya",
        "Alex Pereira",
        "Sean Strickland",
        "Dricus du Plessis",
        "Robert Whittaker",
    ]);
    let out = rank(&roster, "adesa");
    assert_eq!(
        out.first().map(String::as_str),
        Some("Israel Adesanya"),
        "the classic 'adesa' -> 'Israel Adesanya' lookup must work"
    );
    // None of the other names contain the subsequence a-d-e-s-a, so the result
    // should be a singleton.
    assert_eq!(out, names(&["Israel Adesanya"]));
}

#[test]
fn rank_and_rank_scored_agree_on_ordering() {
    let input = names(&["Daniel Cormier", "Cain Velasquez", "Stipe Miocic"]);
    let plain = rank(&input, "ci");
    let from_scored: Vec<String> = rank_scored(&input, "ci")
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(plain, from_scored);
}

#[test]
fn ties_preserve_original_order_for_duplicates() {
    // Two identical names at different positions must both be retained (the
    // deterministic tie-break is on original index, so duplicates survive in
    // their original relative order rather than being collapsed).
    let input = names(&["Test Fighter", "Other Guy", "Test Fighter"]);
    let scored = rank_scored(&input, "test fighter");
    let kept: Vec<&str> = scored
        .iter()
        .filter(|(n, _)| n == "Test Fighter")
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(kept, vec!["Test Fighter", "Test Fighter"]);
}
