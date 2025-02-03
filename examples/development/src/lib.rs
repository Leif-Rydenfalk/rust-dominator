use dominator::animation::{easing, AnimatedMapBroadcaster, MutableAnimation, Percentage};
use dominator::traits::AnimatedSignalVec;
use dominator::{class, clone, events, html, Dom};
use futures::stream::StreamExt;
use futures_signals::map_ref;
use futures_signals::signal::{Mutable, SignalExt};
use futures_signals::signal_vec::MutableVec;
use gloo_timers::future::{IntervalStream, TimeoutFuture};
use once_cell::sync::Lazy;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::js_sys::Date;

mod util;

/// Configuration for the spring animation.
#[derive(Clone, Copy)]
struct AnimationConfig {
    mass: f32,
    stiffness: f32,
    damping: f32,
    initial: f32,
    target: f32,
    delay_factor: f32,
    start_delay: f32,
}

/// A basic spring physics simulation.
#[derive(Debug)]
struct Spring {
    mass: f32,
    stiffness: f32,
    damping: f32,
    value: f32,
    target: f32,
    velocity: f32,
}

impl Spring {
    fn new(initial: f32) -> Self {
        Self {
            mass: 1.0,
            stiffness: 400.0,
            damping: 7.0,
            value: initial,
            target: initial,
            velocity: 0.0,
        }
    }

    fn new_with_config(config: AnimationConfig) -> Self {
        Self {
            mass: config.mass,
            stiffness: config.stiffness,
            damping: config.damping,
            value: config.initial,
            target: config.initial,
            velocity: 0.0,
        }
    }

    fn mass(mut self, mass: f32) -> Self {
        self.mass = mass;
        self
    }

    fn stiffness(mut self, stiffness: f32) -> Self {
        self.stiffness = stiffness;
        self
    }

    fn damping(mut self, damping: f32) -> Self {
        self.damping = damping;
        self
    }

    /// Update the spring state by a small time-step (dt, in seconds).
    fn update(&mut self, dt: f32) {
        let displacement = self.value - self.target;
        let force = -self.stiffness * displacement - self.damping * self.velocity;
        let acceleration = force / self.mass;
        self.velocity += acceleration * dt;
        self.value += self.velocity * dt;
    }
}

/// Each word in the text will be animated using its own spring.
/// We add a `has_reached_target` flag to avoid further work once done.
struct AnimatedWord {
    word: String,
    delay: f32,
    spring: RefCell<Spring>,
    spring_value: Mutable<f32>,
    has_reached_target: Cell<bool>,
}

/// The text to animate. In addition to the animated words and config,
/// we add a flag to indicate when the animation is complete.
struct Text {
    content: Arc<str>,
    animated_words: Vec<AnimatedWord>,
    config: AnimationConfig,
    animation_complete: Mutable<bool>,
}

impl Text {
    /// Create a new Text instance with the given configuration.
    fn new(content: Arc<str>, config: AnimationConfig) -> Rc<Self> {
        // Split the text into words using split_whitespace.
        // (If you want to preserve extra spaces or punctuation, you might need to
        // use a different splitting method.)
        let words: Vec<&str> = content.split_whitespace().collect();
        let animated_words = words
            .into_iter()
            .enumerate()
            .map(|(i, word)| {
                let delay = i as f32 * config.delay_factor;
                AnimatedWord {
                    word: word.to_string(),
                    delay,
                    spring: RefCell::new(Spring::new_with_config(config)),
                    spring_value: Mutable::new(config.initial),
                    has_reached_target: Cell::new(false),
                }
            })
            .collect();

        Rc::new(Self {
            content,
            animated_words,
            config,
            animation_complete: Mutable::new(false),
        })
    }

    /// Render the text.
    ///
    /// When the animation is not complete, we render the animated words.
    /// Once it’s finished, we render the whole text as one span.
    fn render(text: Rc<Self>) -> Dom {
        let initial = text.config.initial;
        html!("span", {
            .child_signal(text.animation_complete.signal().map(clone!(text => move |complete| {
                if complete {
                    // Render plain text when the animation is done.
                    Some(html!("span", {
                        .text(&*text.content)
                    }))
                } else {
                    // Render each word as an animated span.
                    Some(html!("span", {
                        .children(text.animated_words.iter().enumerate().map(|(i, animated)| {
                            // We add a trailing space after each word except the last.
                            let trailing_space = if i < text.animated_words.len()-1 {
                                "\u{00a0}"
                            } else {
                                ""
                            };
                            html!("span", {
                                .style("display", "inline-block")
                                .style_signal("transform", animated.spring_value.signal().map(|val| {
                                    format!("translateY({}rem)", val)
                                }))
                                .style_signal("opacity", animated.spring_value.signal().map(move |val| {
                                    // Fade in the word as the animation progresses.
                                    (val - initial).abs().min(1.0).to_string()
                                }))
                                .text(&format!("{}{}", animated.word, trailing_space))
                            })
                        }))
                    }))
                }
            })))
        })
    }
}

/// Runs the spring animation and, when finished, sets the `animation_complete` flag.
///
/// In this version we only update springs that have not yet reached their target.
/// We also limit the number of springs updated per frame to reduce CPU load.
async fn run_animation(text: Rc<Text>) {
    use futures::channel::oneshot;
    let (sender, receiver) = oneshot::channel::<()>();
    let sender = Rc::new(RefCell::new(Some(sender)));
    let start_time = Date::now();

    // Instead of scanning all springs every frame, we keep an index into the
    // text.animated_words vector to "activate" springs as their delay passes.
    let total_words = text.animated_words.len();
    let mut next_index: usize = 0;
    // Keep track of indices of springs that are currently active.
    let mut active_indices: Vec<usize> = Vec::new();
    // Set a limit on how many springs to update per frame.
    const ACTIVE_SPRING_LIMIT: usize = 400;

    spawn_local({
        let sender = sender.clone();
        let text = text.clone();
        async move {
            let mut interval = IntervalStream::new(16);
            let dt: f32 = 0.016;
            loop {
                interval.next().await;
                let now = Date::now();
                // Convert time values to seconds.
                let elapsed = (now - start_time) as f32 / 1000.0 - text.config.start_delay;
                if elapsed < 0.0 {
                    continue;
                }

                // Activate new springs whose delay has passed.
                while next_index < total_words && elapsed >= text.animated_words[next_index].delay {
                    if !text.animated_words[next_index].has_reached_target.get() {
                        active_indices.push(next_index);
                    }
                    next_index += 1;
                }

                let mut springs_updated = 0;
                // Update only up to ACTIVE_SPRING_LIMIT springs.
                for &i in active_indices.iter() {
                    if springs_updated >= ACTIVE_SPRING_LIMIT {
                        break;
                    }
                    let animated = &text.animated_words[i];
                    let mut spring = animated.spring.borrow_mut();
                    // Once the spring is activated, set its target.
                    if (spring.target - text.config.target).abs() > f32::EPSILON {
                        spring.target = text.config.target;
                    }
                    spring.update(dt);
                    animated.spring_value.set(spring.value);
                    // If the spring is close enough to its target, mark it as done.
                    if (spring.value - spring.target).abs() < 0.001 && spring.velocity.abs() < 0.001
                    {
                        animated.has_reached_target.set(true);
                    } else {
                        springs_updated += 1;
                    }
                }

                // Remove springs that have reached their target.
                active_indices.retain(|&i| !text.animated_words[i].has_reached_target.get());

                // If there are no more springs to update and we've already activated all, finish.
                if next_index >= total_words && active_indices.is_empty() {
                    if let Some(s) = sender.borrow_mut().take() {
                        let _ = s.send(());
                    }
                    text.animation_complete.set(true);
                    break;
                }
            }
        }
    });

    let _ = receiver.await;
}

/// A helper to render a header using animated text.
fn header(text: &str) -> Dom {
    let config = AnimationConfig {
        mass: 2.0,
        stiffness: 600.0,
        damping: 20.0,
        initial: -1.0,
        target: 0.0,
        delay_factor: 0.015,
        start_delay: 0.0,
    };
    let text_rc = Text::new(text.into(), config);
    spawn_local({
        let text_rc = text_rc.clone();
        async move {
            run_animation(text_rc.clone()).await;
        }
    });
    html!("div", {
        .style("font-size", "2rem")
        .children(&mut [
            Text::render(text_rc),
        ])
    })
}

/// A helper to render normal text.
fn text(text: &str) -> Dom {
    let config = AnimationConfig {
        mass: 1.0,
        stiffness: 400.0,
        damping: 11.0,
        initial: -2.0,
        target: 0.0,
        delay_factor: 0.01,
        start_delay: 0.2,
    };
    let text_rc = Text::new(text.into(), config);
    spawn_local({
        let text_rc = text_rc.clone();
        async move {
            run_animation(text_rc.clone()).await;
        }
    });
    html!("div", {
        .style("font-size", "1.5rem")
        .children(&mut [
            Text::render(text_rc),
        ])
    })
}

/// The entry point.
#[wasm_bindgen(start)]
pub fn main_js() -> Result<(), JsValue> {
    #[cfg(debug_assertions)]
    console_error_panic_hook::set_once();

    let mut value: Arc<str> = "Hello, World!".into();
    for _ in 0..9 {
        value = format!("{} {}", value, value).into();
    }

    dominator::append_dom(
        &dominator::body(),
        html!("div", {
            .children(&mut [
                html!("div", {
                    .style("position", "fixed")
                    .style("top", "0")
                    .style("left", "0")
                    .style("right", "0")
                    .style("background", "white")
                    .style("display", "flex")
                    .style("justify-content", "center")
                    .style("align-items", "center")
                    .style("box-shadow", "0 0 0.5rem rgba(0, 0, 0, 0.5)")
                    .style("z-index", "1000")
                    .style("height", "4rem")
                    .style("font-size", "2rem")
                    .children(&mut [
                        text("Leif Adamec Rydenfalk"),
                    ])
                }),
                html!("div", {
                    .style("height", "5rem")
                }),
                header("Adamec Portfolio Website"),
                text(value.as_ref()),
                html!("div", {
                    .style("height", "100vh")
                }),
            ])
        }),
    );

    Ok(())
}
