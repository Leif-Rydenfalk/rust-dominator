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
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::js_sys::{Array, Date};
use web_sys::{
    console, window, IntersectionObserver, IntersectionObserverEntry, IntersectionObserverInit,
};

mod util;

// -------------------------------------------------------------------
// AnimationConfig holds the parameters for both the spring and linear opacity.
// -------------------------------------------------------------------
#[derive(Clone, Copy)]
struct AnimationConfig {
    mass: f32,
    stiffness: f32,
    damping: f32,
    initial: f32,
    target: f32,
    delay_factor: f32,
    start_delay: f32,
    opacity_duration: f32, // Duration (in seconds) for the opacity animation.
}

// -------------------------------------------------------------------
// Spring is a basic spring physics simulation.
// -------------------------------------------------------------------
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

    /// Update the spring state by a small time-step (dt, in seconds).
    fn update(&mut self, dt: f32) {
        let displacement = self.value - self.target;
        let force = -self.stiffness * displacement - self.damping * self.velocity;
        let acceleration = force / self.mass;
        self.velocity += acceleration * dt;
        self.value += self.velocity * dt;
    }
}

// -------------------------------------------------------------------
// WordAnimating holds the spring and linear opacity for one word.
// -------------------------------------------------------------------
struct WordAnimating {
    word: String,
    delay: f32,
    spring: RefCell<Spring>,
    spring_value: Mutable<f32>,
    opacity_value: Mutable<f32>,
    has_reached_target: Cell<bool>,
}

impl WordAnimating {
    fn new(word: &str, delay: f32, config: AnimationConfig) -> Self {
        Self {
            word: word.to_string(),
            delay,
            spring: RefCell::new(Spring::new_with_config(config)),
            spring_value: Mutable::new(config.initial),
            // Start opacity at 0.
            opacity_value: Mutable::new(0.0),
            has_reached_target: Cell::new(false),
        }
    }

    /// Update both the spring physics and the linear opacity.
    ///
    /// - `elapsed` is the time in seconds since this text’s animation started (after start_delay).
    /// - `dt` is the per‑frame delta time.
    /// - `config` holds the animation configuration.
    fn update(&self, elapsed: f32, dt: f32, config: &AnimationConfig) {
        // Update the spring animation.
        {
            let mut spring = self.spring.borrow_mut();
            if (spring.target - config.target).abs() > f32::EPSILON {
                spring.target = config.target;
            }
            spring.update(dt);
            let new_val = spring.value;
            if (new_val - self.spring_value.get()).abs() > 0.001 {
                self.spring_value.set(new_val);
            }
            // Mark as complete if close enough.
            if (spring.value - spring.target).abs() < 0.001 && spring.velocity.abs() < 0.001 {
                self.has_reached_target.set(true);
            }
        }

        // Update the linear opacity.
        let progress = ((elapsed - self.delay) / config.opacity_duration)
            .min(1.0)
            .max(0.0);
        self.opacity_value.set(progress);
    }
}

// -------------------------------------------------------------------
// Text holds the animated text content and a list of animated words.
// We also include bookkeeping for pending/active indices so that
// the global animation loop can update each text in a single pass.
// -------------------------------------------------------------------
struct Text {
    content: Arc<str>,
    animated_words: Vec<WordAnimating>,
    config: AnimationConfig,
    animation_complete: Mutable<bool>,
    // For tracking which words have started animating.
    pending_index: Cell<usize>,
    // Indices of words currently active (i.e. whose delay has passed).
    active_indices: RefCell<Vec<usize>>,
}

impl Text {
    /// Create a new Text instance.
    fn new(content: Arc<str>, config: AnimationConfig) -> Rc<Self> {
        // Split the text into words.
        let words: Vec<&str> = content.split_whitespace().collect();
        let animated_words: Vec<WordAnimating> = words
            .into_iter()
            .enumerate()
            .map(|(i, word)| {
                let delay = i as f32 * config.delay_factor;
                WordAnimating::new(word, delay, config)
            })
            .collect();

        let capacity = animated_words.len(); // Pre-allocate the expected capacity.

        let text = Rc::new(Self {
            content,
            animated_words,
            config,
            animation_complete: Mutable::new(false),
            pending_index: Cell::new(0),
            // Preallocate the vector with the expected capacity.
            active_indices: RefCell::new(Vec::with_capacity(capacity)),
        });

        // Register this text into the global active list.
        ACTIVE_TEXTS.with(|texts| texts.borrow_mut().push(text.clone()));
        text
    }

    /// Update this text’s animation:
    ///
    /// - Compute effective elapsed time (global elapsed minus start_delay).
    /// - Move pending words into the active list when their individual delay has passed.
    /// - Update each active word.
    /// - Mark the text complete when all words are done.
    fn update_all(&self, global_elapsed: f32, dt: f32) {
        // Subtract the text’s overall start_delay.
        let elapsed = global_elapsed - self.config.start_delay;
        if elapsed < 0.0 {
            // Animation for this text hasn't started yet.
            return;
        }

        let total_words = self.animated_words.len();
        // Move words from pending to active as their delay is reached.
        {
            let mut pending_index = self.pending_index.get();
            while pending_index < total_words {
                let word = &self.animated_words[pending_index];
                if elapsed >= word.delay {
                    self.active_indices.borrow_mut().push(pending_index);
                    pending_index += 1;
                } else {
                    break;
                }
            }
            self.pending_index.set(pending_index);
        }

        // Update each active word.
        {
            let mut active = self.active_indices.borrow_mut();
            // Iterate in reverse so we can remove finished words without issues.
            for i in (0..active.len()).rev() {
                let idx = active[i];
                self.animated_words[idx].update(elapsed, dt, &self.config);
                if self.animated_words[idx].has_reached_target.get() {
                    // Remove this index from the active list.
                    active.swap_remove(i);
                }
            }
        }

        // If all words have been processed and no active words remain, mark the text complete.
        if self.pending_index.get() == total_words && self.active_indices.borrow().is_empty() {
            self.animation_complete.set(true);
        }
    }

    /// Render the text. While the animation is running, we render each word individually.
    /// Once finished, we render the whole text as one span.
    fn render(text: Rc<Self>) -> Dom {
        html!("span", {
            // Here we add an id so we can locate this element from the DOM.
            .attr("id", "animated-text")
            .child_signal(text.animation_complete.signal().map(clone!(text => move |complete| {
                if complete {
                    // Render plain text when animation is done.
                    Some(html!("span", {
                        .text(&*text.content)
                    }))
                } else {
                    // Render each animated word.
                    Some(html!("span", {
                        .children(text.animated_words.iter().enumerate().map(|(i, animated)| {
                            // Add trailing non-breaking space except for the last word.
                            let trailing_space = if i < text.animated_words.len() - 1 {
                                "\u{00a0}"
                            } else {
                                ""
                            };
                            html!("span", {
                                .style("display", "inline-block")
                                .style_signal("transform", animated.spring_value.signal().map(|val| {
                                    format!("translateY({}rem)", val)
                                }))
                                .style_signal("opacity", animated.opacity_value.signal().map(|val| {
                                    val.to_string()
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

// -------------------------------------------------------------------
// Global registry for all animated texts.
// Using thread_local ensures that it is accessible from our animation loop.
// -------------------------------------------------------------------
thread_local! {
    static ACTIVE_TEXTS: RefCell<Vec<Rc<Text>>> = RefCell::new(Vec::new());
}

// -------------------------------------------------------------------
// A global flag for whether the animated text is visible in the viewport.
// We use this flag in our animation loop.
// -------------------------------------------------------------------
static TEXT_VISIBLE: Lazy<Mutable<bool>> = Lazy::new(|| Mutable::new(true));

// -------------------------------------------------------------------
// Global animation loop.
// This loop runs at ~60fps (dt ≈ 0.016 seconds) and updates all registered texts.
// When a text is complete, it is removed from the registry.
// We also check if the text is visible before doing updates.
// -------------------------------------------------------------------
async fn global_animation_loop() {
    let dt: f32 = 0.016; // ~60 fps
    let mut interval = IntervalStream::new(16);
    // Global start time in milliseconds.
    let global_start = Date::now();

    loop {
        interval.next().await;
        // Only update if the text is visible.
        if !TEXT_VISIBLE.get() {
            continue;
        }
        let now = Date::now();
        let global_elapsed = ((now - global_start) as f32) / 1000.0;

        ACTIVE_TEXTS.with(|texts| {
            let mut texts = texts.borrow_mut();
            texts.retain(|text| {
                // Update each text.
                text.update_all(global_elapsed, dt);
                // Keep the text if its animation is not complete.
                !text.animation_complete.get()
            });
        });
    }
}

// -------------------------------------------------------------------
// Setup an IntersectionObserver to watch the text container.
// When the element is off-screen, we set TEXT_VISIBLE to false.
// -------------------------------------------------------------------
fn setup_intersection_observer() {
    // Get the window and document.
    let window = window().expect("no global `window` exists");
    let document = window.document().expect("should have a document on window");

    // Try to get the element by id.
    // (You might need to delay this call until after the element is rendered.)
    if let Some(element) = document.get_element_by_id("animated-text") {
        let callback = Closure::wrap(Box::new(
            move |entries: Array, _observer: IntersectionObserver| {
                for entry in entries.iter() {
                    let entry = entry.dyn_into::<IntersectionObserverEntry>().unwrap();
                    let is_intersecting = entry.is_intersecting();
                    // Update the global visibility flag.
                    TEXT_VISIBLE.set(is_intersecting);
                }
            },
        ) as Box<dyn FnMut(Array, IntersectionObserver)>);

        let options = IntersectionObserverInit::new();
        // Adjust the thresholds as needed.
        options.set_threshold(&Array::of1(&0.1.into()));

        // Instead of using new_with_options (which may not be available),
        // use the standard constructor.
        let observer = IntersectionObserver::new(callback.as_ref().unchecked_ref())
            .expect("Failed to create IntersectionObserver");

        // Start observing the element.
        observer.observe(&element);

        // Forget the closure so it remains alive.
        callback.forget();
    }
}

// -------------------------------------------------------------------
// Helper to render a header using animated text.
// -------------------------------------------------------------------
fn header(text: &str) -> Dom {
    let config = AnimationConfig {
        mass: 2.0,
        stiffness: 600.0,
        damping: 20.0,
        initial: -1.0,
        target: 0.0,
        delay_factor: 0.015,
        start_delay: 0.0,
        opacity_duration: 0.5,
    };
    let text_rc = Text::new(text.into(), config);
    html!("div", {
        .style("font-size", "2rem")
        .children(&mut [
            Text::render(text_rc),
        ])
    })
}

// -------------------------------------------------------------------
// Helper to render normal animated text.
// -------------------------------------------------------------------
fn text(text: &str) -> Dom {
    let config = AnimationConfig {
        mass: 1.0,
        stiffness: 400.0,
        damping: 11.0,
        initial: -2.0,
        target: 0.0,
        delay_factor: 0.01,
        start_delay: 0.2,
        opacity_duration: 0.5,
    };
    let text_rc = Text::new(text.into(), config);
    html!("div", {
        .style("font-size", "1.5rem")
        .children(&mut [
            Text::render(text_rc),
        ])
    })
}

// -------------------------------------------------------------------
// Entry point.
// This sets up the DOM and starts the global animation loop.
// -------------------------------------------------------------------
#[wasm_bindgen(start)]
pub fn main_js() -> Result<(), JsValue> {
    // Enable better panic messages when debug_assertions is enabled.
    #[cfg(debug_assertions)]
    console_error_panic_hook::set_once();

    // Create a long text by repeatedly duplicating "Hello, World!".
    let mut value: Arc<str> = "Hello, World!".into();
    for _ in 0..12 {
        value = format!("{} {}", value, value).into();
    }

    // Start the global animation loop.
    spawn_local(global_animation_loop());

    // Append our DOM to the body.
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

    // Because the animated element may not be in the DOM immediately,
    // schedule the IntersectionObserver setup after a short delay.
    spawn_local(async {
        TimeoutFuture::new(100).await;
        setup_intersection_observer();
    });

    Ok(())
}
