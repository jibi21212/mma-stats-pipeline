//! Garnish radar: percentile-rank six career axes for a spider overlay.
//!
//! Shape only. Higher is better. sapm is omitted (raw sapm would invert the
//! silhouette). Missing values sit on the median so a hole does not collapse
//! to the origin.

#[cfg(test)]
use crate::models::Fighter;

/// Axes plotted, in clockwise order starting at the top.
pub const AXIS_COUNT: usize = 6;
pub const AXIS_LABELS: [&str; AXIS_COUNT] = ["SLpM", "Acc", "Def", "TD", "TDd", "Sub"];

#[cfg(test)]
const AXIS_KEYS: [&str; AXIS_COUNT] = ["slpm", "str_acc", "str_def", "td_avg", "td_def", "sub_avg"];

/// One fighter's six-axis silhouette after percentile ranking (0..1).
#[derive(Debug, Clone, PartialEq)]
pub struct RadarSeries {
    pub name: String,
    pub values: [f64; AXIS_COUNT],
}

/// Prepared overlay payload the renderer draws.
#[derive(Debug, Clone, PartialEq)]
pub struct RadarOverlay {
    pub title: String,
    pub series: Vec<RadarSeries>,
}

/// The six higher-is-better career columns. sapm is intentionally dropped.
#[cfg(test)]
fn raw_axes(f: &Fighter) -> [Option<f64>; AXIS_COUNT] {
    [f.slpm, f.str_acc, f.str_def, f.td_avg, f.td_def, f.sub_avg]
}

/// Percentile of `value` among `peers`. Missing / non-finite / no spread -> 0.5.
pub fn percentile_rank(value: Option<f64>, peers: &[f64]) -> f64 {
    let Some(v) = value.filter(|x| x.is_finite()) else {
        return 0.5;
    };
    let n = peers.len();
    if n <= 1 {
        return 0.5;
    }
    let less = peers.iter().filter(|&&x| x < v).count();
    let greater = peers.iter().filter(|&&x| x > v).count();
    if less + greater == 0 {
        return 0.5;
    }
    less as f64 / (n - 1) as f64
}

/// Finite peer samples per axis, taken from `roster` (the local DB set).
pub fn peer_columns(roster: &[(String, [Option<f64>; AXIS_COUNT])]) -> [Vec<f64>; AXIS_COUNT] {
    let mut cols: [Vec<f64>; AXIS_COUNT] = std::array::from_fn(|_| Vec::new());
    for (_, axes) in roster {
        for (i, v) in axes.iter().enumerate() {
            if let Some(x) = v.filter(|x| x.is_finite()) {
                cols[i].push(x);
            }
        }
    }
    cols
}

/// Rank each subject against `roster`. One series per subject, always 6 axes.
pub fn normalize_stats(
    subjects: &[(String, [Option<f64>; AXIS_COUNT])],
    roster: &[(String, [Option<f64>; AXIS_COUNT])],
) -> Vec<RadarSeries> {
    let cols = peer_columns(roster);
    subjects
        .iter()
        .map(|(name, axes)| {
            let mut values = [0.5; AXIS_COUNT];
            for i in 0..AXIS_COUNT {
                values[i] = percentile_rank(axes[i], &cols[i]);
            }
            RadarSeries {
                name: name.clone(),
                values,
            }
        })
        .collect()
}

/// Rank fighter rows against a fighter roster (tests + callers with full rows).
#[cfg(test)]
fn normalize_fighters(subjects: &[&Fighter], roster: &[Fighter]) -> Vec<RadarSeries> {
    let subjects: Vec<(String, [Option<f64>; AXIS_COUNT])> = subjects
        .iter()
        .map(|f| (f.name.clone(), raw_axes(f)))
        .collect();
    let roster: Vec<(String, [Option<f64>; AXIS_COUNT])> = roster
        .iter()
        .map(|f| (f.name.clone(), raw_axes(f)))
        .collect();
    normalize_stats(&subjects, &roster)
}

pub fn overlay_title(series: &[RadarSeries]) -> String {
    match series {
        [a, b] => format!("Radar shape  {} vs {}   (esc close)", a.name, b.name),
        [a] => format!("Radar shape  {}   (esc close)", a.name),
        _ => "Radar shape   (esc close)".to_string(),
    }
}

/// Build an overlay for `names` (order preserved) ranked vs `roster` stats.
pub fn build_overlay_from_stats(
    names: &[String],
    roster: &[(String, [Option<f64>; AXIS_COUNT])],
) -> Option<RadarOverlay> {
    let subjects: Vec<(String, [Option<f64>; AXIS_COUNT])> = names
        .iter()
        .filter_map(|n| roster.iter().find(|(name, _)| name == n).cloned())
        .collect();
    if subjects.is_empty() {
        return None;
    }
    let series = normalize_stats(&subjects, roster);
    Some(RadarOverlay {
        title: overlay_title(&series),
        series,
    })
}

/// Build an overlay for `names` ranked vs full fighter rows.
#[cfg(test)]
fn build_overlay(names: &[String], roster: &[Fighter]) -> Option<RadarOverlay> {
    let roster: Vec<(String, [Option<f64>; AXIS_COUNT])> = roster
        .iter()
        .map(|f| (f.name.clone(), raw_axes(f)))
        .collect();
    build_overlay_from_stats(names, &roster)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fighter(
        name: &str,
        slpm: Option<f64>,
        str_acc: Option<f64>,
        sapm: Option<f64>,
        str_def: Option<f64>,
        td_avg: Option<f64>,
        td_def: Option<f64>,
        sub_avg: Option<f64>,
    ) -> Fighter {
        Fighter {
            fighter_id: 0,
            name: name.to_string(),
            nickname: None,
            nationality: None,
            height_in: None,
            weight_lbs: None,
            reach_in: None,
            stance: None,
            date_of_birth: None,
            wins: 0,
            losses: 0,
            draws: 0,
            no_contests: 0,
            was_champion: 0,
            championship_bouts_won: 0,
            slpm,
            str_acc,
            sapm,
            str_def,
            td_avg,
            td_acc: None,
            td_def,
            sub_avg,
        }
    }

    #[test]
    fn six_axes_and_sapm_is_not_plotted() {
        assert_eq!(AXIS_LABELS.len(), 6);
        assert_eq!(AXIS_KEYS.len(), 6);
        assert!(!AXIS_KEYS.contains(&"sapm"));
        assert_eq!(
            AXIS_KEYS,
            ["slpm", "str_acc", "str_def", "td_avg", "td_def", "sub_avg"]
        );
        assert_eq!(AXIS_LABELS, ["SLpM", "Acc", "Def", "TD", "TDd", "Sub"]);
    }

    #[test]
    fn percentile_maps_min_max_and_ties() {
        let peers = [1.0, 2.0, 3.0];
        assert_eq!(percentile_rank(Some(1.0), &peers), 0.0);
        assert_eq!(percentile_rank(Some(3.0), &peers), 1.0);
        assert_eq!(percentile_rank(Some(2.0), &peers), 0.5);
        assert_eq!(percentile_rank(None, &peers), 0.5);
        assert_eq!(percentile_rank(Some(f64::NAN), &peers), 0.5);
        assert_eq!(percentile_rank(Some(1.0), &[5.0]), 0.5);
        assert_eq!(percentile_rank(Some(5.0), &[5.0, 5.0, 5.0]), 0.5);
    }

    #[test]
    fn missing_stat_sits_on_median() {
        let roster = [
            fighter(
                "A",
                None,
                Some(0.4),
                Some(9.0),
                Some(0.5),
                Some(1.0),
                Some(0.6),
                Some(0.2),
            ),
            fighter(
                "B",
                Some(4.0),
                Some(0.5),
                Some(3.0),
                Some(0.6),
                Some(1.5),
                Some(0.7),
                Some(0.3),
            ),
            fighter(
                "C",
                Some(6.0),
                Some(0.6),
                Some(1.0),
                Some(0.7),
                Some(2.0),
                Some(0.8),
                Some(0.4),
            ),
        ];
        let series = normalize_fighters(&[&roster[0]], &roster);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].values.len(), 6);
        assert_eq!(series[0].values[0], 0.5);
        assert_eq!(series[0].values[1], 0.0);
    }

    #[test]
    fn sapm_does_not_change_the_shape() {
        let a = fighter(
            "A",
            Some(3.0),
            Some(0.5),
            Some(99.0),
            Some(0.5),
            Some(1.0),
            Some(0.5),
            Some(0.2),
        );
        let b = fighter(
            "B",
            Some(3.0),
            Some(0.5),
            Some(0.01),
            Some(0.5),
            Some(1.0),
            Some(0.5),
            Some(0.2),
        );
        let roster = [a.clone(), b.clone()];
        let series = normalize_fighters(&[&roster[0], &roster[1]], &roster);
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].values, series[1].values);
    }

    #[test]
    fn two_fighter_series_rank_against_roster() {
        let roster = [
            fighter(
                "Low",
                Some(1.0),
                Some(0.2),
                Some(8.0),
                Some(0.2),
                Some(0.1),
                Some(0.2),
                Some(0.0),
            ),
            fighter(
                "Mid",
                Some(3.0),
                Some(0.5),
                Some(4.0),
                Some(0.5),
                Some(1.0),
                Some(0.5),
                Some(0.4),
            ),
            fighter(
                "High",
                Some(5.0),
                Some(0.8),
                Some(1.0),
                Some(0.8),
                Some(2.0),
                Some(0.8),
                Some(0.8),
            ),
        ];
        let series = normalize_fighters(&[&roster[0], &roster[2]], &roster);
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].name, "Low");
        assert_eq!(series[1].name, "High");
        assert!(series[0].values.iter().all(|&v| (v - 0.0).abs() < 1e-12));
        assert!(series[1].values.iter().all(|&v| (v - 1.0).abs() < 1e-12));
        let overlay = build_overlay(&["Low".into(), "High".into()], &roster).unwrap();
        assert!(overlay.title.contains("Radar shape"));
        assert!(overlay.title.contains("Low vs High"));
        assert_eq!(overlay.series.len(), 2);
    }
}
