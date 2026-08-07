//! Repro for transcript paragraphs overlapping their own lines.
//!
//! Any paragraph holding an inline image (link favicons in the app) lays
//! out through `InlineFlow`, which slices the text into per-line fragments
//! sized by `shape_line` (real shaping) and then prepaints each fragment as
//! an `Inline`/`StyledText` at exactly that width. `StyledText` re-wraps
//! its text with `LineWrapper`'s per-char width cache (single base font, no
//! kerning, no per-run styles), so whenever the char-sum estimate for a
//! fragment exceeds its shaped width at a word boundary, the fragment's
//! last word wraps INSIDE the fragment and paints one line_height down, on
//! top of the next visual line.
//!
//! The sweep below renders the same icon+link+chips paragraph at many wrap
//! widths; before the fix several rows show the spill (missing words at
//! line ends, doubled text on the next line), after it none do.

use gpui::*;
use gpui_component::{
    ActiveTheme,
    text::{MarkdownExtensions, TextView, TextViewState},
    v_flex,
};
use gpui_component_assets::Assets;

const MESSAGE: &str = "PR is up: https://github.com/betidestu/neostackv6/pull/59 — `main` → `production`, containing exactly the one scroll-fix commit (`b50fc1b`); production was otherwise already level with main. The body covers the four fixes and the test evidence. I haven't merged it, that's your call.";

/// Byte size of one simulated stream chunk (the app drips 8+ bytes per
/// 33ms tick).
const CHUNK: usize = 9;

pub struct Example {
    /// One owned streaming state per sweep width, like the app's owned
    /// streaming message state (`ChatListView::streaming`).
    states: Vec<Entity<TextViewState>>,
    offset: usize,
}

impl Example {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let states = (0..60)
            .map(|_| {
                cx.new(|cx| {
                    let mut state = TextViewState::markdown("", cx);
                    state.enable_streaming_reveal();
                    state
                })
            })
            .collect();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(33))
                    .await;
                let done = this
                    .update(cx, |example, cx| {
                        let mut take = CHUNK.min(MESSAGE.len() - example.offset);
                        while !MESSAGE.is_char_boundary(example.offset + take) {
                            take += 1;
                        }
                        if take == 0 {
                            // Restart the stream so the run keeps probing.
                            example.offset = 0;
                            for state in &example.states {
                                state.update(cx, |state, cx| state.set_text("", cx));
                            }
                            cx.notify();
                            return false;
                        }
                        let chunk = &MESSAGE[example.offset..example.offset + take];
                        example.offset += take;
                        for state in &example.states {
                            state.update(cx, |state, cx| state.push_str(chunk, cx));
                        }
                        cx.notify();
                        false
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        })
        .detach();
        Self { states, offset: 0 }
    }

    fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }
}

fn link_icons() -> MarkdownExtensions {
    // Same shape as the app's transcript extensions: a favicon-style icon
    // inline before every link, which is what routes the paragraph through
    // InlineFlow.
    MarkdownExtensions::default().link_icon(|_href| {
        Some("https://www.google.com/s2/favicons?domain=github.com&sz=64".into())
    })
}

impl Render for Example {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .overflow_hidden()
            .p_4()
            .gap_6()
            .bg(cx.theme().background)
            // Ancestor-styled type like the desktop transcript (Inter
            // prose). 18px > the 16px window default, so the deferred
            // measure's wrong style under-sizes every fragment and the
            // spill reproduces deterministically instead of only on
            // unlucky width/glyph combinations.
            .font_family("Inter")
            .text_size(px(18.))
            .line_height(px(27.))
            .children(self.states.iter().enumerate().map(|(step, state)| {
                let width = 320. + 8. * step as f32;
                div().w(px(width)).border_1().border_color(cx.theme().border).child(
                    TextView::new(state)
                        .markdown_extensions(link_icons())
                        .selectable(true),
                )
            }))
    }
}

fn main() {
    let app = gpui_platform::application().with_assets(Assets);

    app.run(move |cx| {
        // The desktop app bundles Inter; it is not installed system-wide,
        // so load it the same way or the sweep silently runs on the
        // fallback font (whose metrics do not reproduce the bug).
        let fonts = std::path::Path::new(
            "/Users/devesh/Desktop/NeoProjs/neostackv6/apps/desktop/assets/fonts",
        );
        let loaded: Vec<std::borrow::Cow<'static, [u8]>> = std::fs::read_dir(fonts)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| {
                        entry.path().extension().is_some_and(|ext| ext == "otf")
                    })
                    .filter_map(|entry| std::fs::read(entry.path()).ok())
                    .map(std::borrow::Cow::Owned)
                    .collect()
            })
            .unwrap_or_default();
        cx.text_system()
            .add_fonts(loaded)
            .expect("bundled fonts load");
        eprintln!(
            "font-probe: Inter loaded = {}",
            cx.text_system().all_font_names().iter().any(|name| name == "Inter")
        );

        gpui_component_story::init(cx);
        cx.activate(true);

        gpui_component_story::create_new_window("InlineFlow Repro", Example::view, cx);
    });
}
