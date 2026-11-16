//! MiniHabits' product rules, independent of Leptos, HTTP, and persistence.
//!
//! A floor is a real check-in, not a broken streak. Momentum is a weighted
//! trailing window; resilience measures days kept in that window. Callers own
//! clocks and storage, so the same rules run on the server and in the browser.

use chrono::{DateTime, Datelike, Days, NaiveDate, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

pub const DEFAULT_WINDOW_DAYS: usize = 30;
pub const MAX_WINDOW_DAYS: usize = 365;
pub const REVIEW_WINDOW_DAYS: usize = 7;
pub const ANCHOR_SUGGESTIONS: [&str; 6] = [
    "After morning coffee",
    "On your commute",
    "At lunch",
    "After you sit at your desk",
    "Before bed",
    "Right after a workout",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl ValidationError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ValidationError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Floor,
    Lift,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Floor => "floor",
            Self::Lift => "lift",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Floor => "Show up",
            Self::Lift => "Go full",
        }
    }
}

impl std::str::FromStr for Tier {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "floor" => Ok(Self::Floor),
            "lift" => Ok(Self::Lift),
            _ => Err(ValidationError::new("tier", "Choose floor or lift.")),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DayState {
    Floor,
    Lift,
    #[default]
    Miss,
}

impl DayState {
    pub fn tier(self) -> Option<Tier> {
        match self {
            Self::Floor => Some(Tier::Floor),
            Self::Lift => Some(Tier::Lift),
            Self::Miss => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Floor => "Showed up",
            Self::Lift => "Went full",
            Self::Miss => "Not logged",
        }
    }
}

impl From<Tier> for DayState {
    fn from(tier: Tier) -> Self {
        match tier {
            Tier::Floor => Self::Floor,
            Tier::Lift => Self::Lift,
        }
    }
}

/// Every builder field is required. The anchor is the existing routine cue.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemDraft {
    pub name: String,
    pub anchor: String,
    pub floor: String,
    pub lift: String,
}

impl SystemDraft {
    pub fn validate(&self) -> Result<(), ValidationError> {
        for (field, value, message) in [
            ("name", &self.name, "Give this system a name first."),
            ("anchor", &self.anchor, "Pick where it rides along."),
            (
                "floor",
                &self.floor,
                "Describe the floor: the smallest version that still counts.",
            ),
            ("lift", &self.lift, "Describe the lift: the fuller version."),
        ] {
            if value.trim().is_empty() {
                return Err(ValidationError::new(field, message));
            }
        }
        Ok(())
    }

    /// Normalize at the write boundary, not separately in each UI or route.
    pub fn normalized(&self) -> Result<Self, ValidationError> {
        self.validate()?;
        Ok(Self {
            name: self.name.trim().to_owned(),
            anchor: self.anchor.trim().to_owned(),
            floor: self.floor.trim().to_owned(),
            lift: self.lift.trim().to_owned(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MomentumConfig {
    pub window_days: usize,
    pub floor_weight: f64,
    pub lift_weight: f64,
    pub floor_keeps_momentum: bool,
    pub allowed_misses: usize,
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self {
            window_days: DEFAULT_WINDOW_DAYS,
            floor_weight: 1.0,
            lift_weight: 2.0,
            floor_keeps_momentum: true,
            allowed_misses: 0,
        }
    }
}

impl MomentumConfig {
    /// Mirrors the original settings contract, including allowing zero weights.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !(1..=MAX_WINDOW_DAYS).contains(&self.window_days) {
            return Err(ValidationError::new(
                "window_days",
                "Choose a window from 1 to 365 days.",
            ));
        }
        for (field, weight) in [
            ("floor_weight", self.floor_weight),
            ("lift_weight", self.lift_weight),
        ] {
            if !weight.is_finite() || !(0.0..=100.0).contains(&weight) {
                return Err(ValidationError::new(
                    field,
                    "Choose a finite weight from 0 to 100.",
                ));
            }
        }
        if self.allowed_misses > MAX_WINDOW_DAYS {
            return Err(ValidationError::new(
                "allowed_misses",
                "Choose an allowance from 0 to 365 days.",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct System {
    pub id: String,
    pub name: String,
    pub anchor: String,
    pub floor: String,
    pub lift: String,
    pub time_zone: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub momentum: MomentumConfig,
}

impl System {
    pub fn new(
        id: String,
        draft: SystemDraft,
        time_zone: String,
        created_at: DateTime<Utc>,
    ) -> Result<Self, ValidationError> {
        let draft = draft.normalized()?;
        let system = Self {
            id,
            name: draft.name,
            anchor: draft.anchor,
            floor: draft.floor,
            lift: draft.lift,
            time_zone,
            created_at,
            archived: false,
            momentum: MomentumConfig::default(),
        };
        system.validate()?;
        Ok(system)
    }

    pub fn draft(&self) -> SystemDraft {
        SystemDraft {
            name: self.name.clone(),
            anchor: self.anchor.clone(),
            floor: self.floor.clone(),
            lift: self.lift.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.id.trim().is_empty() {
            return Err(ValidationError::new("id", "A system needs an identifier."));
        }
        self.draft().validate()?;
        parse_time_zone(&self.time_zone)?;
        self.momentum.validate()
    }

    pub fn update(&mut self, draft: SystemDraft) -> Result<(), ValidationError> {
        let draft = draft.normalized()?;
        self.name = draft.name;
        self.anchor = draft.anchor;
        self.floor = draft.floor;
        self.lift = draft.lift;
        Ok(())
    }
}

/// There is one live entry per (system_id, day); undo removes that entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub system_id: String,
    pub day: NaiveDate,
    pub tier: Tier,
    pub logged_at: DateTime<Utc>,
}

/// Set today's tier, or undo with None. Repeating the same request is safe.
///
/// The original app permits logging a newly created system immediately. Its
/// builder's "starts tomorrow" copy did not impose a logging restriction.
pub fn apply_check_in(
    entries: &mut Vec<Entry>,
    system: &System,
    tier: Option<Tier>,
    at: DateTime<Utc>,
) -> Result<(), ValidationError> {
    if system.archived {
        return Err(ValidationError::new(
            "system_id",
            "Restore this system before checking in.",
        ));
    }
    let day = day_in_timezone(at, &system.time_zone)?;
    entries.retain(|entry| entry.system_id != system.id || entry.day != day);
    if let Some(tier) = tier {
        entries.push(Entry {
            system_id: system.id.clone(),
            day,
            tier,
            logged_at: at,
        });
    }
    Ok(())
}

/// Parse a real, four-digit ISO calendar date. Never accept relaxed forms.
pub fn parse_day(value: &str) -> Result<NaiveDate, ValidationError> {
    let bytes = value.as_bytes();
    let shape_valid = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
    if shape_valid && let Ok(day) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Ok(day);
    }
    Err(ValidationError::new(
        "day",
        "Choose a real date in YYYY-MM-DD form.",
    ))
}

pub fn parse_time_zone(value: &str) -> Result<Tz, ValidationError> {
    value.parse().map_err(|_| {
        ValidationError::new(
            "time_zone",
            "Choose a valid IANA time zone, such as Europe/Paris.",
        )
    })
}

/// Resolve an instant to a local calendar day using the IANA timezone database.
pub fn day_in_timezone(at: DateTime<Utc>, time_zone: &str) -> Result<NaiveDate, ValidationError> {
    Ok(at.with_timezone(&parse_time_zone(time_zone)?).date_naive())
}

/// Clock-taking helpers above remain the preferred API for deterministic tests.
pub fn today_in_timezone(time_zone: &str) -> Result<NaiveDate, ValidationError> {
    day_in_timezone(Utc::now(), time_zone)
}

/// Monday of the containing ISO week. Saturates only at chrono's minimum date,
/// which is outside the four-digit dates accepted at our external boundaries.
pub fn week_start_of(day: NaiveDate) -> NaiveDate {
    day.checked_sub_days(Days::new(u64::from(day.weekday().num_days_from_monday())))
        .unwrap_or(NaiveDate::MIN)
}

pub fn trailing_window(
    end: NaiveDate,
    window_days: usize,
) -> Result<Vec<NaiveDate>, ValidationError> {
    if window_days > MAX_WINDOW_DAYS {
        return Err(ValidationError::new(
            "window_days",
            "Choose a window no longer than 365 days.",
        ));
    }
    (0..window_days)
        .rev()
        .map(|back| {
            end.checked_sub_days(Days::new(back as u64)).ok_or_else(|| {
                ValidationError::new("day", "The date window is outside the supported calendar.")
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MomentumDay {
    pub day: NaiveDate,
    pub state: DayState,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayStateSplit {
    pub lift: usize,
    pub floor: usize,
    pub miss: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Momentum {
    pub series: Vec<MomentumDay>,
    pub window_days: usize,
    pub kept: usize,
    pub split: DayStateSplit,
    pub score: f64,
    pub resilience: f64,
}

/// Re-bucket logged instants in the system's zone, exactly like the original
/// core engine. Duplicate imports cannot double-count a day: a lift wins.
/// Other systems and entries outside the selected window never affect totals.
pub fn compute_momentum(
    system: &System,
    entries: &[Entry],
    end: NaiveDate,
    window_override: Option<usize>,
) -> Result<Momentum, ValidationError> {
    system.momentum.validate()?;
    let zone = parse_time_zone(&system.time_zone)?;
    let days = trailing_window(end, window_override.unwrap_or(system.momentum.window_days))?;
    let mut by_day = BTreeMap::new();
    for entry in entries.iter().filter(|entry| entry.system_id == system.id) {
        let day = entry.logged_at.with_timezone(&zone).date_naive();
        let state = by_day.entry(day).or_insert(DayState::Miss);
        if *state != DayState::Lift {
            *state = entry.tier.into();
        }
    }
    let series: Vec<_> = days
        .into_iter()
        .map(|day| MomentumDay {
            day,
            state: by_day.get(&day).copied().unwrap_or_default(),
        })
        .collect();
    let mut split = DayStateSplit::default();
    for cell in &series {
        match cell.state {
            DayState::Floor => split.floor += 1,
            DayState::Lift => split.lift += 1,
            DayState::Miss => split.miss += 1,
        }
    }
    let config = &system.momentum;
    let window_days = series.len();
    let kept = split.lift
        + if config.floor_keeps_momentum {
            split.floor
        } else {
            0
        };
    let forgiven = config.allowed_misses.min(window_days - kept);
    let resilience = if window_days == 0 {
        0.0
    } else {
        (kept + forgiven) as f64 / window_days as f64 * 100.0
    };
    // Like the source, custom floor > lift weights may produce a score > 100.
    // Default weights are 1/2, so normal scores stay in the familiar 0..100.
    let score = if window_days == 0 || config.lift_weight == 0.0 {
        0.0
    } else {
        (split.lift as f64 * config.lift_weight + split.floor as f64 * config.floor_weight)
            / (window_days as f64 * config.lift_weight)
            * 100.0
    };
    Ok(Momentum {
        series,
        window_days,
        kept,
        split,
        score,
        resilience,
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SystemRecap {
    pub system_id: String,
    pub name: String,
    pub anchor: String,
    pub window_days: usize,
    pub kept: usize,
    pub resilience: f64,
    pub score: f64,
    pub split: DayStateSplit,
    pub series: Vec<MomentumDay>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SuggestionKind {
    LowerFloor,
    TryLift,
    AddPlan,
    RaiseLift,
    Celebrate,
}

impl SuggestionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LowerFloor => "lower-floor",
            Self::TryLift => "try-lift",
            Self::AddPlan => "add-plan",
            Self::RaiseLift => "raise-lift",
            Self::Celebrate => "celebrate",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSuggestion {
    pub id: String,
    pub system_id: Option<String>,
    pub kind: SuggestionKind,
    pub head: String,
    pub body: String,
}

/// At most one gentle suggestion per system. Ordering is part of the product:
/// a small floor is a success, and the review suggests changes, never demands.
pub fn suggest_for(recap: &SystemRecap) -> Option<ReviewSuggestion> {
    let split = &recap.split;
    let name = &recap.name;
    let (kind, head, body) = if split.miss >= 4 {
        (
            SuggestionKind::LowerFloor,
            format!("Make {name} easier to keep"),
            "More misses than keeps this week. A smaller floor is easier to hit on a hard day.",
        )
    } else if split.lift == 0 && split.floor >= 3 {
        (
            SuggestionKind::TryLift,
            format!("You're showing up for {name}"),
            "Lots of floors, no lifts. When you've got the energy, reach for one lift this week.",
        )
    } else if (1..=3).contains(&split.miss) {
        (
            SuggestionKind::AddPlan,
            format!("Plan around the misses on {name}"),
            "A couple of off days. An if/then plan for the obstacle keeps a slip from becoming a stop.",
        )
    } else if recap.resilience >= 85.0 && split.lift >= 4 {
        (
            SuggestionKind::RaiseLift,
            format!("{name} is humming"),
            "Strong week. If it feels light, consider raising the lift a notch.",
        )
    } else if recap.resilience == 100.0 {
        (
            SuggestionKind::Celebrate,
            format!("A perfect week on {name}"),
            "Every day kept. Notice what made it work; that's the system doing its job.",
        )
    } else {
        return None;
    };
    Some(ReviewSuggestion {
        id: format!("{}:{}", kind.as_str(), recap.system_id),
        system_id: Some(recap.system_id.clone()),
        kind,
        head,
        body: body.to_owned(),
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeeklyReview {
    pub week_start: NaiveDate,
    pub week_end: NaiveDate,
    pub systems: Vec<SystemRecap>,
    pub suggestions: Vec<ReviewSuggestion>,
}

/// Monday through the as-of day, never future days in the current week.
/// Systems remain in caller-supplied display order; archived systems are omitted.
pub fn build_weekly_review(
    systems: &[System],
    entries: &[Entry],
    end: NaiveDate,
) -> Result<WeeklyReview, ValidationError> {
    let week_start = week_start_of(end);
    let window_days = ((end - week_start).num_days() as usize + 1).min(REVIEW_WINDOW_DAYS);
    let systems = systems
        .iter()
        .filter(|system| !system.archived)
        .map(|system| {
            let momentum = compute_momentum(system, entries, end, Some(window_days))?;
            Ok(SystemRecap {
                system_id: system.id.clone(),
                name: system.name.clone(),
                anchor: system.anchor.clone(),
                window_days: momentum.window_days,
                kept: momentum.kept,
                resilience: momentum.resilience,
                score: momentum.score,
                split: momentum.split,
                series: momentum.series,
            })
        })
        .collect::<Result<Vec<_>, ValidationError>>()?;
    let suggestions = systems.iter().filter_map(suggest_for).collect();
    Ok(WeeklyReview {
        week_start,
        week_end: end,
        systems,
        suggestions,
    })
}

/// The weekly reflection is deliberately separate from check-ins. There is no
/// daily note or public-sharing entity in this focused app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reflection {
    pub week_start: NaiveDate,
    #[serde(default)]
    pub worked: String,
    #[serde(default)]
    pub obstacles: String,
    #[serde(default)]
    pub adjustment: String,
}

impl Reflection {
    pub fn empty(week_start: NaiveDate) -> Self {
        Self {
            week_start,
            worked: String::new(),
            obstacles: String::new(),
            adjustment: String::new(),
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.week_start.weekday() != chrono::Weekday::Mon {
            return Err(ValidationError::new(
                "week_start",
                "A weekly reflection starts on Monday.",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(value: &str) -> NaiveDate {
        parse_day(value).unwrap()
    }
    fn instant(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }
    fn system() -> System {
        System::new(
            "reading".into(),
            SystemDraft {
                name: "Become a reader".into(),
                anchor: "After morning coffee".into(),
                floor: "Read one page".into(),
                lift: "Read twenty pages".into(),
            },
            "UTC".into(),
            instant("2026-06-01T08:00:00Z"),
        )
        .unwrap()
    }
    fn entry(at: &str, tier: Tier) -> Entry {
        let logged_at = instant(at);
        Entry {
            system_id: "reading".into(),
            day: logged_at.date_naive(),
            tier,
            logged_at,
        }
    }
    fn week_entries(tier: Tier, count: usize) -> Vec<Entry> {
        (1..=count)
            .map(|index| entry(&format!("2026-06-{index:02}T10:00:00Z"), tier))
            .collect()
    }
    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn builder_requires_each_field_and_normalizes_whitespace() {
        let original = system().draft();
        for field in ["name", "anchor", "floor", "lift"] {
            let mut draft = original.clone();
            match field {
                "name" => draft.name = " \n ".into(),
                "anchor" => draft.anchor.clear(),
                "floor" => draft.floor.clear(),
                _ => draft.lift.clear(),
            }
            assert_eq!(draft.validate().unwrap_err().field, field);
        }
        let mut draft = original.clone();
        draft.name = format!("  {} \n", draft.name);
        assert_eq!(draft.normalized().unwrap(), original);
    }

    #[test]
    fn dates_are_strict_and_calendar_valid() {
        assert!(parse_day("2024-02-29").is_ok());
        for invalid in [
            "2026-02-29",
            "2026-04-31",
            "2026-13-01",
            "2026-00-01",
            "2026-01-00",
            "2026-1-01",
            "26-01-01",
            "2026-01-01T00:00:00Z",
            " 2026-01-01",
            "🪴",
        ] {
            assert!(parse_day(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn trailing_windows_cross_leap_day_and_year_boundary() {
        assert_eq!(
            trailing_window(day("2024-03-01"), 3).unwrap(),
            vec![day("2024-02-28"), day("2024-02-29"), day("2024-03-01")]
        );
        assert_eq!(
            trailing_window(day("2026-01-01"), 2).unwrap(),
            vec![day("2025-12-31"), day("2026-01-01")]
        );
        assert!(trailing_window(day("2026-01-01"), 0).unwrap().is_empty());
        assert!(trailing_window(day("2026-01-01"), usize::MAX).is_err());
        assert!(trailing_window(NaiveDate::MIN, 2).is_err());
    }

    #[test]
    fn time_zones_resolve_midnight_and_dst_without_utc_day_assumptions() {
        let at = instant("2026-06-01T23:30:00Z");
        assert_eq!(
            day_in_timezone(at, "Europe/Paris").unwrap(),
            day("2026-06-02")
        );
        assert_eq!(
            day_in_timezone(at, "America/New_York").unwrap(),
            day("2026-06-01")
        );
        assert_eq!(
            day_in_timezone(instant("2026-03-08T04:30:00Z"), "America/New_York").unwrap(),
            day("2026-03-07")
        );
        assert_eq!(
            day_in_timezone(instant("2026-03-08T16:00:00Z"), "America/New_York").unwrap(),
            day("2026-03-08")
        );
        assert!(day_in_timezone(at, "not/a-time-zone").is_err());
    }

    #[test]
    fn new_system_can_show_up_then_go_full_then_undo_today() {
        let system = system();
        let now = system.created_at;
        let mut entries = vec![];
        apply_check_in(&mut entries, &system, Some(Tier::Floor), now).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tier, Tier::Floor);
        apply_check_in(&mut entries, &system, Some(Tier::Floor), now).unwrap();
        assert_eq!(entries.len(), 1, "retries do not duplicate entries");
        apply_check_in(&mut entries, &system, Some(Tier::Lift), now).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tier, Tier::Lift);
        apply_check_in(&mut entries, &system, None, now).unwrap();
        apply_check_in(&mut entries, &system, None, now).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn undo_is_scoped_to_the_system_and_its_local_today() {
        let mut system = system();
        system.time_zone = "Europe/Paris".into();
        let yesterday = entry("2026-06-01T10:00:00Z", Tier::Floor);
        let mut other = entry("2026-06-01T23:30:00Z", Tier::Lift);
        other.system_id = "writing".into();
        let mut entries = vec![yesterday.clone(), other.clone()];
        let now = instant("2026-06-01T23:30:00Z");
        apply_check_in(&mut entries, &system, Some(Tier::Lift), now).unwrap();
        assert_eq!(entries.last().unwrap().day, day("2026-06-02"));
        apply_check_in(&mut entries, &system, None, now).unwrap();
        assert_eq!(entries, vec![yesterday, other]);
    }

    #[test]
    fn invalid_or_archived_check_in_does_not_modify_entries() {
        let mut system = system();
        let original = vec![entry("2026-06-01T10:00:00Z", Tier::Floor)];
        let mut entries = original.clone();
        system.archived = true;
        assert!(apply_check_in(&mut entries, &system, None, system.created_at).is_err());
        assert_eq!(entries, original);
        system.archived = false;
        system.time_zone = "invalid".into();
        assert!(apply_check_in(&mut entries, &system, None, system.created_at).is_err());
        assert_eq!(entries, original);
    }

    #[test]
    fn default_window_has_thirty_days_and_empty_history_is_zero() {
        let momentum = compute_momentum(&system(), &[], day("2026-06-07"), None).unwrap();
        assert_eq!(momentum.window_days, 30);
        assert_eq!(momentum.split.miss, 30);
        assert_eq!(momentum.kept, 0);
        close(momentum.score, 0.0);
        close(momentum.resilience, 0.0);
    }

    #[test]
    fn all_floors_count_fully_for_resilience_but_half_for_momentum() {
        let momentum = compute_momentum(
            &system(),
            &week_entries(Tier::Floor, 7),
            day("2026-06-07"),
            Some(7),
        )
        .unwrap();
        assert_eq!(momentum.kept, 7);
        assert_eq!(
            momentum.split,
            DayStateSplit {
                floor: 7,
                lift: 0,
                miss: 0
            }
        );
        close(momentum.resilience, 100.0);
        close(momentum.score, 50.0);
    }

    #[test]
    fn a_miss_removes_only_its_share_and_never_resets_progress() {
        let momentum = compute_momentum(
            &system(),
            &week_entries(Tier::Lift, 6),
            day("2026-06-07"),
            Some(7),
        )
        .unwrap();
        assert_eq!(momentum.split.miss, 1);
        close(momentum.resilience, 600.0 / 7.0);
        close(momentum.score, 600.0 / 7.0);
    }

    #[test]
    fn mixed_tiers_score_using_original_weighted_formula() {
        let entries = vec![
            entry("2026-06-01T10:00:00Z", Tier::Lift),
            entry("2026-06-02T10:00:00Z", Tier::Floor),
        ];
        let momentum = compute_momentum(&system(), &entries, day("2026-06-03"), Some(3)).unwrap();
        assert_eq!(momentum.kept, 2);
        close(momentum.score, 50.0);
        close(momentum.resilience, 200.0 / 3.0);
    }

    #[test]
    fn duplicate_imports_prefer_lift_regardless_of_order() {
        let mut entries = vec![
            entry("2026-06-01T09:00:00Z", Tier::Floor),
            entry("2026-06-01T10:00:00Z", Tier::Lift),
        ];
        for _ in 0..2 {
            let momentum =
                compute_momentum(&system(), &entries, day("2026-06-01"), Some(1)).unwrap();
            assert_eq!(momentum.split.lift, 1);
            assert_eq!(momentum.split.floor, 0);
            entries.reverse();
        }
    }

    #[test]
    fn stats_use_local_logged_instant_not_untrusted_stored_day() {
        let mut system = system();
        system.time_zone = "Europe/Paris".into();
        let mut entry = entry("2026-06-01T23:30:00Z", Tier::Lift);
        entry.day = day("2000-01-01");
        let momentum = compute_momentum(&system, &[entry], day("2026-06-02"), Some(2)).unwrap();
        assert_eq!(momentum.series[0].state, DayState::Miss);
        assert_eq!(momentum.series[1].state, DayState::Lift);
    }

    #[test]
    fn other_systems_and_out_of_window_entries_do_not_count() {
        let mut other = entry("2026-06-01T10:00:00Z", Tier::Lift);
        other.system_id = "writing".into();
        let entries = vec![
            other,
            entry("2026-05-31T10:00:00Z", Tier::Lift),
            entry("2026-06-03T10:00:00Z", Tier::Lift),
        ];
        let momentum = compute_momentum(&system(), &entries, day("2026-06-02"), Some(2)).unwrap();
        assert_eq!(momentum.kept, 0);
        assert_eq!(momentum.split.miss, 2);
    }

    #[test]
    fn grace_and_floor_setting_change_resilience_not_weighted_score() {
        let mut system = system();
        system.momentum.floor_keeps_momentum = false;
        system.momentum.allowed_misses = 2;
        let entries = week_entries(Tier::Floor, 7);
        let momentum = compute_momentum(&system, &entries, day("2026-06-07"), Some(7)).unwrap();
        assert_eq!(momentum.kept, 0);
        close(momentum.resilience, 200.0 / 7.0);
        close(momentum.score, 50.0);
        system.momentum.allowed_misses = 365;
        close(
            compute_momentum(&system, &entries, day("2026-06-07"), Some(7))
                .unwrap()
                .resilience,
            100.0,
        );
    }

    #[test]
    fn empty_windows_and_zero_lift_weight_never_divide_by_zero() {
        let mut system = system();
        let empty = compute_momentum(&system, &[], day("2026-06-07"), Some(0)).unwrap();
        close(empty.score, 0.0);
        close(empty.resilience, 0.0);
        system.momentum.lift_weight = 0.0;
        close(
            compute_momentum(
                &system,
                &week_entries(Tier::Floor, 7),
                day("2026-06-07"),
                Some(7),
            )
            .unwrap()
            .score,
            0.0,
        );
    }

    #[test]
    fn invalid_config_is_rejected_instead_of_panicking_or_propagating_nan() {
        let mut config = MomentumConfig {
            window_days: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
        config.window_days = 365;
        config.floor_weight = f64::NAN;
        assert!(config.validate().is_err());
        config.floor_weight = 1.0;
        config.lift_weight = f64::INFINITY;
        assert!(config.validate().is_err());
        config.lift_weight = -1.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn iso_week_start_handles_monday_sunday_and_year_boundary() {
        assert_eq!(week_start_of(day("2026-01-05")), day("2026-01-05"));
        assert_eq!(week_start_of(day("2026-01-07")), day("2026-01-05"));
        assert_eq!(week_start_of(day("2026-01-11")), day("2026-01-05"));
        assert_eq!(week_start_of(day("2026-01-01")), day("2025-12-29"));
    }

    #[test]
    fn unfinished_week_does_not_punish_days_that_have_not_happened() {
        let entries = week_entries(Tier::Floor, 3);
        let review = build_weekly_review(&[system()], &entries, day("2026-06-03")).unwrap();
        assert_eq!(review.week_start, day("2026-06-01"));
        assert_eq!(review.week_end, day("2026-06-03"));
        assert_eq!(review.systems[0].window_days, 3);
        assert_eq!(review.systems[0].split.miss, 0);
        close(review.systems[0].resilience, 100.0);
        let completed = build_weekly_review(&[system()], &entries, day("2026-06-07")).unwrap();
        assert_eq!(completed.systems[0].window_days, 7);
        assert_eq!(completed.systems[0].split.miss, 4);
    }

    #[test]
    fn review_omits_archived_systems_preserves_order_and_supports_empty_state() {
        let first = system();
        let mut archived = first.clone();
        archived.id = "archived".into();
        archived.archived = true;
        let mut last = first.clone();
        last.id = "writing".into();
        let review = build_weekly_review(&[first, archived, last], &[], day("2026-06-03")).unwrap();
        assert_eq!(
            review
                .systems
                .iter()
                .map(|item| item.system_id.as_str())
                .collect::<Vec<_>>(),
            ["reading", "writing"]
        );
        let empty = build_weekly_review(&[], &[], day("2026-06-03")).unwrap();
        assert!(empty.systems.is_empty());
        assert!(empty.suggestions.is_empty());
    }

    #[test]
    fn suggestion_thresholds_and_priority_match_the_original_review() {
        let mut recap = build_weekly_review(&[system()], &[], day("2026-06-07"))
            .unwrap()
            .systems
            .remove(0);
        for (lift, floor, miss, resilience, expected) in [
            (0, 3, 4, 42.0, SuggestionKind::LowerFloor),
            (0, 5, 2, 71.0, SuggestionKind::TryLift),
            (4, 1, 2, 100.0, SuggestionKind::AddPlan),
            (4, 3, 0, 100.0, SuggestionKind::RaiseLift),
            (3, 4, 0, 100.0, SuggestionKind::Celebrate),
        ] {
            recap.split = DayStateSplit { lift, floor, miss };
            recap.resilience = resilience;
            assert_eq!(suggest_for(&recap).unwrap().kind, expected);
        }
        recap.split = DayStateSplit::default();
        recap.resilience = 0.0;
        assert!(suggest_for(&recap).is_none());
    }

    #[test]
    fn reflection_is_keyed_to_monday_and_keeps_all_three_prompts() {
        let mut reflection = Reflection::empty(day("2026-06-01"));
        assert!(reflection.validate().is_ok());
        reflection.worked = "Kept a book beside the kettle.".into();
        reflection.obstacles = "Late mornings.".into();
        reflection.adjustment = "Read the floor before checking email.".into();
        assert!(reflection.validate().is_ok());
        reflection.week_start = day("2026-06-02");
        assert_eq!(reflection.validate().unwrap_err().field, "week_start");
    }
}
