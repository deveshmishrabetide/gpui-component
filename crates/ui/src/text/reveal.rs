//! Word-level fade-in for streamed text.
//!
//! A `TextView` in streaming-reveal mode watches the rendered text of its
//! tail paragraph. Whenever that text grows, the appended suffix starts
//! transparent and eases to full opacity, so streamed words resolve into
//! the page instead of stamping onto it. Only run colors are touched —
//! layout, selection, and copy see the text as if it were fully opaque.
//!
//! Tracking lives here, at the view level, because parsed nodes are
//! rebuilt on every incremental parse: any per-node memory would reset
//! each chunk and re-fade the whole paragraph. The tail is identified by
//! matching text — the inline whose rendered text *extends* the last
//! known tail is the tail; when a pass ends with no extension, the last
//! inline rendered adopts the role (a new paragraph started).

use std::time::{Duration, Instant};

use gpui::{EntityId, TextRun};

use crate::animation::ease_out_cubic;

pub(crate) const REVEAL_FADE: Duration = Duration::from_millis(220);

pub(crate) struct RevealState {
    /// The owning `TextViewState`, notified while marks are still fading
    /// so the animation finishes even after the last chunk arrived.
    entity_id: EntityId,
    /// Rendered text of the tail inline as of the last extension.
    tail: Option<String>,
    /// The last inline text observed this pass — document order makes the
    /// final writer the candidate tail for adoption.
    last_seen: Option<String>,
    /// Whether any inline extended the tail this pass. When none did, the
    /// tail moved on (new paragraph) and `last_seen` takes over.
    extended_this_pass: bool,
    /// Fade fronts: text at and beyond `offset` arrived at `at`.
    /// Offsets ascend, and so do arrival times.
    marks: Vec<(usize, Instant)>,
}

impl RevealState {
    pub(crate) fn new(entity_id: EntityId) -> Self {
        Self {
            entity_id,
            tail: None,
            last_seen: None,
            extended_this_pass: false,
            marks: Vec::new(),
        }
    }

    pub(crate) fn entity_id(&self) -> EntityId {
        self.entity_id
    }

    /// Called once per view render, before any inline lays out.
    pub(crate) fn begin_pass(&mut self) {
        let now = Instant::now();
        self.marks
            .retain(|(_, at)| now.duration_since(*at) < REVEAL_FADE);
        if !self.extended_this_pass {
            if let Some(last) = self.last_seen.take() {
                if self.tail.as_ref() != Some(&last) {
                    // The tail moved to a new inline; its first chunk shows
                    // plain and everything appended after fades.
                    self.tail = Some(last);
                    self.marks.clear();
                }
            }
        }
        self.last_seen = None;
        self.extended_this_pass = false;
    }

    /// An inline reporting its rendered text. Returns the fade fronts to
    /// apply to that inline's runs — empty for everyone but the tail.
    pub(crate) fn observe(&mut self, text: &str) -> Vec<(usize, f32)> {
        self.last_seen = Some(text.to_string());
        match &self.tail {
            Some(tail) if text == tail.as_str() => self.alphas(),
            Some(tail) if text.starts_with(tail.as_str()) => {
                let boundary = tail.len();
                self.marks.push((boundary, Instant::now()));
                self.tail = Some(text.to_string());
                self.extended_this_pass = true;
                self.alphas()
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn animating(&self) -> bool {
        !self.marks.is_empty()
    }

    /// Active fade fronts as (offset, opacity): text between one front and
    /// the next renders at that front's opacity.
    fn alphas(&self) -> Vec<(usize, f32)> {
        let now = Instant::now();
        self.marks
            .iter()
            .map(|(offset, at)| {
                let age = now.duration_since(*at).as_secs_f32() / REVEAL_FADE.as_secs_f32();
                (*offset, ease_out_cubic(age.clamp(0., 1.)))
            })
            .collect()
    }
}

/// Re-cuts `runs` at the fade fronts and scales every color (text,
/// background, underline, strikethrough) in the still-fading regions.
pub(crate) fn apply_reveal(runs: Vec<TextRun>, alphas: &[(usize, f32)]) -> Vec<TextRun> {
    if alphas.is_empty() {
        return runs;
    }
    let mut segments: Vec<(usize, f32)> = Vec::with_capacity(alphas.len() + 1);
    segments.push((0, 1.));
    segments.extend_from_slice(alphas);

    let mut out = Vec::with_capacity(runs.len() + segments.len());
    let mut segment_ix = 0;
    let mut pos = 0;
    for run in runs {
        let mut remaining = run.len;
        while remaining > 0 {
            while segment_ix + 1 < segments.len() && segments[segment_ix + 1].0 <= pos {
                segment_ix += 1;
            }
            let segment_end = segments
                .get(segment_ix + 1)
                .map(|(offset, _)| *offset)
                .unwrap_or(usize::MAX);
            let take = remaining.min(segment_end - pos);
            let alpha = segments[segment_ix].1.clamp(0., 1.);
            let mut piece = run.clone();
            piece.len = take;
            if alpha < 1. {
                piece.color.a *= alpha;
                if let Some(background) = &mut piece.background_color {
                    background.a *= alpha;
                }
                if let Some(underline) = &mut piece.underline {
                    if let Some(color) = &mut underline.color {
                        color.a *= alpha;
                    }
                }
                if let Some(strikethrough) = &mut piece.strikethrough {
                    if let Some(color) = &mut strikethrough.color {
                        color.a *= alpha;
                    }
                }
            }
            out.push(piece);
            pos += take;
            remaining -= take;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Hsla, TextRun};

    fn run(len: usize, alpha: f32) -> TextRun {
        TextRun {
            len,
            color: Hsla {
                h: 0.,
                s: 0.,
                l: 1.,
                a: alpha,
            },
            ..Default::default()
        }
    }

    #[test]
    fn reveal_recuts_runs_and_scales_the_fading_suffix() {
        let out = apply_reveal(vec![run(10, 1.)], &[(6, 0.5)]);
        let lens: Vec<usize> = out.iter().map(|r| r.len).collect();
        assert_eq!(lens, [6, 4]);
        assert_eq!(out[0].color.a, 1.0);
        assert_eq!(out[1].color.a, 0.5);
    }

    #[test]
    fn stacked_fronts_apply_their_own_opacity() {
        // Two chunks still fading: [0,4) settled, [4,8) at 0.8, [8,12) at 0.2.
        let out = apply_reveal(vec![run(12, 1.)], &[(4, 0.8), (8, 0.2)]);
        let lens: Vec<usize> = out.iter().map(|r| r.len).collect();
        assert_eq!(lens, [4, 4, 4]);
        assert_eq!(
            out.iter().map(|r| r.color.a).collect::<Vec<_>>(),
            [1.0, 0.8, 0.2]
        );
    }

    #[test]
    fn fronts_falling_mid_run_split_only_what_they_touch() {
        let out = apply_reveal(vec![run(4, 1.), run(8, 0.9)], &[(6, 0.5)]);
        let lens: Vec<usize> = out.iter().map(|r| r.len).collect();
        assert_eq!(lens, [4, 2, 6]);
        assert_eq!(out[1].color.a, 0.9);
        assert!((out[2].color.a - 0.45).abs() < 1e-6);
    }
}
