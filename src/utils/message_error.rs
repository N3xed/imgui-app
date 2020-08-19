#[derive(Debug)]
pub struct MessageError(pub String);

impl std::error::Error for MessageError {}

impl std::fmt::Display for MessageError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for MessageError {
    fn from(string: String) -> MessageError {
        MessageError(string)
    }
}
impl From<&str> for MessageError {
    fn from(string: &str) -> MessageError {
        MessageError(String::from(string))
    }
}

impl MessageError {
    /// Creates a message error from an arbitrary value that implements the `Debug` trait.
    /// The resulting message is the debug format `{:?}` of `val`.
    pub fn debug<T: std::fmt::Debug>(val: &T) -> MessageError {
        MessageError(format!("{:?}", val))
    }

    /// Creates a message error from an arbitrary value that implements the `Display` trait.
    /// The resulting message is the normal format `{}` of `val`.
    pub fn display<T: std::fmt::Display>(val: &T) -> MessageError {
        MessageError(format!("{}", val))
    }
}