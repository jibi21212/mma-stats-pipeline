//! The "layman layer": maps raw stat keys to plain-English explanations and
//! formats a stat value into a human-readable sentence.
//!
//! Stat keys are the DB / model column names (e.g. `"slpm"`, `"str_acc"`,
//! `"td_def"`, `"reach_in"`, `"control_time"`, `"elo"`, `"recent_winrate"`).
//!
//! Conventions (see docs/SCHEMA_CONTRACT.md):
//! - `*_acc` / `*_def` / `*_pct` and the winrate / probability fields are stored
//!   as 0..1 FRACTIONS and rendered to users as percentages.
//! - `reach_in` / `height_in` are inches; `control_time` is seconds; `age` is
//!   years; `layoff_days` is days; `elo` is a unitless rating.
//! - A `None` value means missing / NULL data and renders as "unknown".

/// Normalize a stat key so callers can pass either the DB column name or one of
/// a few friendly aliases used by the predictor's tale-of-the-tape.
fn canonical(stat_key: &str) -> &str {
    match stat_key {
        // win-probability output appears under several names across the pipeline
        "win_prob" | "probability" | "prob_a" | "prob_b" | "prob" => "win_probability",
        // tale-of-tape sometimes refers to reach/height as advantages
        "reach" => "reach_in",
        "height" => "height_in",
        "winrate" => "recent_winrate",
        other => other,
    }
}

/// Return a short, plain-English explanation of what `stat_key` measures.
///
/// Unknown keys return a generic fallback string (never panics).
pub fn explain(stat_key: &str) -> &'static str {
    match canonical(stat_key) {
        // --- fighters-table career stats ---------------------------------- //
        "slpm" => {
            "Significant strikes a fighter lands per minute — how busy and \
             offensive they are on the feet (higher means more output)."
        }
        "str_acc" => {
            "Striking accuracy: the share of attempted strikes that actually \
             land — higher means cleaner, more precise punching and kicking."
        }
        "sapm" => {
            "Significant strikes a fighter absorbs per minute — how much damage \
             they take (lower is better, it means they get hit less)."
        }
        "str_def" => {
            "Striking defense: the share of opponents' strikes the fighter \
             avoids or blocks — higher means they are harder to hit."
        }
        "td_avg" => {
            "Average takedowns landed per 15 minutes — how often a fighter \
             drags the fight to the mat (higher means a stronger wrestler)."
        }
        "td_acc" => {
            "Takedown accuracy: the share of takedown attempts that succeed — \
             higher means their wrestling shots actually finish."
        }
        "td_def" => {
            "Takedown defense: the share of opponents' takedown attempts the \
             fighter stuffs — higher means they keep the fight standing."
        }
        "sub_avg" => {
            "Average submission attempts per 15 minutes — how actively a fighter \
             hunts for chokes and joint locks (higher means a grappling threat)."
        }

        // --- predictor tale-of-the-tape ----------------------------------- //
        "elo" => {
            "An overall skill rating earned by beating opponents — like a chess \
             rank for fighters; around 1500 is average and the best are 1750+."
        }
        "age" => {
            "The fighter's age in years on fight day — younger fighters are \
             usually fresher, older ones more experienced but more worn down."
        }
        "record" => {
            "The fighter's career win-loss record (wins first) — a quick read on \
             how often they have won versus lost."
        }
        "reach_in" => {
            "Arm span (fingertip to fingertip) in inches — a longer reach lets a \
             fighter hit from farther away while staying out of range."
        }
        "height_in" => {
            "The fighter's height in inches — taller fighters often have leverage \
             and reach advantages, though it is not always decisive."
        }
        "stance" => {
            "Which side the fighter leads with: orthodox (left foot forward), \
             southpaw (right foot forward), or switch (changes between them)."
        }
        "recent_winrate" => {
            "The share of recent fights the fighter has won — a snapshot of how \
             well they have been doing lately rather than over a whole career."
        }
        "form_delta" => {
            "Whether the fighter is trending up or down — a positive number means \
             improving form, a negative number means a recent slide."
        }
        "layoff_days" => {
            "Days since the fighter's last bout — a long layoff can mean ring \
             rust, while a short one means they are sharp and active."
        }
        "win_probability" => {
            "The model's estimated chance this fighter wins — 50% is a coin flip, \
             and the farther above 50% the more confident the pick."
        }

        // --- fallback ----------------------------------------------------- //
        _ => "A fighter statistic (no plain-English description available).",
    }
}

// --------------------------------------------------------------------------- //
// value formatting helpers
// --------------------------------------------------------------------------- //

/// Render a 0..1 fraction as a whole-percent string, e.g. `0.476 -> "48%"`.
fn pct(value: f64) -> String {
    format!("{}%", (value * 100.0).round() as i64)
}

/// Qualitative read for a 0..1 "good when high" accuracy/defense fraction.
fn high_good_band(value: f64) -> &'static str {
    if value >= 0.60 {
        "elite"
    } else if value >= 0.50 {
        "strong"
    } else if value >= 0.40 {
        "average"
    } else {
        "below average"
    }
}

/// Render seconds as `m:ss` (e.g. `135 -> "2:15"`).
fn mmss(total_seconds: f64) -> String {
    let secs = total_seconds.round().max(0.0) as i64;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Format `value` for `stat_key` into a human-readable phrase, applying the
/// right units / scaling (e.g. fractions -> percent, seconds -> m:ss, inches).
///
/// `value` is `None` for missing/NULL data and should render as e.g. "unknown".
/// NaN / infinite values are also treated as missing so the UI never shows
/// garbage like "NaN%".
pub fn describe(stat_key: &str, value: Option<f64>) -> String {
    // Treat missing, NaN and infinite all as "unknown" — the sidecar maps
    // NaN/Inf to null, but be defensive in case a raw f64 sneaks through.
    let v = match value {
        Some(v) if v.is_finite() => v,
        _ => return "unknown".to_string(),
    };

    match canonical(stat_key) {
        // --- per-minute / per-15-min rates (raw numbers, 2 dp) ------------- //
        "slpm" => {
            let read = if v >= 5.0 {
                "very high output"
            } else if v >= 3.5 {
                "high output"
            } else if v >= 2.0 {
                "moderate output"
            } else {
                "low output"
            };
            format!("{v:.2} strikes landed per minute ({read})")
        }
        "sapm" => {
            // lower is better here
            let read = if v <= 2.0 {
                "hard to hit"
            } else if v <= 3.5 {
                "average"
            } else {
                "takes a lot of damage"
            };
            format!("{v:.2} strikes absorbed per minute ({read})")
        }
        "td_avg" => {
            let read = if v >= 3.0 {
                "heavy wrestler"
            } else if v >= 1.0 {
                "mixes in takedowns"
            } else {
                "rarely takes it down"
            };
            format!("{v:.2} takedowns per 15 min ({read})")
        }
        "sub_avg" => {
            let read = if v >= 1.0 {
                "active submission hunter"
            } else if v >= 0.3 {
                "occasional threat"
            } else {
                "rarely goes for submissions"
            };
            format!("{v:.2} submission attempts per 15 min ({read})")
        }

        // --- 0..1 accuracy / defense fractions ---------------------------- //
        "str_acc" => format!("{} striking accuracy ({})", pct(v), high_good_band(v)),
        "str_def" => format!("{} striking defense ({})", pct(v), high_good_band(v)),
        "td_acc" => format!("{} takedown accuracy ({})", pct(v), high_good_band(v)),
        "td_def" => format!("{} takedown defense ({})", pct(v), high_good_band(v)),

        // --- elo rating with tiers ---------------------------------------- //
        "elo" => {
            let tier = if v >= 1750.0 {
                "elite"
            } else if v >= 1600.0 {
                "strong"
            } else if v >= 1450.0 {
                "around average"
            } else {
                "below average"
            };
            format!("Elo {} ({})", v.round() as i64, tier)
        }

        // --- age ---------------------------------------------------------- //
        "age" => {
            let read = if v < 25.0 {
                "young, on the rise"
            } else if v <= 32.0 {
                "in their prime"
            } else if v <= 37.0 {
                "veteran"
            } else {
                "late-career"
            };
            format!("{} years old ({read})", v.round() as i64)
        }

        // --- reach / height in inches ------------------------------------- //
        "reach_in" => {
            let inches = v.round() as i64;
            let feet = inches / 12;
            let rem = inches % 12;
            format!("{inches}\" reach ({feet}'{rem}\")")
        }
        "height_in" => {
            let inches = v.round() as i64;
            let feet = inches / 12;
            let rem = inches % 12;
            format!("{inches}\" tall ({feet}'{rem}\")")
        }

        // --- recent winrate (0..1) ---------------------------------------- //
        "recent_winrate" => {
            let read = if v >= 0.75 {
                "on a hot streak"
            } else if v >= 0.5 {
                "winning more than losing"
            } else {
                "struggling lately"
            };
            format!("{} of recent fights won ({read})", pct(v))
        }

        // --- form delta (trend) ------------------------------------------- //
        "form_delta" => {
            if v > 0.05 {
                format!("trending up (+{v:.2}, improving form)")
            } else if v < -0.05 {
                format!("trending down ({v:.2}, declining form)")
            } else {
                format!("steady ({v:+.2}, holding form)")
            }
        }

        // --- layoff days -------------------------------------------------- //
        "layoff_days" => {
            let days = v.round() as i64;
            let read = if days <= 120 {
                "active, no rust"
            } else if days <= 365 {
                "normal break"
            } else if days <= 730 {
                "long layoff, possible rust"
            } else {
                "very long layoff, likely ring rust"
            };
            format!("{days} days since last fight ({read})")
        }

        // --- win probability (0..1) --------------------------------------- //
        "win_probability" => {
            let read = if v >= 0.70 {
                "strong favorite"
            } else if v >= 0.55 {
                "slight favorite"
            } else if v >= 0.45 {
                "near coin-flip"
            } else if v >= 0.30 {
                "slight underdog"
            } else {
                "heavy underdog"
            };
            format!("{} chance to win ({read})", pct(v))
        }

        // --- generic round-stat percentages (e.g. sig_str_pct, head_pct) -- //
        k if k.ends_with("_pct") => pct(v),

        // --- control time (seconds) --------------------------------------- //
        "control_time" => format!("{} of control time", mmss(v)),

        // --- fallback: just print the number ------------------------------ //
        _ => format!("{v:.2}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_rounds_to_whole_percent() {
        assert_eq!(pct(0.476), "48%");
        assert_eq!(pct(0.0), "0%");
        assert_eq!(pct(1.0), "100%");
    }

    #[test]
    fn mmss_formats_seconds() {
        assert_eq!(mmss(0.0), "0:00");
        assert_eq!(mmss(9.0), "0:09");
        assert_eq!(mmss(135.0), "2:15");
    }

    #[test]
    fn aliases_resolve() {
        assert_eq!(canonical("prob_a"), "win_probability");
        assert_eq!(canonical("reach"), "reach_in");
        assert_eq!(canonical("slpm"), "slpm");
    }
}
