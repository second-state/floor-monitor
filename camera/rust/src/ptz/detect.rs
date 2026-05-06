//! Capability detection from `v4l2-ctl --list-ctrls` output.
//!
//! Pure parsing lives in [`parse_list_ctrls`] — feed it the captured stdout
//! string and it returns the set of control names plus per-control min/max
//! /step ranges. [`detect`] runs the command via a [`super::v4l2ctl::V4l2CtlRunner`]
//! and folds the parse into a [`super::PtzCapabilities`].

use super::v4l2ctl::V4l2CtlRunner;
use super::PtzCapabilities;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlRange {
    pub min: i32,
    pub max: i32,
    pub step: i32,
}

impl ControlRange {
    /// Snap a value to the nearest legal point on the V4L2 control's
    /// lattice. The V4L2 spec requires writes to integer absolute
    /// controls to be of the form `min + N*step` for non-negative N
    /// — values off-lattice are rejected by the driver. Without this,
    /// a hardware whose `step` differs from the configured `pan_step`
    /// /`tilt_step` would reject every `--set-ctrl` invocation.
    ///
    /// Rounding is round-half-up via integer arithmetic
    /// (`(offset + step/2) / step`): exactly-halfway values round
    /// toward positive infinity. Edge case: a step of 0 or 1 means no
    /// granularity to enforce, so we just clamp.
    pub fn snap(&self, value: i32) -> i32 {
        let clamped = value.clamp(self.min, self.max);
        if self.step <= 1 {
            return clamped;
        }
        // offset is non-negative because clamped >= self.min.
        let offset = clamped - self.min;
        let q = (offset + self.step / 2) / self.step;
        (q * self.step + self.min).clamp(self.min, self.max)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedControls {
    pub names: HashSet<String>,
    pub ranges: HashMap<String, ControlRange>,
    /// Per-control current `value=N` from the v4l2-ctl output. Used to
    /// seed absolute-mode position tracking — without this we'd assume
    /// the camera is at 0 even when another tool left it elsewhere.
    pub values: HashMap<String, i32>,
}

impl ParsedControls {
    pub fn has(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    pub fn range(&self, name: &str) -> Option<ControlRange> {
        self.ranges.get(name).copied()
    }

    pub fn value(&self, name: &str) -> Option<i32> {
        self.values.get(name).copied()
    }
}

/// Parse `v4l2-ctl --list-ctrls` stdout. Tolerant: ignores blank lines,
/// section headers, and any line whose first token doesn't look like a
/// control name. Extracts min/max/step where present.
pub fn parse_list_ctrls(output: &str) -> ParsedControls {
    let mut names = HashSet::new();
    let mut ranges = HashMap::new();
    let mut values = HashMap::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // A control line has shape:
        //   pan_absolute 0x009a0908 (int) : min=-36000 max=36000 step=3600 default=0 value=0
        // We require a colon to distinguish it from `User Controls` headers.
        if !trimmed.contains(':') {
            continue;
        }

        let Some(name) = trimmed.split_whitespace().next() else {
            continue;
        };

        // Drop obviously-non-control tokens (heuristic: control names are
        // lowercase identifiers with underscores; section headers like
        // "User Controls" start with an uppercase letter).
        if !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
        {
            continue;
        }

        names.insert(name.to_string());

        let mut min = None;
        let mut max = None;
        let mut step = None;
        let mut value = None;
        for tok in trimmed.split_whitespace() {
            if let Some(v) = tok.strip_prefix("min=") {
                min = v.parse::<i32>().ok();
            } else if let Some(v) = tok.strip_prefix("max=") {
                max = v.parse::<i32>().ok();
            } else if let Some(v) = tok.strip_prefix("step=") {
                step = v.parse::<i32>().ok();
            } else if let Some(v) = tok.strip_prefix("value=") {
                value = v.parse::<i32>().ok();
            }
        }
        if let (Some(min), Some(max), Some(step)) = (min, max, step) {
            ranges.insert(name.to_string(), ControlRange { min, max, step });
        }
        if let Some(v) = value {
            values.insert(name.to_string(), v);
        }
    }

    ParsedControls {
        names,
        ranges,
        values,
    }
}

impl PtzCapabilities {
    /// Derive hardware capabilities from a parsed control list. Decision rules:
    ///
    /// - `pan_relative` OR `pan_absolute` present → `pan = true`. (Speed-mode
    ///   controls like `pan_speed` are NOT counted: `V4l2CtlPtz` only knows
    ///   how to drive relative and absolute, so advertising speed-only
    ///   cameras as PTZ-capable would just produce `Unsupported` errors.)
    /// - Same scheme for tilt and zoom.
    /// - `home` is synthesizable when both `pan_absolute` and `tilt_absolute`
    ///   exist (write 0 to both).
    pub fn from_controls(p: &ParsedControls) -> Self {
        let pan = p.has("pan_relative") || p.has("pan_absolute");
        let tilt = p.has("tilt_relative") || p.has("tilt_absolute");
        let zoom = p.has("zoom_relative") || p.has("zoom_absolute");
        let home = p.has("pan_absolute") && p.has("tilt_absolute");
        Self {
            pan,
            tilt,
            zoom,
            home,
        }
    }
}

/// Run `v4l2-ctl -d <device> --list-ctrls` via `runner`, parse the output,
/// and return the inferred [`PtzCapabilities`]. Any runner error (timeout,
/// non-zero exit, missing binary) is treated as "no PTZ" — frame streaming
/// must keep working on hosts without `v4l-utils` installed.
pub async fn detect<R: V4l2CtlRunner + ?Sized>(runner: &R, device: &str) -> PtzCapabilities {
    let args = ["-d", device, "--list-ctrls"];
    match runner.run(&args).await {
        Ok(output) => PtzCapabilities::from_controls(&parse_list_ctrls(&output)),
        Err(_) => PtzCapabilities::default(),
    }
}

/// Resolve the list of capability strings to advertise to the server.
/// If the user-supplied list is non-empty it wins (explicit override);
/// otherwise we use the auto-detected set.
///
/// Note: this only resolves the **wire-level advertised list**. The
/// `Ptz` implementation in use is still picked by detection + the
/// `[ptz] enabled` flag — the override never swaps a `NoopPtz` for a
/// `V4l2CtlPtz` or vice versa.
pub fn resolve_advertised_capabilities(
    user_supplied: &[String],
    detected: PtzCapabilities,
) -> Vec<String> {
    if !user_supplied.is_empty() {
        return user_supplied.to_vec();
    }
    detected.advertised()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_returns_empty() {
        let p = parse_list_ctrls("");
        assert!(p.names.is_empty());
        assert!(p.ranges.is_empty());
    }

    #[test]
    fn parse_skips_section_header() {
        let p = parse_list_ctrls("User Controls\n");
        assert!(p.names.is_empty());
    }

    #[test]
    fn snap_aligned_value_is_unchanged() {
        let r = ControlRange {
            min: -36000,
            max: 36000,
            step: 3600,
        };
        assert_eq!(r.snap(0), 0);
        assert_eq!(r.snap(3600), 3600);
        assert_eq!(r.snap(-7200), -7200);
        assert_eq!(r.snap(36000), 36000);
        assert_eq!(r.snap(-36000), -36000);
    }

    #[test]
    fn snap_off_lattice_value_rounds_to_nearest() {
        let r = ControlRange {
            min: 0,
            max: 100,
            step: 10,
        };
        assert_eq!(r.snap(13), 10); // 13 closer to 10
        assert_eq!(r.snap(15), 20); // round-half-up
        assert_eq!(r.snap(17), 20);
        assert_eq!(r.snap(99), 100);
    }

    #[test]
    fn snap_clamps_to_range() {
        let r = ControlRange {
            min: -100,
            max: 100,
            step: 10,
        };
        assert_eq!(r.snap(200), 100);
        assert_eq!(r.snap(-200), -100);
    }

    #[test]
    fn snap_with_step_one_is_pure_clamp() {
        let r = ControlRange {
            min: 0,
            max: 100,
            step: 1,
        };
        assert_eq!(r.snap(13), 13);
        assert_eq!(r.snap(150), 100);
    }

    #[test]
    fn snap_with_offset_min() {
        // Range starts at non-zero min — values are min + N*step.
        let r = ControlRange {
            min: -180,
            max: 180,
            step: 12,
        };
        // -180 + 0*12 = -180, -180 + 1*12 = -168, ..., 0 = -180 + 15*12.
        assert_eq!(r.snap(-180), -180);
        assert_eq!(r.snap(-168), -168);
        assert_eq!(r.snap(-160), -156); // closer to -168+12=-156
        assert_eq!(r.snap(0), 0);
        assert_eq!(r.snap(180), 180);
    }

    #[test]
    fn from_controls_no_ptz() {
        let p = ParsedControls::default();
        assert_eq!(
            PtzCapabilities::from_controls(&p),
            PtzCapabilities::default()
        );
    }
}
