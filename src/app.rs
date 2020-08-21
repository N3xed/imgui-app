/*!
Implements the main event loop and UI managment.

The `App` struct is the central manager for events and windows. For the event loop to run you must call
`App::run()` in the main thread. An `AppHandle` instance created using the `App::handle()` method is used
to send messages and execute code in the main or UI thread from a different thread. All GUI related methods
are not thread-safe and can only be called in the UI thread (the thread that runs `App::run()`).

The platform specific windows are managed using the [winit](https://crates.io/crates/winit) crate. Winit
is a window creation and management library. It can create windows and lets you handle events (for example:
the window being resized, a key being pressed, a mouse movement, etc.) produced by window.

The GUI is created using the [Dear ImGui](https://github.com/ocornut/imgui) C++ library and its rust bindings
[imgui-rs](https://github.com/Gekkio/imgui-rs). ImGui is a first of its kind library that enables GUI creation
through simple function calls, where the state and rendering of the GUI is in the same place. This enables quick
creation of complex GUIs with minimal performance impact (see [here](https://github.com/ocornut/imgui#how-it-works)
for more information).
*/

use super::{ErrorCode, Layout, UiError, UiResult, Window, WindowId};
use crate::utils;
use std::cell::RefCell;
use std::cell::{Cell, UnsafeCell};
use std::collections::hash_map::HashMap;
use std::error::Error;
use std::path::Path;
use std::rc::Rc;
use winit::event_loop::ControlFlow;

pub type Event<'a> = winit::event::Event<'a, AppEvent>;
pub type EventLoop = winit::event_loop::EventLoop<AppEvent>;
pub type EventLoopWindowTarget = winit::event_loop::EventLoopWindowTarget<AppEvent>;
pub type EventLoopProxy = winit::event_loop::EventLoopProxy<AppEvent>;

/// This struct contains all data for the GUI and all NativeWindows.
pub struct App {
    windows: HashMap<WindowId, Window>,
    main_window_id: Option<WindowId>,
    next_tick: std::time::Instant,
    event_loop_proxy: EventLoopProxy,
    event_loop: Option<EventLoop>,
    /// A WGPU context used for all `Window`s.
    pub(crate) wgpu_instance: wgpu::Instance,
    /// The amount of MSAA samples for each frame, defaults to 4.
    pub msaa_samples: u32,
    /// The font atlas shared between all windows and imgui contexts.
    pub font_atlas: Rc<RefCell<imgui::SharedFontAtlas>>,
    pub event_loop_window_target: Option<*const EventLoopWindowTarget>,
}

impl App {
    /// Create a new App instance.  
    ///
    /// Only one instance must ever be created per program, else this function will panic.
    pub fn new() -> App {
        let event_loop = EventLoop::with_user_event();
        App {
            windows: HashMap::new(),
            main_window_id: None,
            next_tick: std::time::Instant::now(),
            event_loop_proxy: event_loop.create_proxy(),
            event_loop: Some(event_loop),
            wgpu_instance: wgpu::Instance::new(wgpu::BackendBit::PRIMARY),
            msaa_samples: 1,
            font_atlas: Rc::new(RefCell::new(imgui::SharedFontAtlas::create())),
            event_loop_window_target: None,
        }
    }

    /// Sets the main window that is returned by the `main_window()` method.
    ///
    /// The first window that is created is automatically taken as the main window. If a
    /// window is closed / destroyed that is currently set as the main window, the main
    /// window will be set to `None`.
    #[inline]
    pub fn set_main_window(&mut self, window: Option<&Window>) {
        self.main_window_id = match window {
            Some(wnd) => Some(wnd.id()),
            None => None,
        };
    }

    /// Gets the current main window, or `None` if no such window exists.
    #[inline]
    pub fn main_window(&mut self) -> Option<&mut Window> {
        match self.main_window_id {
            Some(id) => self.windows.get_mut(&id),
            None => None,
        }
    }

    /// Loads a font from a file given by `path` and returnes its new id.
    pub fn load_font<P: AsRef<Path>>(
        &mut self,
        path: P,
        size_pixels: f32,
    ) -> UiResult<imgui::FontId> {
        let buf = std::fs::read(path)?;
        self.load_font_bytes(buf.as_slice(), size_pixels)
    }

    /// Loads a font from a byte buffer and returns its new id.
    pub fn load_font_bytes(&mut self, font: &[u8], size_pixels: f32) -> UiResult<imgui::FontId> {
        let mut atlas = self.font_atlas.borrow_mut();

        let font_source = imgui::FontSource::TtfData {
            data: font,
            size_pixels,
            config: None,
        };
        Ok(atlas.add_font(&[font_source]))
    }

    /// Removes the window from the app, it is automatically closed and destroyed.
    ///
    /// This destroyes the window instance referred to by `id`. Once the window is
    /// dropped it automatically closes. If no further windows exist, this method
    /// sends a `Quit` event, which exits the message loop in `App::run()`.
    ///
    /// *This method is for internal use only.* To close/remove/destroy a window call
    /// its `Window::close()` method.
    pub(super) fn remove_window(&mut self, id: WindowId) {
        match self.windows.remove(&id) {
            Some(wnd) => {
                if let Some(main_id) = self.main_window_id {
                    if main_id == wnd.id() {
                        self.main_window_id = None;
                    }
                }
            }
            None => (),
        };

        if self.windows.is_empty() {
            let _ = self.event_loop_proxy.send_event(AppEvent::Quit);
        }
    }

    /// Called when the settings for imgui get updated.
    pub(super) fn apply_imgui_settings(&self, imgui: &mut imgui::Context) {
        imgui.set_ini_filename(None);
        // TODO
    }

    /// Tries to get a reference to a `NativeWindow` by its id.
    pub fn window(&self, id: WindowId) -> Option<&Window> {
        self.windows.get(&id)
    }

    /// Tries to get a mutable reference to a `NativeWindow` by its id.
    pub fn window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.get_mut(&id)
    }

    /// Gets the `EventLoopWindowTarget` used for window creation.
    /// If not available panics.  
    /// *For internal use only.*
    fn event_loop(&self) -> &EventLoopWindowTarget {
        if let Some(ref evt_loop) = self.event_loop {
            evt_loop
        } else if let Some(evt_loop) = self.event_loop_window_target {
            unsafe { &*(evt_loop as *const EventLoopWindowTarget) }
        } else {
            panic!("Tried to get invalid reference to event loop.");
        }
    }

    /// Creates a new window.
    ///
    /// The window is created by calling the `window_builder_func` closure. This method
    /// calls `Window::new()` with the boxed `data_model` and  the created
    /// `winit::window::Window`. It then adds the window to the app's list of windows and
    /// also sets it as the main window (see `App::set_main_window()`) if no windows
    /// existed before this method call.
    ///
    /// ## Example
    /// ```no_run
    /// let window = app
    ///     .new_window(data_model, &|app| {
    ///         ui::Window::build_window("Title", (1200, 800))
    ///     })
    ///     .unwrap();
    /// ```
    pub fn new_window<'a>(
        &'a mut self,
        data_model: impl Layout + 'static,
        window_builder_func: &impl Fn(&App) -> Result<winit::window::WindowBuilder, Box<dyn Error>>,
    ) -> UiResult<&'a mut Window> {
        let mut wnd_builder: winit::window::WindowBuilder = match window_builder_func(self) {
            Ok(w) => w,
            Err(err) => {
                return Err(UiError::with_boxed_source(
                    ErrorCode::WINDOW_BUILD_FAILED,
                    err,
                ))
            }
        };

        // Prevents the window from flickering white as much when shown initially.
        let visible = wnd_builder.window.visible;
        wnd_builder.window.visible = false;

        let wnd = match wnd_builder.build(self.event_loop()) {
            Ok(w) => w,
            Err(err) => return Err(UiError::with_source(ErrorCode::WINDOW_BUILD_FAILED, err)),
        };

        let mut window: Window = Window::new(self, Box::new(data_model), wnd, visible)?;
        let window_ptr = &mut window as *mut _;

        match window.data_model.init(unsafe { &mut *window_ptr }) {
            Err(err) => {
                return Err(UiError::with_boxed_source(
                    ErrorCode::WINDOW_BUILD_FAILED,
                    err,
                ))
            }
            Ok(_) => (),
        };

        let id = window.id();
        let first_window = self.windows.is_empty();

        let entry: std::collections::hash_map::Entry<_, _> = self.windows.entry(id);
        if let std::collections::hash_map::Entry::Occupied(val) = entry {
            panic!(
                "Tried to create a window with an id ({:?}) that already exists.",
                val.key()
            );
        }

        if first_window {
            self.main_window_id = Some(id);
        }

        Ok(entry.or_insert(window))
    }

    /// Runs the event loop of the GUI.  
    ///
    /// This method hijacks the main thread and never returns. Only the main thread (the
    /// thread the initially ran `main()`) must ever run this method, otherwise it will
    /// panic.
    pub fn run(mut self) -> ! {
        use winit::event::*;

        let mut continuous_updated_windows = HashMap::<WindowId, super::InvalidateAmount>::new();
        let mut executes_at =
            Vec::<(Cell<std::time::Instant>, UnsafeCell<ExecuteAtCallback>)>::new();

        let event_loop = self.event_loop.take().unwrap();

        let closure = move |evt: Event<'_, _>,
                            wnd_target: &EventLoopWindowTarget,
                            ctrl_flow: &mut ControlFlow| {
            self.event_loop_window_target = Some(wnd_target);

            match evt {
                Event::NewEvents(_) => {
                    let now = std::time::Instant::now();
                    // Execute and possible remove the callbacks.
                    executes_at.retain(|(instant, callback)| {
                        if instant.get() < now {
                            let next_instant = unsafe { (&mut *callback.get()).0(self.handle()) };

                            if let Some(next_instant) = next_instant {
                                instant.set(next_instant);
                                self.next_tick = utils::min_instant(self.next_tick, next_instant);
                                return true;
                            }

                            false
                        } else {
                            self.next_tick = utils::min_instant(self.next_tick, instant.get());
                            true
                        }
                    });
                    self.next_tick = utils::max_instant(self.next_tick, now);

                    // figure out wait time for next event
                    use std::time::{Duration, Instant};
                    if continuous_updated_windows.is_empty() && executes_at.is_empty() {
                        *ctrl_flow =
                            ControlFlow::WaitUntil(Instant::now() + Duration::from_secs(5));
                    } else {
                        if !continuous_updated_windows.is_empty() {
                            *ctrl_flow = ControlFlow::Poll;
                        } else {
                            *ctrl_flow = ControlFlow::WaitUntil(self.next_tick);
                        }
                    }

                    // request redraw for continuous updated windows
                    continuous_updated_windows.retain(|id, state| {
                        let retain = match state {
                            super::InvalidateAmount::Stop => return false,
                            super::InvalidateAmount::Once => false,
                            super::InvalidateAmount::Until(instant) if *instant < now => false,
                            _ => {
                                true
                            }
                        };

                        match self.window_mut(*id) {
                            Some(window) => {
                                // TODO: check if this causes event lag?
                                window.window().request_redraw();
                            },
                            None => {
                                log::warn!("WindowId {:?} not found when continously updating. Removing window.", id);
                                return false;
                            }
                        }
                        retain
                    });
                }
                Event::RedrawRequested(window_id) => {
                    let app_ptr = &self as *const App;
                    if let Some(wnd) = self.window_mut(window_id) {
                        let frame_delta = wnd.update_frame_time();
                        let mut active_wnd: super::ActiveWindow<'_> = match wnd.activate() {
                            Ok(w) => w,
                            Err(err) => return log::warn!("{}", err),
                        };
                        match active_wnd.render(unsafe { &*app_ptr }, frame_delta) {
                            Err(err) => log::warn!("{}", err),
                            Ok(_) => (),
                        }
                    }
                }

                Event::UserEvent(app_evt) => match app_evt {
                    AppEvent::Quit => {
                        *ctrl_flow = ControlFlow::Exit;
                        // return early to skip changing of the control flow below
                        return;
                    }

                    AppEvent::Execute { callback } => {
                        callback.0(&mut self);
                    }

                    AppEvent::ExecuteAt { instant, callback } => {
                        executes_at.push((Cell::new(instant), UnsafeCell::new(callback)));
                    }

                    AppEvent::ExecuteWithWindow {
                        window_id,
                        callback,
                    } => {
                        let app_ptr = &mut self as *mut _;
                        let wnd = match self.window_mut(window_id) {
                            Some(w) => w,
                            None => {
                                return log::warn!("Failed to get window id ({:?}).", window_id)
                            }
                        };
                        callback.0(unsafe { &mut *app_ptr }, wnd);
                    }

                    AppEvent::SetWindowInvalidateAmount { window_id, state } => {
                        continuous_updated_windows.insert(window_id, state);
                        *ctrl_flow = ControlFlow::Poll;
                    }

                    AppEvent::InvalidateWindow { window_id } => {
                        match self.window(window_id) {
                            Some(w) => w.window().request_redraw(),
                            None => log::warn!("Failed to get window id ({:?}).", window_id),
                        };
                    }
                },

                event => {
                    let app_ptr = &self as *const App;

                    for (_, wnd) in self.windows.iter_mut() {
                        let mut active_wnd = match wnd.activate() {
                            Ok(w) => w,
                            Err(err) => return log::warn!("{}", err),
                        };

                        active_wnd.on_event(unsafe { &*app_ptr }, &event);
                    }
                }
            }

            self.event_loop_window_target = None;
        };
        event_loop.run(closure);
    }

    /// Creates a thread-safe handle to the `App` instance.
    ///
    /// This handle can be used to send asynchronous events, to modify the app, any window
    /// or to execute code in the main thread.
    #[inline]
    pub fn handle(&self) -> AppHandle {
        AppHandle {
            event_loop_proxy: self.event_loop_proxy.clone(),
        }
    }
}

/// A thread-safe handle to the `App` instance.
///
/// Enables modification of the GUI and app from another thread.
#[derive(Clone)]
pub struct AppHandle {
    event_loop_proxy: EventLoopProxy,
}

impl AppHandle {
    /// Sends a custom event to the event loop in the UI thread.
    pub fn send_event(&self, event: AppEvent) -> UiResult<()> {
        match self.event_loop_proxy.send_event(event) {
            Err(err) => Err(super::UiError::with_source(
                ErrorCode::EVENT_LOOP_CLOSED,
                err,
            )),
            Ok(_) => Ok(()),
        }
    }

    /// Executes the closure `callback` in the UI thread with a mutable reference to `App`.
    pub fn execute_with_gui(&self, callback: impl FnOnce(&mut App) + 'static) -> UiResult<()> {
        self.send_event(AppEvent::Execute {
            callback: ExecuteCallback(Box::new(callback)),
        })
    }

    /// Requests the main window to redraw.
    ///
    /// Redraws the main window set with `App::set_main_window()`.
    pub fn redraw_main_window(&self) -> UiResult<()> {
        self.execute_with_gui(|app: &mut App| {
            if let Some(wnd) = app.main_window() {
                wnd.request_invalidate();
            }
        })
    }
}

/// A callback to be executed in the UI thread.
///
/// It returnes an optional `Instant` that specifies at what time this callback should be
/// called again. If `None` is returned, the callback is only called once and then
/// discarded.
pub struct ExecuteAtCallback(pub Box<dyn FnMut(AppHandle) -> Option<std::time::Instant>>);

/// A callback that receives an `App` reference to be executed in the UI thread.
pub struct ExecuteCallback(pub Box<dyn FnOnce(&mut App)>);

/// A callback that receives an `App` and `Window` reference to be executed in the UI thread.
pub struct ExecuteWithWindowCallback(pub Box<dyn FnOnce(&mut App, &mut Window)>);

/// An enum that represents multiple app or window specific events.
#[derive(Debug)]
pub enum AppEvent {
    /// Quits the event loop.
    Quit,
    /// Requests  the window to invalidate in the UI thread.
    InvalidateWindow {
        /// The ID of the window to invalidate.
        window_id: WindowId,
    },
    /// Executes the closure `callback` in the UI thread.
    Execute {
        /// The closure to execute.
        ///
        /// It receives a mutable reference to the `App` instance.
        callback: ExecuteCallback,
    },
    /// Executes `callback` in the UI thread with a reference to window represented by `window_id`.
    ExecuteWithWindow {
        /// Identifies the window that gets passed as a mutable reference to `callback`.
        window_id: WindowId,
        /// The callback to execute in the UI thread with the `NativeWindow` and `App` reference.
        callback: ExecuteWithWindowCallback,
    },
    /// Schedules a closure to be executed in the UI thread approximately at a specific time.
    ExecuteAt {
        /// The instant when the closure `callback` should be executed.
        instant: std::time::Instant,
        /// The closure that should be executed at the specified time.
        callback: ExecuteAtCallback,
    },
    /// Sets the invalidate amount of the window identified by `window_id`.
    SetWindowInvalidateAmount {
        /// The `window_id` that identifies the window.
        window_id: WindowId,
        /// The invalidate amount that should be set for the window.
        state: super::InvalidateAmount,
    },
}

impl std::fmt::Debug for ExecuteCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<ExecuteCallback>())
    }
}
impl std::fmt::Debug for ExecuteWithWindowCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<ExecuteWithWindowCallback>())
    }
}
impl std::fmt::Debug for ExecuteAtCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<ExecuteAtCallback>())
    }
}
