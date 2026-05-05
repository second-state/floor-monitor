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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedControls {
    pub names: HashSet<String>,
    pub ranges: HashMap<String, ControlRange>,
}

impl ParsedControls {
    pub fn has(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    pub fn range(&self, name: &str) -> Option<ControlRange> {
        self.ranges.get(name).copied()
    }
}

/// Parse `v4l2-ctl --list-ctrls` stdout. Tolerant: ignores blank lines,
/// section headers, and any line whose first token doesn't look like a
/// control name. Extracts min/max/step where present.
pub fn parse_list_ctrls(output: &str) -> ParsedControls {
    let mut names = HashSet::new();
    let mut ranges = HashMap::new();

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
        for tok in trimmed.split_whitespace() {
            if let Some(v) = tok.strip_prefix("min=") {
                min = v.parse::<i32>().ok();
            } else if let Some(v) = tok.strip_prefix("max=") {
                max = v.parse::<i32>().ok();
            } else if let Some(v) = tok.strip_prefix("step=") {
                step = v.parse::<i32>().ok();
            }
        }
        if let (Some(min), Some(max), Some(step)) = (min, max, step) {
            ranges.insert(name.to_string(), ControlRange { min, max, step });
        }
    }

    ParsedControls { names, ranges }
}

impl PtzCapabilities {
    /// Derive hardware capabilities from a parsed control list. Decision rules:
    ///
    /// - `pan_relative`, `pan_absolute`, or `pan_speed` present → `pan = true`.
    /// - Same scheme for tilt and zoom.
    /// - `home` is synthesizable when both `pan_absolute` and `tilt_absolute`
    ///   exist (write 0 to both).
    pub fn from_controls(p: &ParsedControls) -> Self {
        let pan = p.has("pan_relative") || p.has("pan_absolute") || p.has("pan_speed");
        let tilt = p.has("tilt_relative") || p.has("tilt_absolute") || p.has("tilt_speed");
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
    fn from_controls_no_ptz() {
        let p = ParsedControls::default();
        assert_eq!(
            PtzCapabilities::from_controls(&p),
            PtzCapabilities::default()
        );
    }
}
