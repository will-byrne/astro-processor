//! Edit history: a list of named snapshots of the processing parameters with
//! a cursor. Going back keeps the later steps (so they can be returned to)
//! until a new edit is made from the earlier point, which replaces them.

use super::params::{CurvePoints, Level, ProcessParams};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Oldest steps are dropped beyond this many.
pub const MAX_STEPS: usize = 300;

/// Repeated changes to one setting within this window (arrow keys, scroll
/// wheel, nudging a slider) are folded into a single step.
const MERGE_WINDOW: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub label: String,
    /// When the step was made, in seconds since the Unix epoch.
    pub time: u64,
    pub params: ProcessParams,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct History {
    steps: Vec<Step>,
    current: usize,
    /// Setting changed by the newest step and when, for merging.
    #[serde(skip)]
    recent: Option<(String, Instant)>,
}

impl PartialEq for History {
    fn eq(&self, other: &Self) -> bool {
        self.steps == other.steps && self.current == other.current
    }
}

impl History {
    /// A history holding only its starting point.
    pub fn new(label: &str, params: ProcessParams, time: u64) -> Self {
        Self { steps: vec![Step { label: label.into(), time, params }], current: 0, recent: None }
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Index of the step the image currently shows.
    pub fn current(&self) -> usize {
        self.current
    }

    pub fn current_params(&self) -> Option<&ProcessParams> {
        self.steps.get(self.current).map(|s| &s.params)
    }

    /// Whether this history's current step describes `params` (false for an
    /// empty or stale history, e.g. from a project edited by other means).
    pub fn matches(&self, params: &ProcessParams) -> bool {
        self.current_params() == Some(params)
    }

    pub fn can_undo(&self) -> bool {
        self.current > 0
    }

    pub fn can_redo(&self) -> bool {
        self.current + 1 < self.steps.len()
    }

    /// Move to step `index` and return its parameters.
    pub fn jump(&mut self, index: usize) -> Option<&ProcessParams> {
        if index >= self.steps.len() {
            return None;
        }
        self.current = index;
        self.recent = None;
        Some(&self.steps[index].params)
    }

    pub fn undo(&mut self) -> Option<&ProcessParams> {
        if self.can_undo() { self.jump(self.current - 1) } else { None }
    }

    pub fn redo(&mut self) -> Option<&ProcessParams> {
        if self.can_redo() { self.jump(self.current + 1) } else { None }
    }

    /// Record that the parameters are now `params`. `action` names the edit
    /// when the caller knows better than a field-by-field description ("Auto
    /// stretch"). Steps after the current one are discarded. Returns false
    /// if nothing changed.
    pub fn record(&mut self, params: &ProcessParams, action: Option<&str>, time: u64, now: Instant) -> bool {
        let Some(current) = self.current_params() else {
            *self = Self::new(action.unwrap_or("Start"), params.clone(), time);
            return true;
        };
        if current == params {
            return false;
        }
        let change = describe(current, params);
        if !change.visible {
            // Not an edit: keep the step exact so it is not seen again.
            self.steps[self.current].params = params.clone();
            return false;
        }
        let at_tip = self.current + 1 == self.steps.len();
        let merge = action.is_none()
            && at_tip
            && self.current > 0
            && change.key.is_some()
            && self.recent.as_ref().is_some_and(|(k, t)| Some(k) == change.key.as_ref() && now.duration_since(*t) < MERGE_WINDOW);
        if merge {
            // Describe the combined change from the step before.
            let before = &self.steps[self.current - 1].params;
            if before == params {
                self.steps.pop();
                self.current -= 1;
                self.recent = None;
            } else {
                let label = describe(before, params).label;
                let step = &mut self.steps[self.current];
                step.label = label;
                step.time = time;
                step.params = params.clone();
                self.recent = change.key.map(|k| (k, now));
            }
            return true;
        }
        self.steps.truncate(self.current + 1);
        let label = action.map(str::to_owned).unwrap_or(change.label);
        self.steps.push(Step { label, time, params: params.clone() });
        if self.steps.len() > MAX_STEPS {
            let extra = self.steps.len() - MAX_STEPS;
            self.steps.drain(..extra);
        }
        self.current = self.steps.len() - 1;
        self.recent = if action.is_none() { change.key.map(|k| (k, now)) } else { None };
        true
    }
}

/// What differs between two parameter sets, in words.
pub struct Change {
    pub label: String,
    /// False when every changed number still reads the same at the
    /// precision it is shown with (a slider rounding its value).
    pub visible: bool,
    /// The one setting that changed, when it was only one.
    pub key: Option<String>,
}

struct Diff {
    section: &'static str,
    name: String,
    /// Old and new value, when a value is worth showing (the old one is
    /// empty for switches).
    values: Option<(String, String)>,
}

struct Diffs(Vec<Diff>);

impl Diffs {
    fn num(&mut self, section: &'static str, name: &str, a: f32, b: f32, decimals: usize) {
        if a != b {
            let values = Some((format!("{a:.decimals$}"), format!("{b:.decimals$}")));
            self.0.push(Diff { section, name: name.into(), values });
        }
    }

    fn percent(&mut self, section: &'static str, name: &str, a: f32, b: f32) {
        if a != b {
            let values = Some((format!("{:.1}%", a * 100.0), format!("{:.1}%", b * 100.0)));
            self.0.push(Diff { section, name: name.into(), values });
        }
    }

    fn flag(&mut self, section: &'static str, name: &str, a: bool, b: bool) {
        if a != b {
            let word = if b { "on" } else { "off" };
            self.0.push(Diff { section, name: name.into(), values: Some((String::new(), word.into())) });
        }
    }

    fn level(&mut self, channel: &str, a: &Level, b: &Level) {
        self.num("Levels", &format!("{channel} levels black"), a.black, b.black, 2);
        self.num("Levels", &format!("{channel} levels gamma"), a.gamma, b.gamma, 2);
        self.num("Levels", &format!("{channel} levels white"), a.white, b.white, 2);
    }

    fn curve(&mut self, channel: &str, a: &CurvePoints, b: &CurvePoints) {
        if a != b {
            self.0.push(Diff { section: "Curves", name: format!("{channel} curve"), values: None });
        }
    }
}

/// Describe the change from `a` to `b`.
pub fn describe(a: &ProcessParams, b: &ProcessParams) -> Change {
    let mut d = Diffs(Vec::new());
    let mut reset: Vec<&'static str> = Vec::new();
    macro_rules! section {
        ($field:ident, $name:literal) => {
            if a.$field != b.$field && b.$field == Default::default() {
                reset.push($name);
            }
        };
    }
    section!(stretch, "Stretch");
    section!(tone, "Tone");
    section!(levels, "Levels");
    section!(curves, "Curves");
    section!(color, "Colour balance");
    section!(gradient, "Background gradient");
    section!(detail, "Noise reduction & sharpening");
    section!(stars, "Star reduction");
    section!(geometry, "Crop & rotate");

    let (x, y) = (&a.stretch, &b.stretch);
    d.num("Stretch", "Black point", x.black, y.black, 4);
    d.num("Stretch", "Midtones", x.midtone, y.midtone, 4);
    d.num("Stretch", "Arcsinh", x.asinh, y.asinh, 0);

    let (x, y) = (&a.tone, &b.tone);
    d.num("Tone", "Exposure", x.exposure, y.exposure, 2);
    d.num("Tone", "Contrast", x.contrast, y.contrast, 2);
    d.num("Tone", "Highlights", x.highlights, y.highlights, 2);
    d.num("Tone", "Shadows", x.shadows, y.shadows, 2);
    d.num("Tone", "Saturation", x.saturation, y.saturation, 2);
    d.num("Tone", "Green removal", x.green_removal, y.green_removal, 2);

    let (x, y) = (&a.levels, &b.levels);
    d.level("RGB", &x.master, &y.master);
    d.level("Red", &x.r, &y.r);
    d.level("Green", &x.g, &y.g);
    d.level("Blue", &x.b, &y.b);

    let (x, y) = (&a.curves, &b.curves);
    d.curve("RGB", &x.master, &y.master);
    d.curve("Red", &x.r, &y.r);
    d.curve("Green", &x.g, &y.g);
    d.curve("Blue", &x.b, &y.b);

    let (x, y) = (&a.color, &b.color);
    d.flag("Colour balance", "Neutral sky background", x.neutralize_background, y.neutralize_background);
    for (i, name) in ["Red balance", "Green balance", "Blue balance"].iter().enumerate() {
        d.num("Colour balance", name, x.wb[i], y.wb[i], 2);
    }

    let (x, y) = (&a.gradient, &b.gradient);
    d.flag("Background gradient", "Remove gradient", x.enabled, y.enabled);
    d.num("Background gradient", "Gradient complexity", x.degree as f32, y.degree as f32, 0);
    d.num("Background gradient", "Object protection", x.tolerance, y.tolerance, 2);
    d.num("Background gradient", "Gradient strength", x.strength, y.strength, 2);

    let (x, y) = (&a.detail, &b.detail);
    d.num("Noise reduction & sharpening", "Luminance noise", x.denoise_luma, y.denoise_luma, 2);
    d.num("Noise reduction & sharpening", "Colour noise", x.denoise_chroma, y.denoise_chroma, 2);
    d.num("Noise reduction & sharpening", "Sharpen", x.sharpen, y.sharpen, 2);

    let (x, y) = (&a.stars, &b.stars);
    d.num("Star reduction", "Star reduction", x.reduce, y.reduce, 2);
    d.num("Star reduction", "Star radius", x.radius, y.radius, 2);

    let (x, y) = (&a.geometry, &b.geometry);
    for (i, name) in ["Crop left", "Crop top", "Crop right", "Crop bottom"].iter().enumerate() {
        d.percent("Crop & rotate", name, x.crop[i], y.crop[i]);
    }
    if x.quarter_turns != y.quarter_turns {
        let name = match (y.quarter_turns + 4 - x.quarter_turns % 4) % 4 {
            1 => "Rotate right",
            3 => "Rotate left",
            _ => "Rotate 180°",
        };
        d.0.push(Diff { section: "Crop & rotate", name: name.into(), values: None });
    }
    d.flag("Crop & rotate", "Mirror", x.flip_h, y.flip_h);
    d.flag("Crop & rotate", "Flip", x.flip_v, y.flip_v);
    d.num("Crop & rotate", "Straighten", x.angle, y.angle, 1);

    let diffs = d.0;
    let one_section = diffs.first().map(|f| f.section).filter(|s| diffs.iter().all(|f| f.section == *s));
    let label = match (diffs.as_slice(), one_section) {
        ([], _) => "No change".to_owned(),
        ([one], _) => match &one.values {
            // The built-in font has no arrow glyph.
            Some((from, to)) if from.is_empty() => format!("{} {to}", one.name),
            Some((from, to)) => format!("{} {from} to {to}", one.name),
            None => one.name.clone(),
        },
        (_, Some(section)) if reset.contains(&section) => format!("Reset {}", lower_first(section)),
        ([first, second], _) => format!("{}, {}", first.name, lower_first(&second.name)),
        (many, Some(section)) => format!("{section}: {} settings", many.len()),
        (many, None) => format!("{} settings changed", many.len()),
    };
    let key = match diffs.as_slice() {
        // A rotation is a discrete action; each press is its own step.
        [one] if one.values.is_some() || one.section == "Curves" => Some(one.name.clone()),
        _ => None,
    };
    let visible = diffs.iter().any(|d| d.values.as_ref().is_none_or(|(from, to)| from != to));
    Change { label, visible, key }
}

/// Lower-case the first letter, leaving acronyms ("RGB curve") alone.
fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match (chars.next(), chars.clone().next()) {
        (Some(first), Some(second)) if second.is_lowercase() || second == ' ' => {
            first.to_lowercase().chain(chars).collect()
        }
        _ => s.to_owned(),
    }
}

/// "just now", "5 min ago", "3 h ago", "2 d ago".
pub fn ago(now: u64, then: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn later(t: Instant, secs: u64) -> Instant {
        t + Duration::from_secs(secs)
    }

    #[test]
    fn steps_are_recorded_and_can_be_revisited() {
        let t = Instant::now();
        let mut p = ProcessParams::default();
        let mut h = History::new("Stacked", p.clone(), 100);
        assert!(!h.record(&p, None, 101, t), "no change, no step");

        p.tone.contrast = 0.25;
        assert!(h.record(&p, None, 110, later(t, 10)));
        p.tone.saturation = 1.5;
        h.record(&p, None, 120, later(t, 20));
        p.stretch.black = 0.01;
        p.stretch.midtone = 0.02;
        h.record(&p, Some("Auto stretch"), 130, later(t, 30));
        let labels: Vec<_> = h.steps().iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["Stacked", "Contrast 0.00 to 0.25", "Saturation 1.00 to 1.50", "Auto stretch"]);
        assert_eq!(h.current(), 3);

        // Back to the contrast step: later steps stay until a new edit.
        let back = h.jump(1).unwrap().clone();
        assert_eq!(back.tone.contrast, 0.25);
        assert_eq!(back.tone.saturation, 1.0);
        assert_eq!(h.steps().len(), 4);
        assert!(h.can_redo());
        assert_eq!(h.redo().unwrap().tone.saturation, 1.5);
        assert_eq!(h.undo().unwrap().tone.saturation, 1.0);

        let mut q = back;
        q.tone.shadows = 0.3;
        h.record(&q, None, 140, later(t, 40));
        assert_eq!(h.steps().len(), 3);
        assert_eq!(h.steps()[2].label, "Shadows 0.00 to 0.30");
        assert!(!h.can_redo());
        assert!(h.matches(&q));
    }

    #[test]
    fn rounding_by_a_slider_is_not_a_step() {
        let t = Instant::now();
        let mut p = ProcessParams::default();
        p.stretch.black = 0.016_012_3;
        p.stretch.midtone = 0.010_749;
        let mut h = History::new("Stacked", p.clone(), 0);
        p.stretch.black = 0.016;
        p.stretch.midtone = 0.0107;
        assert!(!h.record(&p, None, 1, t));
        assert_eq!(h.steps().len(), 1);
        assert!(h.matches(&p), "the step follows the rounded values");
        p.stretch.midtone = 0.0108;
        assert!(h.record(&p, None, 2, t));
        assert_eq!(h.steps()[1].label, "Midtones 0.0107 to 0.0108");
    }

    #[test]
    fn quick_repeats_of_one_setting_merge() {
        let t = Instant::now();
        let mut p = ProcessParams::default();
        let mut h = History::new("Stacked", p.clone(), 0);
        for (i, v) in [0.1, 0.2, 0.3].into_iter().enumerate() {
            p.tone.contrast = v;
            h.record(&p, None, i as u64, later(t, i as u64));
        }
        assert_eq!(h.steps().len(), 2);
        assert_eq!(h.steps()[1].label, "Contrast 0.00 to 0.30");

        // A pause, or a different setting, starts a new step.
        p.tone.contrast = 0.4;
        h.record(&p, None, 10, later(t, 10));
        p.tone.shadows = 0.1;
        h.record(&p, None, 11, later(t, 11));
        assert_eq!(h.steps().len(), 4);

        // Nudging back to where it started removes the step again.
        p.tone.shadows = 0.0;
        h.record(&p, None, 12, later(t, 12));
        assert_eq!(h.steps().len(), 3);
        assert_eq!(h.current(), 2);
    }

    #[test]
    fn changes_are_described() {
        let a = ProcessParams::default();
        let label = |f: &dyn Fn(&mut ProcessParams)| {
            let mut b = a.clone();
            f(&mut b);
            describe(&a, &b).label
        };
        assert_eq!(label(&|p| p.geometry.crop[0] = 0.125), "Crop left 0.0% to 12.5%");
        assert_eq!(label(&|p| p.geometry.quarter_turns = 3), "Rotate left");
        assert_eq!(label(&|p| p.gradient.enabled = true), "Remove gradient on");
        assert_eq!(label(&|p| p.curves.r.insert(1, [0.3, 0.5])), "Red curve");
        assert_eq!(label(&|p| p.levels.master.gamma = 1.2), "RGB levels gamma 1.00 to 1.20");
        assert_eq!(label(&|p| (p.stretch.black, p.stretch.midtone) = (0.1, 0.1)), "Black point, midtones");
        assert_eq!(label(&|p| p.color.wb = [2.0, 1.5, 1.2]), "Colour balance: 3 settings");
        assert_eq!(label(&|p| (p.tone.contrast, p.detail.sharpen) = (0.1, 0.1)), "Contrast, sharpen");
        assert_eq!(
            label(&|p| (p.tone.contrast, p.detail.sharpen, p.stars.reduce) = (0.1, 0.1, 0.1)),
            "3 settings changed"
        );

        let mut edited = a.clone();
        edited.tone.contrast = 0.2;
        edited.tone.shadows = 0.2;
        assert_eq!(describe(&edited, &a).label, "Reset tone");
    }

    #[test]
    fn old_steps_are_dropped_and_history_survives_json() {
        let t = Instant::now();
        let mut p = ProcessParams::default();
        let mut h = History::new("Stacked", p.clone(), 0);
        for i in 0..MAX_STEPS + 20 {
            p.tone.exposure = (i + 1) as f32 * 0.01;
            h.record(&p, Some("Edit"), i as u64, t);
        }
        assert_eq!(h.steps().len(), MAX_STEPS);
        assert_eq!(h.current(), MAX_STEPS - 1);
        assert!(h.matches(&p));

        h.jump(5);
        let back: History = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        assert_eq!(back, h);
        assert_eq!(back.current(), 5);
        assert!(History::default().is_empty());
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(ago(100, 90), "just now");
        assert_eq!(ago(1000, 100), "15 min ago");
        assert_eq!(ago(10_000, 100), "2 h ago");
        assert_eq!(ago(200_000, 100), "2 d ago");
        assert_eq!(ago(5, 100), "just now");
    }
}
