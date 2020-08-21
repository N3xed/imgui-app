//! Error handling.

use std::error::Error;

/// The error code that represents the actual error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
#[allow(non_camel_case_types)]
pub enum ErrorCode {
    /// The underlying window does not exist, it was either already closed and destroyed
    /// or it never existed in the first place.
    WINDOW_DOES_NOT_EXIST = 0,
    /// When trying to send an event to the event loop which already shut down.
    EVENT_LOOP_CLOSED,
    /// No graphics adapter available from `wgpu`.
    GRAPHICS_ADAPTER_NOT_AVAILABLE,
    REQUEST_GRAPHICS_DEVICE_FAILED,
    /// Failed to load the font.
    FONT_LOAD_FAILED,
    /// Timeout when requesting the next swap chain texture with `wgpu::SwapChain::get_next_texture()`.
    SWAP_CHAIN_TIMEOUT,
    /// `ActiveWindow::render()` failed.
    RENDER_ERROR,
    IMGUI_CONTEXT_ACTIVATE_FAILED,
    INVALID_IMGUI_CONTEXT,
    WINDOW_BUILD_FAILED,
    FILE_ERROR,
}

/// A generic error struct for `App` and `Window` related errors.
///
/// All `UiError`s have an associated `ErrorCode` that specifies the exact error.
#[derive(Debug)]
pub struct UiError {
    error_code: ErrorCode,
    source_error: Option<Box<dyn Error>>,
}

impl std::fmt::Display for UiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.error_code)?;
        if let Some(ref err) = self.source_error {
            write!(f, " caused by {}", err)?;
        }
        Ok(())
    }
}

impl Error for UiError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source_error.as_ref().map(|s| &**s)
    }
}

impl UiError {
    /// Create a new `UiError` with the provided error code.
    pub fn new(error_code: ErrorCode) -> UiError {
        UiError {
            error_code: error_code,
            source_error: None,
        }
    }

    /// Create a new `UiError` with the provided error code and source error.
    pub fn with_source<T: Error + 'static>(error_code: ErrorCode, source: T) -> UiError {
        UiError {
            error_code: error_code,
            source_error: Some(Box::new(source)),
        }
    }

    /// Create a new `UiError` with the provided error code and boxed source error.
    pub fn with_boxed_source(error_code: ErrorCode, source: Box<dyn Error>) -> UiError {
        UiError {
            error_code: error_code,
            source_error: Some(source),
        }
    }
}

impl From<ErrorCode> for UiError {
    fn from(error_code: ErrorCode) -> UiError {
        UiError::new(error_code)
    }
}

impl From<std::io::Error> for UiError {
    fn from(err: std::io::Error) -> UiError {
        UiError::with_source(ErrorCode::FILE_ERROR, err)
    }
}

pub type UiResult<T> = Result<T, UiError>;
