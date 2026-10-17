//! Production font controller bodies with deterministic UI/idle boundaries.
//! The font helper still uses native Pango; this does not claim GTK rendering.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
const FONT_PERSIST_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);
mod glib {
    use super::*;
    type Callback = Box<dyn FnOnce()>;
    thread_local! {
        static IDLES: RefCell<Vec<Callback>> = RefCell::new(Vec::new());
        static TIMERS: RefCell<Vec<Callback>> = RefCell::new(Vec::new());
    }
    pub fn idle_add_local_once(f: impl FnOnce() + 'static) {
        IDLES.with(|q| q.borrow_mut().push(Box::new(f)));
    }
    pub fn timeout_add_local_once(_: std::time::Duration, f: impl FnOnce() + 'static) {
        TIMERS.with(|q| q.borrow_mut().push(Box::new(f)));
    }
    pub fn run_idles() {
        for callback in IDLES.with(|q| q.take()) {
            callback();
        }
    }
    pub fn run_timers() {
        for callback in TIMERS.with(|q| q.take()) {
            callback();
        }
    }
}
#[derive(Clone)]
struct Config {
    font_desc: String,
    default_font_scale: f64,
    chrome_font_size: String,
    chrome_font_scale: f64,
}
mod font {
    pub(crate) use super::font_description_with_size;
    pub fn terminal_font_description(desc: &str, _: &super::Config) -> super::FontDescription {
        super::FontDescription::from_string(desc)
    }
}
#[derive(Clone, Default)]
struct Terminal {
    size: Rc<Cell<i32>>,
    scale: Rc<Cell<f64>>,
    scale_writes: Rc<Cell<u32>>,
}
impl Terminal {
    fn set_font(&self, font: Option<&FontDescription>) {
        self.size.set(font.unwrap().size());
    }
    fn set_font_scale(&self, scale: f64) {
        self.scale.set(scale);
        self.scale_writes.set(self.scale_writes.get() + 1);
    }
}
#[derive(Clone)]
struct View {
    terminal: Terminal,
    config: Rc<RefCell<Config>>,
}
impl View {
    fn set_font(&self, desc: &str) {
        self.terminal
            .set_font(Some(&FontDescription::from_string(desc)));
        self.config.borrow_mut().font_desc = desc.into();
    }
    fn set_font_scale(&self, scale: f64) {
        self.terminal.set_font_scale(scale);
        self.config.borrow_mut().default_font_scale = scale;
    }
}
#[derive(Clone)]
struct Leaf {
    terminal: Terminal,
    view: Option<View>,
}
impl Leaf {
    fn new(config: &Config, is_block: bool) -> Self {
        let terminal = Terminal::default();
        terminal.set_font(Some(&FontDescription::from_string(&config.font_desc)));
        terminal.set_font_scale(config.default_font_scale);
        let view = is_block.then(|| View {
            terminal: terminal.clone(),
            config: Rc::new(RefCell::new(config.clone())),
        });
        Self { terminal, view }
    }
    fn block_view(&self) -> Option<&View> {
        self.view.as_ref()
    }
    fn terminal(&self) -> &Terminal {
        &self.terminal
    }
}
#[derive(Clone, Default)]
struct Dirty(Rc<Cell<u64>>);
impl Dirty {
    fn record_edit(&self) {
        self.0.set(self.0.get() + 1);
    }
}
#[derive(Clone)]
struct UiState {
    config: Rc<RefCell<Config>>,
    font_scale: Rc<Cell<f64>>,
    pending_font_scale: Rc<Cell<Option<f64>>>,
    pending_chrome_font_scale: Rc<Cell<Option<f64>>>,
    font_persist_generation: Rc<Cell<u64>>,
    config_dirty: Dirty,
    leaves: Rc<RefCell<Vec<Leaf>>>,
    css: Rc<RefCell<String>>,
    paints: Rc<Cell<u32>>,
    persists: Rc<Cell<u32>>,
    saved: Rc<RefCell<Vec<Config>>>,
}
impl UiState {
    fn new() -> Self {
        let config = Config {
            font_desc: "Monospace 12.5".into(),
            default_font_scale: 1.0,
            chrome_font_size: "12.5".into(),
            chrome_font_scale: 1.0,
        };
        let leaves = [true, true, false].map(|block| Leaf::new(&config, block));
        let ui = Self {
            config: Rc::new(RefCell::new(config)),
            font_scale: Rc::new(Cell::new(1.0)),
            pending_font_scale: Default::default(),
            pending_chrome_font_scale: Default::default(),
            font_persist_generation: Default::default(),
            config_dirty: Default::default(),
            leaves: Rc::new(RefCell::new(leaves.to_vec())),
            css: Default::default(),
            paints: Default::default(),
            persists: Default::default(),
            saved: Default::default(),
        };
        ui.apply_dynamic_css();
        ui
    }
    fn configuration_leaves(&self) -> Vec<Leaf> {
        self.leaves.borrow().clone()
    }
    fn apply_dynamic_css(&self) {
        let config = self.config.borrow();
        *self.css.borrow_mut() = window_font_css(
            &config.font_desc,
            self.font_scale.get(),
            &config.chrome_font_size,
            config.chrome_font_scale,
        );
        self.paints.set(self.paints.get() + 1);
    }
    fn persist_config(&self) {
        self.persists.set(self.persists.get() + 1);
        self.saved.borrow_mut().push(self.config.borrow().clone());
    }
    fn settings_main_size(&self, value: f64) {
        let ui = self;
        let row = Row(value);
        // @font-sync:main-size
    }
    fn settings_chrome_size(&self, value: f64) {
        let ui = self;
        let row = Row(value);
        let chrome_absolute =
            FontDescription::from_string(&self.config.borrow().chrome_font_size).is_size_absolute();
        // @font-sync:chrome-size
    }
    // @font-sync:apply-scale
    // @font-sync:apply-group-scale
    // @font-sync:set-scale-all
    // @font-sync:apply-font-all
}
struct Row(f64);
impl Row {
    fn value(&self) -> f64 {
        self.0
    }
}
// @font-sync:claim-sweep

#[test]
fn font_zoom_burst_updates_all_panes_with_fixed_chrome_in_one_idle() {
    let ui = UiState::new();
    assert!(ui.css.borrow().contains("12.5pt"));
    for scale in [1.025, 1.05, 1.1, 1.2] {
        ui.apply_font_scale(scale);
    }
    assert_eq!(ui.font_scale.get(), 1.2);
    assert_eq!(ui.config.borrow().default_font_scale, 1.2);
    assert_eq!(
        ui.paints.get(),
        1,
        "one pending sweep, no immediate repaint train"
    );
    glib::run_idles();
    assert_eq!(ui.paints.get(), 2);
    assert!(ui.css.borrow().contains("15pt"));
    assert!(ui.css.borrow().contains("12.5pt"));
    assert_eq!(ui.config.borrow().chrome_font_scale, 1.0);
    for leaf in ui.configuration_leaves() {
        assert_eq!(leaf.terminal().scale.get(), 1.2);
        if let Some(view) = leaf.block_view() {
            assert_eq!(view.config.borrow().default_font_scale, 1.2);
        }
    }
    glib::run_timers();
    assert_eq!(ui.persists.get(), 1, "only the latest generation persists");
}

#[test]
fn settings_change_and_new_panes_share_current_scaled_size() {
    let ui = UiState::new();
    ui.apply_font_scale(1.5);
    glib::run_idles();
    ui.config.borrow_mut().font_desc = "Monospace 17px".into();
    ui.apply_font_all();
    assert!(ui.css.borrow().contains("25.5px"));
    let late = Leaf::new(&ui.config.borrow(), true);
    ui.leaves.borrow_mut().push(late);
    for leaf in ui.configuration_leaves() {
        assert_eq!(leaf.terminal().size.get(), 17 * 1024);
        assert_eq!(leaf.terminal().scale.get(), 1.5);
        if let Some(view) = leaf.block_view() {
            assert_eq!(view.config.borrow().font_desc, "Monospace 17px");
        }
    }
    ui.apply_font_scale(1.0);
    glib::run_idles();
    glib::run_timers();
    assert!(ui.css.borrow().contains("17px"));
    for leaf in ui.configuration_leaves() {
        assert_eq!(leaf.terminal().scale.get(), 1.0);
    }
}

#[test]
fn explicit_reload_scale_refreshes_chrome_without_a_queued_sweep() {
    let ui = UiState::new();
    *ui.config.borrow_mut() = Config {
        font_desc: "Monospace 18".into(),
        default_font_scale: 0.75,
        chrome_font_size: "16px".into(),
        chrome_font_scale: 1.5,
    };
    ui.set_font_scale_all(0.75);
    ui.apply_font_all();
    assert!(ui.css.borrow().contains("13.5pt"));
    assert!(ui.css.borrow().contains("24px"));
    assert_eq!(ui.pending_font_scale.get(), None);
    for leaf in ui.configuration_leaves() {
        assert_eq!(leaf.terminal().size.get(), 18 * 1024);
        assert_eq!(leaf.terminal().scale.get(), 0.75);
    }
}

#[test]
fn chrome_burst_never_walks_terminals_and_mixed_bursts_save_both_groups() {
    let ui = UiState::new();
    let writes: Vec<_> = ui
        .configuration_leaves()
        .iter()
        .map(|leaf| leaf.terminal().scale_writes.get())
        .collect();
    for scale in [1.1, 1.2, 1.5] {
        ui.apply_font_group_scale(font_zoom::FontGroup::Chrome, scale);
    }
    assert_eq!(ui.font_scale.get(), 1.0);
    assert_eq!(ui.config.borrow().default_font_scale, 1.0);
    glib::run_idles();
    assert_eq!(ui.paints.get(), 2);
    assert!(ui.css.borrow().contains("18.75pt"));
    for (leaf, before) in ui.configuration_leaves().iter().zip(writes) {
        assert_eq!(leaf.terminal().scale_writes.get(), before);
    }
    ui.apply_font_scale(0.75);
    ui.apply_font_group_scale(font_zoom::FontGroup::Chrome, 2.0);
    glib::run_idles();
    glib::run_timers();
    assert_eq!(ui.persists.get(), 1);
    let saved = ui.saved.borrow();
    assert_eq!(saved[0].default_font_scale, 0.75);
    assert_eq!(saved[0].chrome_font_scale, 2.0);
    assert_eq!(saved[0].chrome_font_size, "12.5");
}

#[test]
fn actual_settings_callback_bodies_keep_other_group_size_and_units() {
    let ui = UiState::new();
    ui.config.borrow_mut().font_desc = "Monospace Bold Italic 17px".into();
    ui.config.borrow_mut().chrome_font_size = "13.5".into();
    ui.apply_font_scale(1.2);
    ui.apply_font_group_scale(font_zoom::FontGroup::Chrome, 0.8);
    glib::run_idles();
    ui.settings_main_size(18.25);
    assert_eq!(
        effective_font_size_css(&ui.config.borrow().font_desc, 1.0),
        "18.25px"
    );
    assert!(ui.config.borrow().font_desc.contains("Bold Italic"));
    assert_eq!(ui.config.borrow().chrome_font_size, "13.5");
    assert_eq!(ui.config.borrow().chrome_font_scale, 0.8);
    let main_before = ui.config.borrow().font_desc.clone();
    ui.settings_chrome_size(16.5);
    assert_eq!(ui.config.borrow().chrome_font_size, "16.5");
    assert_eq!(ui.config.borrow().font_desc, main_before);
    assert_eq!(ui.config.borrow().default_font_scale, 1.2);
    ui.config.borrow_mut().chrome_font_size = "17px".into();
    ui.settings_chrome_size(19.25);
    assert_eq!(ui.config.borrow().chrome_font_size, "19.25px");
    assert_eq!(ui.config.borrow().font_desc, main_before);
    glib::run_timers();
    let saved = ui.saved.borrow();
    assert_eq!(saved.last().unwrap().chrome_font_size, "19.25px");
    assert_eq!(saved.last().unwrap().font_desc, main_before);
}
