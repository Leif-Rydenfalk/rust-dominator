use dominator::animation::{easing, AnimatedMapBroadcaster, MutableAnimation, Percentage};
use dominator::traits::AnimatedSignalVec;
use dominator::{class, clone, events, html, Dom};
use futures::stream::StreamExt;
use futures_signals::map_ref;
use futures_signals::signal::{Mutable, SignalExt};
use futures_signals::signal_vec::MutableVec;
use gloo_timers::future::IntervalStream;
use once_cell::sync::Lazy;
use std::sync::Arc;
use wasm_bindgen::prelude::*;

struct Text {
    text: Arc<str>,
}

impl Text {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            text: "Hello World".into(),
        })
    }

    fn render_character(character: &str, index: usize) -> Dom {
        html!("div", {
            .text(character)
            // .children_signal_vec(app.bars.signal_vec_cloned()
            //     // Animates the Bar for 2000ms when inserting/removing
            //     .animated_map(2000.0, |bar, animation| Bar::render(bar, animation)))
        })
    }

    fn render(text: Arc<Self>) -> Dom {
        static CLASS: Lazy<String> = Lazy::new(|| {
            class! {
                .style("display", "flex")
                .style("flex-direction", "row")
            }
        });

        html!("div", {
            .class(&*CLASS)
            // .text("s")
            .children(&mut [
                // .text("s")
                Text::render_character("s", 0)
            ])
            // .children(&mut text.text.chars().enumerate().for_each(|(character, id)| {
            //     Text::render_character(character)
            // }).collect())
        })
    }
}

#[wasm_bindgen(start)]
pub fn main_js() -> Result<(), JsValue> {
    #[cfg(debug_assertions)]
    console_error_panic_hook::set_once();

    let text = Text::new();
    dominator::append_dom(&dominator::body(), Text::render(text));

    Ok(())
}
