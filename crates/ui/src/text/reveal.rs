//! Word-level fade-in for streamed text.
//!
//! A `TextView` in streaming-reveal mode fades freshly appended text from
//! transparent to full opacity, so streamed words resolve into the page
//! instead of stamping onto it. Only run colors are touched — layout,
//! selection, and copy see the text as if it were fully opaque.
//!
//! # Design constraints (learned the hard way)
//!
//! Tracking lives at the view level because parsed nodes are rebuilt on
//! every incremental parse: per-node memory resets each chunk. An earlier
//! version *elected* a tail inline ("the last one rendered this pass") and
//! faded whatever extended it. That design depended on render/layout
//! cadence invariants GPUI does not guarantee — layouts run without
//! renders, the same pass can lay an inline out twice, and paragraphs
//! rendered through `InlineFlow` (any text with an inline image, e.g. link
//! favicons) never report in at all. A mis-election faded a *settled*
//! paragraph out from byte 0 — text visibly vanishing mid-stream.
//!
//! This version has no election and no pass tracking. Every observing
//! inline identifies itself by its stable leading bytes and fades only
//! text that provably grew on that same inline:
//!
//! - A settled paragraph's text never changes → it can never fade.
//! - The growing paragraph matches its own history → its appended suffix
//!   fades.
//! - Anything unrecognized (block splits, re-parses that rewrite text)
//!   resets to fully opaque — the failure mode is a skipped fade, never
//!   hidden text.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use gpui::{EntityId, TextRun};

use crate::animation::ease_out_cubic;

pub(crate) const REVEAL_FADE: Duration = Duration::from_millis(220);

/// How many leading bytes identify an inline. A paragraph's first bytes
/// are written once and never change while it grows, so they are a stable
/// identity across incremental re-parses; 32 bytes make cross-paragraph
/// collisions unlikely (and a collision only means a shared fade record —
/// bounded shimmer, never disappearance).
const IDENTITY_PREFIX: usize = 32;

struct Entry {
    /// Full text as of the last observation.
    text: String,
    /// Fade fronts: text at and beyond `offset` arrived at `at`.
    /// Offsets strictly ascend, and so do arrival times.
    fronts: Vec<(usize, Instant)>,
}

pub(crate) struct RevealState {
    /// The owning `TextViewState`, notified while fronts are still fading
    /// so the animation finishes after the last chunk arrived.
    entity_id: EntityId,
    /// Per-inline fade records, keyed by identity prefix hash. Lives as
    /// long as the streaming view does (one message), so it stays small.
    entries: HashMap<u64, Entry>,
    debug: bool,
}

fn identity_key(text: &str) -> u64 {
    let mut end = text.len().min(IDENTITY_PREFIX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text[..end].hash(&mut hasher);
    hasher.finish()
}

impl RevealState {
    pub(crate) fn new(entity_id: EntityId) -> Self {
        Self {
            entity_id,
            entries: HashMap::new(),
            debug: std::env::var("GPUI_TEXT_REVEAL_DEBUG").is_ok(),
        }
    }

    pub(crate) fn entity_id(&self) -> EntityId {
        self.entity_id
    }

    /// An inline reporting its rendered text; returns the fade fronts to
    /// apply to its runs. Idempotent within a frame and safe to call at
    /// any cadence — repeated observations of unchanged text return the
    /// same (aging) fronts.
    pub(crate) fn observe(&mut self, text: &str) -> Vec<(usize, f32)> {
        if text.trim().is_empty() {
            return Vec::new();
        }
        let now = Instant::now();
        let key = identity_key(text);
        if !self.entries.contains_key(&key) {
            // A paragraph shorter than the identity prefix re-keys as it
            // grows. Migrate the entry this text extends so its history
            // (and mid-flight fades) follow it instead of orphaning.
            let migrated = self.entries.iter().find_map(|(old_key, entry)| {
                (!entry.text.is_empty()
                    && entry.text.len() < IDENTITY_PREFIX
                    && text.starts_with(entry.text.as_str()))
                .then_some(*old_key)
            });
            if let Some(old_key) = migrated {
                let entry = self.entries.remove(&old_key).expect("key just found");
                self.entries.insert(key, entry);
            }
        }
        let entry = self.entries.entry(key).or_insert_with(|| Entry {
            text: String::new(),
            fronts: Vec::new(),
        });
        if text != entry.text {
            if !entry.text.is_empty() && text.starts_with(entry.text.as_str()) {
                // This same inline grew: exactly the appended bytes fade.
                if self.debug {
                    eprintln!(
                        "reveal: extend at {} (+{} bytes) for {:?}…",
                        entry.text.len(),
                        text.len() - entry.text.len(),
                        &text[..text.len().min(24)]
                    );
                }
                entry.fronts.push((entry.text.len(), now));
            } else {
                // First sighting, or a rewrite (block split / identity
                // collision): render plainly and fade only what grows
                // from here. Never fade text we cannot prove is new.
                if self.debug {
                    eprintln!(
                        "reveal: rebase ({} -> {} bytes) for {:?}…",
                        entry.text.len(),
                        text.len(),
                        &text[..text.len().min(24)]
                    );
                }
                entry.fronts.clear();
            }
            entry.text.clear();
            entry.text.push_str(text);
        }
        entry
            .fronts
            .retain(|(_, at)| now.duration_since(*at) < REVEAL_FADE);
        entry
            .fronts
            .iter()
            .map(|(offset, at)| {
                let age = now.duration_since(*at).as_secs_f32() / REVEAL_FADE.as_secs_f32();
                (*offset, ease_out_cubic(age.clamp(0., 1.)))
            })
            .collect()
    }

    /// Whether any fade is still mid-flight (drives the keep-rendering
    /// notify from paint).
    pub(crate) fn animating(&self) -> bool {
        let now = Instant::now();
        self.entries.values().any(|entry| {
            entry
                .fronts
                .iter()
                .any(|(_, at)| now.duration_since(*at) < REVEAL_FADE)
        })
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

    fn reveal() -> RevealState {
        RevealState::new(EntityId::from(u64::MAX))
    }

    /// The field bug this design exists to prevent: settled paragraphs
    /// must never fade, no matter what other inlines do around them.
    #[test]
    fn settled_text_never_fades() {
        let mut reveal = reveal();
        assert!(reveal.observe("First paragraph, long settled.").is_empty());
        // The tail paragraph grows over several observations…
        assert!(reveal.observe("Tail").is_empty());
        assert!(!reveal.observe("Tail grows here").is_empty());
        assert!(!reveal.observe("Tail grows here and here").is_empty());
        // …and no cadence of re-observing the settled paragraph — layout
        // repeats, idle frames, whatever — ever fades it.
        for _ in 0..10 {
            assert!(reveal.observe("First paragraph, long settled.").is_empty());
        }
    }

    #[test]
    fn only_the_appended_suffix_fades_and_repeats_are_idempotent() {
        let mut reveal = reveal();
        assert!(reveal.observe("Hello").is_empty());
        let first = reveal.observe("Hello world");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, "Hello".len());
        // Same text observed again (double layout in one frame): same
        // front, no new ones.
        let again = reveal.observe("Hello world");
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].0, "Hello".len());
        // Another extension stacks a second, later front.
        let more = reveal.observe("Hello world again");
        assert_eq!(more.len(), 2);
        assert_eq!(more[1].0, "Hello world".len());
    }

    #[test]
    fn rewrites_and_splits_reset_to_opaque_rather_than_fade() {
        let mut reveal = reveal();
        assert!(reveal.observe("A paragraph that will split in two").is_empty());
        assert!(!reveal.observe("A paragraph that will split in two, growing").is_empty());
        // A block split rewrites this inline's text (same leading bytes,
        // different continuation): everything renders opaque again.
        assert!(reveal.observe("A paragraph that will split").is_empty());
    }

    #[test]
    fn empty_and_whitespace_inlines_are_ignored() {
        let mut reveal = reveal();
        assert!(reveal.observe("").is_empty());
        assert!(reveal.observe("   ").is_empty());
        // And they never become anyone's history: a real paragraph after
        // them starts opaque, no fade-from-zero.
        assert!(reveal.observe("Real text").is_empty());
        let grown = reveal.observe("Real text grows");
        assert_eq!(grown.len(), 1);
        assert_eq!(grown[0].0, "Real text".len());
    }

    #[test]
    fn short_first_chunks_rebase_without_fading_older_text() {
        let mut reveal = reveal();
        // Chunks shorter than the identity prefix change their own key as
        // they grow; each rebase must only skip the fade, never carry a
        // front into unrelated text.
        assert!(reveal.observe("Hi").is_empty());
        let observed = reveal.observe("Hi there, this is a much longer paragraph now");
        for (offset, _) in &observed {
            assert!(*offset >= "Hi".len());
        }
    }

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
