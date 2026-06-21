//! Offline, deterministic tests for the layman-explanation layer
//! (`src/stats_text.rs`).
//!
//! Guarantees:
//! - every supported stat key has a NON-empty plain-English explanation;
//! - `describe()` formats representative values sensibly (fractions ->
//!   percent, inches, m:ss, elo tiers, …);
//! - missing / NaN / infinite values render gracefully as "unknown" and never
//!   panic.

// The crate is a pure binary (no `[lib]` target and `main.rs` declares its
// modules privately), so an external test cannot `use mma_tui::stats_text`. We
// instead `include!` the module source directly into this test crate via
// `#[path]`, which compiles and tests the exact same code. `src/stats_text.rs`
// is self-contained (no `crate::` references) and carries its own
// `#[cfg(test)] mod tests`; we wrap the include in a private module and
// re-export only the two public fns so those inner unit tests don't collide
// with the integration cases below.
#[path = "../src/stats_text.rs"]
mod stats_text_impl;

use stats_text_impl::{describe, explain};

/// Every stat key the spec says we must cover.
const SUPPORTED_KEYS: &[&str] = &[
    // fighters-table career stats
    "slpm",
    "str_acc",
    "sapm",
    "str_def",
    "td_avg",
    "td_acc",
    "td_def",
    "sub_avg",
    // predictor tale-of-the-tape
    "elo",
    "age",
    "record",
    "reach_in",
    "height_in",
    "stance",
    "recent_winrate",
    "form_delta",
    "layoff_days",
    // win-probability output
    "win_probability",
];

#[test]
fn every_supported_key_has_nonempty_explanation() {
    for &key in SUPPORTED_KEYS {
        let text = explain(key);
        assert!(
            !text.trim().is_empty(),
            "explain({key:?}) returned an empty/whitespace string"
        );
        // sanity: should read like a sentence, not just a token
        assert!(
            text.len() > 15,
            "explain({key:?}) is suspiciously short: {text:?}"
        );
    }
}

#[test]
fn explanations_are_distinct_per_key() {
    // Distinct keys should not all collapse to the same fallback text.
    let mut seen = std::collections::HashSet::new();
    for &key in SUPPORTED_KEYS {
        seen.insert(explain(key));
    }
    assert_eq!(
        seen.len(),
        SUPPORTED_KEYS.len(),
        "some supported keys share the same explanation"
    );
}

#[test]
fn unknown_key_returns_nonempty_fallback() {
    let text = explain("totally_made_up_stat");
    assert!(!text.trim().is_empty());
}

#[test]
fn aliases_for_win_probability_explain_the_same_thing() {
    let canonical = explain("win_probability");
    for alias in ["win_prob", "probability", "prob_a", "prob_b", "prob"] {
        assert_eq!(
            explain(alias),
            canonical,
            "alias {alias:?} should map to the win-probability explanation"
        );
    }
}

#[test]
fn fraction_stats_render_as_percent() {
    // 0..1 fractions -> whole-percent string.
    assert_eq!(&describe("str_acc", Some(0.5))[..3], "50%");
    assert!(describe("str_acc", Some(0.476)).starts_with("48%"));
    assert!(describe("str_def", Some(0.62)).contains("62%"));
    assert!(describe("td_acc", Some(0.41)).contains("41%"));
    assert!(describe("td_def", Some(0.85)).contains("85%"));
    assert!(describe("recent_winrate", Some(0.75)).contains("75%"));
}

#[test]
fn rate_stats_render_with_units() {
    assert!(describe("slpm", Some(4.25)).contains("4.25"));
    assert!(
        describe("slpm", Some(4.25))
            .to_lowercase()
            .contains("per minute")
    );
    assert!(
        describe("sapm", Some(2.10))
            .to_lowercase()
            .contains("per minute")
    );
    assert!(describe("td_avg", Some(3.10)).to_lowercase().contains("15"));
    assert!(
        describe("sub_avg", Some(1.20))
            .to_lowercase()
            .contains("submission")
    );
}

#[test]
fn elo_describes_tiers() {
    assert!(
        describe("elo", Some(1400.0))
            .to_lowercase()
            .contains("below average")
    );
    assert!(
        describe("elo", Some(1500.0))
            .to_lowercase()
            .contains("average")
    );
    assert!(
        describe("elo", Some(1650.0))
            .to_lowercase()
            .contains("strong")
    );
    assert!(
        describe("elo", Some(1800.0))
            .to_lowercase()
            .contains("elite")
    );
    // the numeric rating should be present too
    assert!(describe("elo", Some(1650.0)).contains("1650"));
}

#[test]
fn age_describes_career_phase() {
    assert!(describe("age", Some(22.0)).contains("22"));
    assert!(!describe("age", Some(22.0)).is_empty());
    assert!(describe("age", Some(29.0)).to_lowercase().contains("prime"));
    assert!(describe("age", Some(40.0)).contains("40"));
}

#[test]
fn reach_and_height_render_inches_and_feet() {
    let reach = describe("reach_in", Some(72.0));
    assert!(reach.contains("72"), "{reach}");
    assert!(reach.contains("6'0"), "{reach}"); // 72 inches == 6'0"

    let height = describe("height_in", Some(70.0));
    assert!(height.contains("70"), "{height}");
    assert!(height.contains("5'10"), "{height}"); // 70 inches == 5'10"
}

#[test]
fn form_delta_signals_direction() {
    assert!(
        describe("form_delta", Some(0.4))
            .to_lowercase()
            .contains("up")
    );
    assert!(
        describe("form_delta", Some(-0.4))
            .to_lowercase()
            .contains("down")
    );
    // near zero -> steady, must not panic and not be empty
    assert!(!describe("form_delta", Some(0.0)).is_empty());
}

#[test]
fn layoff_days_signals_ring_rust() {
    assert!(describe("layoff_days", Some(30.0)).contains("30"));
    let long = describe("layoff_days", Some(900.0));
    assert!(long.contains("900"));
    assert!(long.to_lowercase().contains("rust"));
}

#[test]
fn win_probability_describes_favorite_status() {
    assert!(
        describe("win_probability", Some(0.8))
            .to_lowercase()
            .contains("favorite")
    );
    assert!(
        describe("win_probability", Some(0.2))
            .to_lowercase()
            .contains("underdog")
    );
    assert!(describe("win_probability", Some(0.5)).contains("50%"));
    // alias should behave identically
    assert_eq!(
        describe("prob_a", Some(0.8)),
        describe("win_probability", Some(0.8))
    );
}

#[test]
fn generic_pct_key_renders_percent() {
    // round-stat columns ending in _pct should still format as a percentage
    assert!(describe("sig_str_pct", Some(0.55)).contains("55%"));
    assert!(describe("head_pct", Some(0.40)).contains("40%"));
}

#[test]
fn control_time_renders_mmss() {
    let s = describe("control_time", Some(135.0));
    assert!(s.contains("2:15"), "{s}");
}

#[test]
fn missing_value_renders_unknown_for_every_key() {
    for &key in SUPPORTED_KEYS {
        let out = describe(key, None);
        assert_eq!(
            out, "unknown",
            "describe({key:?}, None) should be \"unknown\""
        );
    }
    // an unknown key with None must also be graceful
    assert_eq!(describe("totally_made_up_stat", None), "unknown");
}

#[test]
fn nan_and_infinite_values_render_unknown_without_panicking() {
    for &key in SUPPORTED_KEYS {
        assert_eq!(describe(key, Some(f64::NAN)), "unknown");
        assert_eq!(describe(key, Some(f64::INFINITY)), "unknown");
        assert_eq!(describe(key, Some(f64::NEG_INFINITY)), "unknown");
    }
}

#[test]
fn extreme_and_edge_values_do_not_panic() {
    // hammer every key with a spread of weird-but-finite inputs.
    let values = [
        0.0,
        1.0,
        -1.0,
        0.5,
        100.0,
        -100.0,
        1e9,
        -1e9,
        f64::MIN,
        f64::MAX,
    ];
    for &key in SUPPORTED_KEYS {
        for &v in &values {
            let _ = describe(key, Some(v)); // must not panic
        }
    }
    // also the generic _pct and unknown paths
    for &v in &values {
        let _ = describe("sig_str_pct", Some(v));
        let _ = describe("control_time", Some(v));
        let _ = describe("nonsense_key", Some(v));
    }
}

#[test]
fn describe_never_returns_empty_for_finite_values() {
    for &key in SUPPORTED_KEYS {
        let out = describe(key, Some(1.0));
        assert!(!out.trim().is_empty(), "describe({key:?}, 1.0) was empty");
    }
}
