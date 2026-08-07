//! Instrumented repro for "the input box moves a little when text
//! changes": the desktop composer's exact construction (auto_grow 1..10,
//! appearance off, px_2/py_1, 14px text, a min-h 30 wrapper), driven by a
//! timer that types, wraps, and clears — while a canvas logs every change
//! to the input's measured bounds. Any line that isn't an intended row
//! transition IS the jiggle.

use std::cell::Cell;
use std::rc::Rc;

use gpui::*;
use gpui_component::{
    ActiveTheme,
    input::{Input, InputState},
    v_flex,
};
use gpui_component_assets::Assets;

const SCRIPT: &[&str] = &[
    "h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d", " ",
    "this is a longer run of text meant to reach the wrap boundary of the box ",
    "and a second sentence that certainly wraps onto further lines now",
    "\n", "newline row", "<CLEAR>",
];

pub struct Example {
    input: Entity<InputState>,
    step: usize,
}

impl Example {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(1, 10)
                .submit_on_enter(true)
                .placeholder("Message the agent — Shift+Enter for a new line")
        });
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(400))
                    .await;
                let done = this
                    .update_in(cx, |example, window, cx| {
                        let step = SCRIPT[example.step % SCRIPT.len()];
                        example.step += 1;
                        example.input.update(cx, |input, cx| {
                            if step == "<CLEAR>" {
                                eprintln!("TYPE: <CLEAR>");
                                input.set_value("", window, cx);
                            } else {
                                eprintln!("TYPE: {step:?}");
                                input.insert(step, window, cx);
                            }
                        });
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
        Self { input, step: 0 }
    }

    fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }
}

/// Paints nothing; logs when the watched bounds move or resize.
fn bounds_probe(name: &'static str) -> impl IntoElement {
    let last: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
    canvas(
        |_, _, _| (),
        move |bounds, _, _, _| {
            let previous = last.replace(Some(bounds));
            if previous != Some(bounds) {
                eprintln!(
                    "BOUNDS[{name}]: origin=({:?},{:?}) size=({:?}x{:?})",
                    bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height
                );
            }
        },
    )
    .absolute()
    .inset_0()
}

impl Render for Example {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_8()
            .justify_end()
            .bg(cx.theme().background)
            .font_family("Inter")
            .text_size(px(14.))
            .child(
                // The desktop composer box, faithfully.
                v_flex()
                    .w(px(720.))
                    .px(px(10.))
                    .pt(px(10.))
                    .pb(px(10.))
                    .gap_2()
                    .rounded(px(16.))
                    .border_1()
                    .border_color(cx.theme().border)
                    .relative()
                    .child(bounds_probe("box"))
                    .child(
                        div().min_w_0().w_full().min_h(px(30.)).relative().child(
                            Input::new(&self.input)
                                .appearance(false)
                                .bordered(false)
                                .focus_bordered(false)
                                .px_2()
                                .py_1()
                                .text_size(px(14.)),
                        )
                        .child(bounds_probe("input-wrap")),
                    )
                    .child(div().h(px(32.)).child("controls row")),
            )
    }
}

fn main() {
    let app = gpui_platform::application().with_assets(Assets);

    app.run(move |cx| {
        let fonts = std::path::Path::new(
            "/Users/devesh/Desktop/NeoProjs/neostackv6/apps/desktop/assets/fonts",
        );
        let loaded: Vec<std::borrow::Cow<'static, [u8]>> = std::fs::read_dir(fonts)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "otf"))
                    .filter_map(|entry| std::fs::read(entry.path()).ok())
                    .map(std::borrow::Cow::Owned)
                    .collect()
            })
            .unwrap_or_default();
        cx.text_system().add_fonts(loaded).expect("fonts load");

        gpui_component_story::init(cx);
        cx.activate(true);

        gpui_component_story::create_new_window("Composer Jiggle Repro", Example::view, cx);
    });
}
