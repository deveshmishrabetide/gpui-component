//! Inline element slots: app-defined widgets that flow inside prose.
//!
//! A matcher recognizes spans in already-parsed text (an `@file` mention,
//! a citation token, a path) and replaces each with an [`InlineElementSpec`]
//! — an arbitrary element builder plus the plain text the span contributes
//! to selection and copy. The flow layout treats the element like an
//! unbreakable word: measured up front, wrapped with the line, vertically
//! centered in its line box.

use std::{fmt, ops::Range, sync::Arc};

use gpui::{AnyElement, App, SharedString, Window};

/// Builds the inline element. Called fresh for measurement and for every
/// painted frame, so it must be cheap and idempotent.
pub type InlineElementBuilder = Arc<dyn Fn(&mut Window, &mut App) -> AnyElement + Send + Sync>;

/// One inline widget: what it renders, and what it *is* as text.
#[derive(Clone)]
pub struct InlineElementSpec {
    /// The plain-text stand-in for selection, copy, and `text()` — an
    /// element with no text identity would silently vanish from every
    /// copy. Never empty by contract.
    pub copy_text: SharedString,
    pub build: InlineElementBuilder,
}

impl InlineElementSpec {
    pub fn new(
        copy_text: impl Into<SharedString>,
        build: impl Fn(&mut Window, &mut App) -> AnyElement + Send + Sync + 'static,
    ) -> Self {
        Self {
            copy_text: copy_text.into(),
            build: Arc::new(build),
        }
    }
}

impl fmt::Debug for InlineElementSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InlineElementSpec")
            .field("copy_text", &self.copy_text)
            .finish_non_exhaustive()
    }
}

impl PartialEq for InlineElementSpec {
    fn eq(&self, other: &Self) -> bool {
        // Builders are opaque; the text identity is the comparable part.
        // Callers whose rendering changes for the same copy_text bump the
        // registry revision instead.
        self.copy_text == other.copy_text
    }
}

/// One recognized span within a text run.
#[derive(Debug)]
pub struct InlineElementMatch {
    /// Byte range in the text handed to the matcher.
    pub range: Range<usize>,
    pub spec: InlineElementSpec,
}

type MatcherFn = dyn Fn(&str) -> Vec<InlineElementMatch> + Send + Sync;

/// Registry of inline-element matchers for one `TextView`, compared by
/// revision like [`super::markdown_ext::MarkdownExtensions`]: the same
/// builder chain yields the same revision, so re-rendering with an
/// unchanged registry never re-parses.
#[derive(Clone, Default)]
pub struct InlineMatchers {
    matchers: Vec<Arc<MatcherFn>>,
    revision: u64,
}

impl InlineMatchers {
    /// Register a matcher over plain text runs. Ranges must not overlap
    /// within one matcher's output; across matchers, earlier registrations
    /// win.
    pub fn matcher<F>(mut self, matcher: F) -> Self
    where
        F: Fn(&str) -> Vec<InlineElementMatch> + Send + Sync + 'static,
    {
        self.matchers.push(Arc::new(matcher));
        self.revision += 1;
        self
    }

    /// Overrides the revision. Use when a matcher's *behavior* depends on
    /// data the builder chain cannot see (a lookup table that loads late):
    /// key the registry by that data's version so changes re-apply.
    pub fn keyed(mut self, revision: u64) -> Self {
        self.revision = revision;
        self
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.matchers.is_empty()
    }

    /// All matches over `text`, sorted, overlaps dropped (first
    /// registration wins), empty ranges and empty copy_text discarded.
    pub(crate) fn matches(&self, text: &str) -> Vec<InlineElementMatch> {
        let mut all: Vec<(usize, InlineElementMatch)> = Vec::new();
        for (priority, matcher) in self.matchers.iter().enumerate() {
            for hit in matcher(text) {
                let valid = hit.range.start < hit.range.end
                    && hit.range.end <= text.len()
                    && text.is_char_boundary(hit.range.start)
                    && text.is_char_boundary(hit.range.end)
                    && !hit.spec.copy_text.is_empty();
                if valid {
                    all.push((priority, hit));
                }
            }
        }
        all.sort_by(|a, b| {
            a.1.range
                .start
                .cmp(&b.1.range.start)
                .then(a.0.cmp(&b.0))
                .then(b.1.range.end.cmp(&a.1.range.end))
        });
        let mut result: Vec<InlineElementMatch> = Vec::new();
        for (_, hit) in all {
            if result
                .last()
                .is_none_or(|last| hit.range.start >= last.range.end)
            {
                result.push(hit);
            }
        }
        result
    }
}

impl fmt::Debug for InlineMatchers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InlineMatchers")
            .field("matchers", &self.matchers.len())
            .field("revision", &self.revision)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::div;
    use gpui::prelude::*;

    fn spec(text: &str) -> InlineElementSpec {
        InlineElementSpec::new(text.to_string(), |_, _| div().into_any_element())
    }

    #[test]
    fn overlapping_matches_prefer_earlier_registration_then_position() {
        let matchers = InlineMatchers::default()
            .matcher(|text| {
                text.match_indices("@one")
                    .map(|(at, hit)| InlineElementMatch {
                        range: at..at + hit.len(),
                        spec: spec("first"),
                    })
                    .collect()
            })
            .matcher(|text| {
                // Overlaps the first matcher's token and adds its own.
                let mut hits: Vec<InlineElementMatch> = text
                    .match_indices("one")
                    .map(|(at, hit)| InlineElementMatch {
                        range: at..at + hit.len(),
                        spec: spec("second"),
                    })
                    .collect();
                hits.extend(text.match_indices("@two").map(|(at, hit)| {
                    InlineElementMatch {
                        range: at..at + hit.len(),
                        spec: spec("third"),
                    }
                }));
                hits
            });

        let matches = matchers.matches("say @one and @two");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].spec.copy_text.as_ref(), "first");
        assert_eq!(matches[1].spec.copy_text.as_ref(), "third");
    }

    #[test]
    fn invalid_ranges_and_empty_copy_text_are_dropped() {
        let matchers = InlineMatchers::default().matcher(|text| {
            vec![
                InlineElementMatch {
                    range: 0..0,
                    spec: spec("empty-range"),
                },
                InlineElementMatch {
                    range: 0..text.len() + 10,
                    spec: spec("out-of-bounds"),
                },
                InlineElementMatch {
                    range: 0..1,
                    spec: spec(""),
                },
                InlineElementMatch {
                    range: 0..2,
                    spec: spec("ok"),
                },
            ]
        });
        let matches = matchers.matches("hello");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].spec.copy_text.as_ref(), "ok");
    }
}
