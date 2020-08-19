//! General logic for OS window manipulation and creation.
//!
//! Each `Window` gets a data model, which is a struct that implements the `Layout`
//! trait. All window and UI specific state is contained in the data model and it is
//! responsible for the GUI layout (`Layout::layout()`).
//!
//! Manipulation of all GUI related things can only be done in the main thread. This is
//! possible by sending events either with a `ui::AppHandle` or `WindowHandle`.

use super::app::{self, App};
use super::ErrorCode;
use super::UiError;
use super::UiResult;
use crate::utils;
use std::any::Any;
use std::cell::Cell;
use std::error::Error;
use std::ops::Deref;
use std::ops::DerefMut;
use std::sync::Arc;
use std::sync::Weak;

pub use winit::window::WindowId;

/// This trait contains the functions which are used to layout the GUI.
///
/// You must implement this trait in your own class and create a `NativeWindow` with an
/// instance of that class (which is called the data model). On every update of the GUI,
/// the App instance calls the `NativeWindow::update()` and `NativeWindow::invalidate()`
/// method, which in turn calls `Layout::layout()`.
pub trait Layout {
    /// The central method for creating the GUI.
    fn layout(&mut self, ui: LayoutContext, app: &App, window: &mut Window);

    /// A user define logging method, which can be called from a thread-safe handle of the
    /// window.
    ///
    /// If `level` is `None`, the `message` is only displayed in the GUI and not logged.
    fn log(&mut self, level: Option<log::Level>, message: &str);

    /// Used for downcasting to the actual data model type.
    ///
    /// When manipulating the data model from a different thread using
    /// `NativeWindowHandle::run_with_data_model()`, this allows the cast from the
    /// `Layout` trait reference to the actual data model type.
    fn as_any(&mut self) -> &mut dyn Any;

    /// This method is used to initialize the data model.
    ///
    /// It will be called only once in the entire lifetime of the window, before the
    /// window is shown. If this method fails it can return a boxed `std::error::Error`
    /// and both `App::new_window()` and `NativeWindow::new()` will also fail and return
    /// the same error.
    fn init(&mut self, window: &mut Window) -> Result<(), Box<dyn Error>>;

    /// This method is called before the window gets closed and destroyed.
    /// The return value determines if the window actually gets closed.
    ///
    /// If the return value is `true`, the window will be closed and destroyed,
    /// otherwise when `false` it will remain open and the close action will be
    /// ignored.
    ///
    /// **Warning:**  
    /// Don't use this method for cleanup, because there is no guaratee that it is ever
    /// called (for example when the window creation fails). Implement the `Drop` trait
    /// for the data model, where all cleanup can then be done in the `Drop::drop()`
    /// method, which is guaranteed to be called when the data model is destroyed.
    fn before_close(&mut self, window: &mut Window) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InvalidateAmount {
    /// Possible continuous invalidation is stopped or inactive.
    Stop,
    /// The window will be invalidated once as soon as possible.
    Once,
    /// The window is continuously invalidated until the given instant.
    Until(std::time::Instant),
    /// The window is continuously invalidated indefinetly.
    Indefinetely,
}

impl InvalidateAmount {
    /// Wether or not continuous updating is currently active.
    ///
    /// `true` if `InvalidateAmount::Until(_)` or `InvalidateAmount::Indefinetely`,
    /// `false` otherwise.
    pub fn is_continuously(&self) -> bool {
        match self {
            Self::Stop => false,
            _ => true,
        }
    }
}

/// The context used to create the GUI using Dear ImGUI.
/// It is passed to the `Layout::layout()` method.
pub struct LayoutContext<'ui> {
    pub ui: &'ui imgui::Ui<'ui>,
    pub window_handle: WindowHandle,

    invalidate_amount_changed: Cell<bool>,
    invalidate_amount: Cell<InvalidateAmount>,
}

impl LayoutContext<'_> {
    /// Requests invalidation of the specified `amount` after the current frame is
    /// finished. The resulting requested invalidation amount is the maximum of
    /// all `request_invalidate()` calls for one frame.
    #[inline]
    pub fn request_invalidate(&self, amount: InvalidateAmount) {
        if self.invalidate_amount.get() < amount {
            self.invalidate_amount.set(amount);
        } else if self.invalidate_amount.get() == amount {
            self.invalidate_amount
                .set(match (self.invalidate_amount.get(), amount) {
                    (InvalidateAmount::Until(inst0), InvalidateAmount::Until(inst1)) => {
                        InvalidateAmount::Until(utils::max_instant(inst0, inst1))
                    }
                    (curr, _) => curr,
                });
        }

        self.invalidate_amount_changed.set(true);
    }
}

impl<'ui> Deref for LayoutContext<'ui> {
    type Target = imgui::Ui<'ui>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        if self.ui.is_item_edited() {
            self.request_invalidate(InvalidateAmount::Once);
        } else if self.ui.is_item_activated() {
            self.request_invalidate(InvalidateAmount::Indefinetely);
        } else if self.ui.is_item_deactivated() {
            self.request_invalidate(InvalidateAmount::Stop);
        }

        self.ui
    }
}

impl Drop for LayoutContext<'_> {
    fn drop(&mut self) {
        if self.invalidate_amount_changed.get() {
            let _ = self
                .window_handle
                .set_invalidate_amount(self.invalidate_amount.get());
        } else {
            if self.ui.is_any_item_active() {
                let _ = self.window_handle.request_invalidate();
            }
        }
    }
}

/// This struct represents an OS window, which contains an ImGUI graphical user interface.
pub struct Window {
    window: winit::window::Window,
    last_frame_time: std::time::Instant,
    alive: Arc<()>,
    app_handle: app::AppHandle,
    invalidate_amount: InvalidateAmount,

    // Everything for rendering
    surface: wgpu::Surface,
    gpu_device: wgpu::Device,
    swap_chain_desc: wgpu::SwapChainDescriptor,
    swap_chain: wgpu::SwapChain,
    renderer: imgui_wgpu::Renderer,
    queue: wgpu::Queue,

    // All imgui related
    winit_platform: imgui_winit_support::WinitPlatform,
    imgui: ImguiContext,
    default_font: Option<imgui::FontId>,
    last_cursor: Option<imgui::MouseCursor>,

    /// The data model associated with this native window, that holds its state.
    pub data_model: Box<dyn Layout>,
}

enum ImguiContext {
    Suspended(imgui::SuspendedContext),
    Used(),
}

impl Window {
    /// Closes the window and, if no further windows remain, shuts down the application.
    pub fn close(&mut self) {
        let data_model = &mut self.data_model as *mut Box<(dyn Layout + 'static)>;
        let should_close = unsafe { &mut *data_model }.before_close(self);

        if should_close {
            let window_id = self.id();
            let _ = self.app_handle.execute_with_gui(move |app: &mut App| {
                app.remove_window(window_id);
            });
        }
    }

    /// Get a mutable reference to the underlying `winit::window::Window`, which can be used to
    /// change size, position, etc.
    pub fn window_mut(&mut self) -> &mut winit::window::Window {
        &mut self.window
    }

    /// Get a reference to the underlying `winit::window::Window`, which can be used to
    /// change size, position, etc.
    pub fn window(&self) -> &winit::window::Window {
        &self.window
    }

    /// Get the id of the window.
    pub fn id(&self) -> winit::window::WindowId {
        self.window.id()
    }

    /// Get the time the window was last updated.
    pub fn last_frame_time(&self) -> std::time::Instant {
        self.last_frame_time
    }

    /// Creates a standard top level window.  
    ///
    /// Call this method inside the closure passed to `App::new_window()`.
    pub fn build_window(title: &str, size: (u32, u32)) -> winit::window::WindowBuilder {
        winit::window::WindowBuilder::new()
            .with_title(title)
            .with_inner_size(winit::dpi::PhysicalSize {
                width: size.0,
                height: size.1,
            })
            .with_resizable(true)
    }

    /// Creates a new NativeWindow instance.
    ///
    /// *Only for internal use.* The user creates new windows using `App::new_window()`.
    pub fn new(
        app: &mut app::App,
        data_model: Box<dyn Layout>,
        wnd: winit::window::Window,
    ) -> UiResult<Window> {
        let size = wnd.inner_size();
        let surface = wgpu::Surface::create(&wnd);

        // select adapter and gpu device
        let (device, queue) = Window::select_gpu_device(&app, &surface)?;

        // create the swapchain
        let format = wgpu::TextureFormat::Bgra8UnormSrgb;
        let swap_chain_desc = wgpu::SwapChainDescriptor {
            usage: wgpu::TextureUsage::OUTPUT_ATTACHMENT,
            format,
            width: size.width,
            height: size.height,
            present_mode: wgpu::PresentMode::Fifo,
        };
        let swap_chain = device.create_swap_chain(&surface, &swap_chain_desc);

        // create imgui ui
        // Note: This is going to panic if any other `imgui::Context` is currently active
        let mut imgui = imgui::Context::create_with_shared_font_atlas(app.font_atlas.clone());

        let mut platform = imgui_winit_support::WinitPlatform::init(&mut imgui);
        platform.attach_window(
            imgui.io_mut(),
            &wnd,
            imgui_winit_support::HiDpiMode::Default,
        );
        app.apply_imgui_settings(&mut imgui);

        // create renderer
        let renderer = imgui_wgpu::Renderer::new(
            &mut imgui,
            &device,
            &queue,
            &swap_chain_desc,
            Some(wgpu::Color::BLACK),
            app.msaa_samples,
        );

        Ok(Window {
            window: wnd,
            last_frame_time: std::time::Instant::now(),
            alive: Arc::default(),
            app_handle: app.handle(),
            invalidate_amount: InvalidateAmount::Stop,

            surface,
            gpu_device: device,
            swap_chain_desc,
            swap_chain,
            renderer,
            queue,

            winit_platform: platform,
            imgui: ImguiContext::Suspended(imgui.suspend()),
            default_font: None,
            last_cursor: None,

            data_model,
        })
    }

    pub fn invalidate_amount(&self) -> InvalidateAmount {
        self.invalidate_amount
    }

    pub fn set_invalidate_amount(&self, amount: InvalidateAmount) -> UiResult<()> {
        self.app_handle
            .send_event(super::AppEvent::SetWindowInvalidateAmount {
                window_id: self.id(),
                state: amount,
            })
    }

    fn update_render_size<T: std::convert::Into<u32>>(
        &mut self,
        _app: &App,
        size: winit::dpi::PhysicalSize<T>,
    ) {
        self.swap_chain_desc.width = size.width.into();
        self.swap_chain_desc.height = size.height.into();

        self.swap_chain = self
            .gpu_device
            .create_swap_chain(&self.surface, &self.swap_chain_desc);
    }

    pub(super) fn activate<'a>(&'a mut self) -> UiResult<ActiveWindow<'a>> {
        let imgui = std::mem::replace(&mut self.imgui, ImguiContext::Used());
        if let ImguiContext::Suspended(ctx) = imgui {
            let ctx = ctx.activate();
            match ctx {
                Ok(ctx) => {
                    return Ok(ActiveWindow {
                        imgui_context: std::mem::ManuallyDrop::new(ctx),
                        wrapped_window: self,
                    });
                }
                Err(ctx) => {
                    self.imgui = ImguiContext::Suspended(ctx);
                    return Err(UiError::new(ErrorCode::IMGUI_CONTEXT_ACTIVATE_FAILED));
                }
            }
        }

        Err(UiError::new(ErrorCode::INVALID_IMGUI_CONTEXT))
    }

    /// Creates a thread-safe handle to this native window.
    ///
    /// This handle can be used to access the represented native window from another
    /// thread using events that get sent to and dispatched in the main (UI) thread.
    pub fn handle(&self) -> WindowHandle {
        WindowHandle {
            window_id: self.id(),
            app_handle: self.app_handle.clone(),
            alive: Arc::downgrade(&self.alive),
        }
    }

    /// Request window invalidation as soon as possible.
    pub fn request_invalidate(&self) {
        self.window.request_redraw();
    }

    fn select_gpu_device(
        _app: &App,
        surface: &wgpu::Surface,
    ) -> UiResult<(wgpu::Device, wgpu::Queue)> {
        use futures::executor::block_on;

        let adapter_opts = wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::Default,
            compatible_surface: Some(&surface),
        };

        let adapter_request = wgpu::Adapter::request(&adapter_opts, wgpu::BackendBit::PRIMARY);
        let adapter: wgpu::Adapter = match block_on(adapter_request) {
            Some(val) => val,
            None => {
                log::debug!("Failed to get a PRIMARY graphics adapter, retrying with SECONDARY.");

                let adapter_request =
                    wgpu::Adapter::request(&adapter_opts, wgpu::BackendBit::SECONDARY);
                match block_on(adapter_request) {
                    Some(val) => val,
                    None => {
                        log::debug!("Failed to request graphics adapter.");
                        return Err(ErrorCode::GRAPHICS_ADAPTER_NOT_AVAILABLE.into());
                    }
                }
            }
        };

        let extensions = wgpu::Extensions {
            anisotropic_filtering: false,
        };
        let limits = wgpu::Limits::default();
        let device_desc = wgpu::DeviceDescriptor { extensions, limits };

        Ok(block_on(adapter.request_device(&device_desc)))
    }
}

/// A window prepared to be updated.
///
/// This struct is used to disjoin the lifetimes of the `Window` with that of the
/// `imgui::Context`.
pub struct ActiveWindow<'a> {
    /// The imgui context of the `window`.
    pub imgui_context: std::mem::ManuallyDrop<imgui::Context>,
    /// The original native window, where its `imgui` value has been replaced with
    /// `ImguiContext::Used()` and moved to `imgui_context`.
    pub wrapped_window: &'a mut Window,
}

impl<'a> Drop for ActiveWindow<'a> {
    /// Returns the `imgui::Context` back to the native window.
    fn drop(&mut self) {
        let val = std::mem::replace(&mut *self.imgui_context, unsafe {
            std::mem::MaybeUninit::uninit().assume_init()
        });
        self.wrapped_window.imgui = ImguiContext::Suspended(val.suspend());
    }
}

impl Deref for ActiveWindow<'_> {
    type Target = Window;

    fn deref(&self) -> &Self::Target {
        self.wrapped_window
    }
}
impl DerefMut for ActiveWindow<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.wrapped_window
    }
}

impl ActiveWindow<'_> {
    pub fn on_event(&mut self, app: &App, evt: &super::Event) {
        let ActiveWindow {
            wrapped_window: this,
            imgui_context: imgui,
        } = self;
        let this: &mut Window = this;
        let imgui: &mut imgui::Context = imgui;

        this.winit_platform
            .handle_event(imgui.io_mut(), &this.window, evt);

        match evt {
            super::Event::WindowEvent {
                window_id,
                event: ref wnd_evt,
            } if *window_id == this.id() => match wnd_evt {
                winit::event::WindowEvent::CloseRequested => {
                    self.close();
                }
                winit::event::WindowEvent::Resized(physical_size) => {
                    self.update_render_size(app, *physical_size);
                }
                _ => (),
            },
            super::Event::MainEventsCleared => {
                this.request_invalidate();
            }
            _ => (),
        }
    }

    pub fn render(&mut self, app: &App) -> UiResult<()> {
        let ActiveWindow {
            wrapped_window: this,
            imgui_context: imgui,
        } = self;
        let this: &mut Window = this;
        let imgui: &mut imgui::Context = imgui;

        let now = std::time::Instant::now();
        imgui.io_mut().update_delta_time(now - this.last_frame_time);
        this.last_frame_time = now;

        let framebuffer = match this.swap_chain.get_next_texture() {
            Ok(val) => val,
            Err(_) => return Err(UiError::new(ErrorCode::SWAP_CHAIN_TIMEOUT)),
        };

        this.winit_platform
            .prepare_frame(imgui.io_mut(), &this.window)
            .expect("Failed to prepare frame.");
        let ui = imgui.frame();
        {
            let font_handle = match this.default_font {
                Some(font) => Some(ui.push_font(font)),
                None => None,
            };

            let layout_ctx = LayoutContext {
                ui: &ui,
                window_handle: this.handle(),
                invalidate_amount_changed: Cell::new(false),
                invalidate_amount: Cell::new(this.invalidate_amount()),
            };

            let data_model = &mut this.data_model as *mut Box<(dyn Layout + 'static)>;
            unsafe {
                (*data_model).layout(layout_ctx, app, this);
            }

            if let Some(font_handle) = font_handle {
                font_handle.pop(&ui);
            }
        }
        if this.last_cursor != ui.mouse_cursor() {
            this.last_cursor = ui.mouse_cursor();
            this.winit_platform.prepare_render(&ui, &this.window);
        }
        let draw_data = ui.render();

        let cmd_encoder_desc = wgpu::CommandEncoderDescriptor {
            label: Some("imgui_command_encoder"),
        };
        let mut encoder = this.gpu_device.create_command_encoder(&cmd_encoder_desc);

        match this.renderer.render(
            draw_data,
            &this.gpu_device,
            &mut encoder,
            &framebuffer.view,
            &this.swap_chain_desc,
        ) {
            Err(err) => {
                return Err(UiError::with_source(
                    ErrorCode::RENDER_ERROR,
                    utils::MessageError::debug(&err),
                ))
            }
            Ok(_) => (),
        };

        this.queue.submit(&[encoder.finish()]);

        Ok(())
    }
}

/// A thread-safe handle to a NativeWindow.
///
/// This handle can be used to communicate with the NativeWindow from a different thread
/// through events. All methods on this handle will return an error when the window does
/// not exist anymore (can be queried with `alive()`).
pub struct WindowHandle {
    window_id: winit::window::WindowId,
    app_handle: app::AppHandle,
    alive: Weak<()>,
}

impl WindowHandle {
    /// Queries wether the represented main window still exists or not.
    pub fn alive(&self) -> bool {
        match self.alive.upgrade() {
            Some(_) => true,
            _ => false,
        }
    }

    /// Runs the closure `callback` in the UI thread.
    ///
    /// Returns an error if the NativeWindow that this handle referres to doesn't exist
    /// anymore.
    pub fn run(&self, callback: impl FnOnce(&mut app::App, &mut Window) + 'static) -> UiResult<()> {
        if let None = self.alive.upgrade() {
            return Err(ErrorCode::WINDOW_DOES_NOT_EXIST.into());
        }

        self.app_handle
            .send_event(app::AppEvent::ExecuteWithWindow {
                window_id: self.window_id,
                callback: app::ExecuteWithWindowCallback(Box::new(callback)),
            })
            .unwrap();
        Ok(())
    }

    /// Runs the closure callback in the UI thread and passes
    /// the data model of the NativeWindow downcast to T.  
    ///
    /// The main thread will panic if the data model of the NativeWindow
    /// cannot be downcast to T.  
    ///
    /// ## Note
    /// There is no guarantee that the passed closure will be run.
    /// If the NativeWindow gets destryed after this method has been called
    /// and before the main thread has gotten the event for running the closure,
    /// it will be skipped.
    pub fn run_with_data_model<T: Layout + Any>(
        &self,
        callback: impl FnOnce(&mut app::App, &mut T, WindowHandle) + 'static,
    ) -> UiResult<()> {
        if let None = self.alive.upgrade() {
            return Err(ErrorCode::WINDOW_DOES_NOT_EXIST.into());
        }

        self.app_handle
            .send_event(app::AppEvent::ExecuteWithWindow {
                window_id: self.window_id,
                callback: app::ExecuteWithWindowCallback(Box::new(move |app, wnd: &mut Window| {
                    let handle = wnd.handle();
                    let data_model = wnd.data_model.as_any().downcast_mut::<T>().unwrap();
                    callback(app, data_model, handle);
                })),
            })
            .unwrap();
        Ok(())
    }

    /// Request a redraw of the window.
    pub fn request_invalidate(&self) -> UiResult<()> {
        if let None = self.alive.upgrade() {
            return Err(ErrorCode::WINDOW_DOES_NOT_EXIST.into());
        }

        self.app_handle.send_event(app::AppEvent::InvalidateWindow {
            window_id: self.window_id,
        })
    }

    pub fn set_invalidate_amount(&self, amount: InvalidateAmount) -> UiResult<()> {
        self.app_handle
            .send_event(super::AppEvent::SetWindowInvalidateAmount {
                window_id: self.window_id,
                state: amount,
            })
    }

    /// Calls `NativeWindow::data_model.log(level, message)` from the UI thread. If the
    /// window does not exist anymore (it was already destroyed) and `level` is not
    /// `None`, logs the `message` with the given `level` instead.
    pub fn log(&self, level: Option<log::Level>, message: &str) {
        let message_copy = String::from(message);
        match self.run(move |_app, wnd| {
            wnd.data_model.log(level, &message_copy);
        }) {
            Ok(_) => (),
            Err(_) if level.is_some() => {
                log::log!(level.unwrap(), "{}", message);
            }
            Err(_) => (),
        };
    }

    /// Schedules the closure `callback` to approximately be executed at the given `instant`.
    ///
    /// Returnes an error if the window represented by this handle does not exist anymore.
    pub fn schedule(
        &self,
        instant: std::time::Instant,
        callback: app::ExecuteAtCallback,
    ) -> UiResult<()> {
        if let None = self.alive.upgrade() {
            return Err(ErrorCode::WINDOW_DOES_NOT_EXIST.into());
        }

        self.app_handle
            .send_event(app::AppEvent::ExecuteAt { instant, callback })
    }
}

impl Clone for WindowHandle {
    fn clone(&self) -> Self {
        WindowHandle {
            window_id: self.window_id,
            app_handle: self.app_handle.clone(),
            alive: self.alive.clone(),
        }
    }
}
