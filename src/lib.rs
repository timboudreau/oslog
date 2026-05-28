mod sys;

#[cfg(feature = "logger")]
mod logger;

#[cfg(feature = "logger")]
pub use logger::OsLogger;

use crate::sys::*;
use std::{ffi::{CString, c_void}, sync::{Mutex, atomic::AtomicUsize}};

#[cfg(not(feature = "async"))]
#[inline]
fn to_cstr(message: &str) -> CString {
    to_owned_cstr(message)
}

fn to_owned_cstr(message: &str) -> CString {
    let fixed = message.replace('\0', "(null)");
    CString::new(fixed).unwrap()
}

#[cfg(feature = "async")]
#[inline]
fn to_cstr(message: &str) -> &'static CString {
    let mut guard = HOLDER.try_lock().unwrap();
    guard.add(message.replace('\0', "(null)"))
}

#[cfg(feature = "async")]
static HOLDER : Mutex<StringsHolder<50>> = Mutex::new(StringsHolder::new());

/// A hideous hack to keep log messages allocated long enough for Apple's caulk async log
/// (the default in audio unit system extensions) to (hopefully) finish with them before
/// they are deallocated.
///
/// Simply keeps a ring buffer of `N` log messages and reaps the oldest when a new one is
/// added.  Not for any sort of production use, but allows this crate to be used without
/// causing constant access violations.
#[cfg(feature = "async")]
struct StringsHolder<const N: usize> {
    cursor : AtomicUsize,
    entries : [usize; N],
}

#[cfg(feature = "async")]
impl<const N : usize> StringsHolder<N> {
    const fn new() -> Self {
        Self {
            cursor: AtomicUsize::new(0),
            entries: [0; N],
        }
    }

    fn add(&mut self, msg : String) -> &'static CString {
        let cs = CString::new(msg).unwrap();
        let boxed: &'static mut CString = Box::leak(Box::new(cs));
        let mut address = boxed.as_ptr() as usize;
        let n = self.cursor.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        std::mem::swap(&mut address, &mut self.entries[n % N]);
        if address != 0 {
            unsafe { std::ptr::drop_in_place(address as *mut CString) };
        }
        boxed
    }
}

#[repr(u8)]
pub enum Level {
    Debug = OS_LOG_TYPE_DEBUG,
    Info = OS_LOG_TYPE_INFO,
    Default = OS_LOG_TYPE_DEFAULT,
    Error = OS_LOG_TYPE_ERROR,
    Fault = OS_LOG_TYPE_FAULT,
}

#[cfg(feature = "logger")]
impl From<log::Level> for Level {
    fn from(other: log::Level) -> Self {
        match other {
            log::Level::Trace => Self::Debug,
            log::Level::Debug => Self::Info,
            log::Level::Info => Self::Default,
            log::Level::Warn => Self::Error,
            log::Level::Error => Self::Fault,
        }
    }
}

#[derive(Clone)]
pub struct OsLog {
    inner: os_log_t,
    /// These need to remain allocated or system logging code can use
    /// them after they are freed.
    #[allow(dead_code)]
    subsystem: Option<CString>,
    #[allow(dead_code)]
    category: Option<CString>,
}

unsafe impl Send for OsLog {}
unsafe impl Sync for OsLog {}

impl Drop for OsLog {
    fn drop(&mut self) {
        unsafe {
            if self.inner != wrapped_get_default_log() {
                os_release(self.inner as *mut c_void);
            }
        }
    }
}

impl OsLog {
    #[inline]
    pub fn new(subsystem: &str, category: &str) -> Self {
        let subsystem = to_owned_cstr(subsystem);
        let category = to_owned_cstr(category);

        let inner = unsafe { os_log_create(subsystem.as_ptr(), category.as_ptr()) };

        assert!(!inner.is_null(), "Unexpected null value from os_log_create");

        Self {
            inner,
            subsystem: Some(subsystem),
            category: Some(category),
        }
    }

    #[inline]
    pub fn global() -> Self {
        let inner = unsafe { wrapped_get_default_log() };

        assert!(!inner.is_null(), "Unexpected null value for OS_DEFAULT_LOG");

        Self {
            inner,
            subsystem: None,
            category: None,
        }
    }

    #[inline]
    pub fn with_level(&self, level: Level, message: &str) {
        let message = to_cstr(message);
        unsafe { wrapped_os_log_with_type(self.inner, level as u8, message.as_ptr()) }
    }

    #[inline]
    pub fn debug(&self, message: &str) {
        let message = to_cstr(message);
        unsafe { wrapped_os_log_debug(self.inner, message.as_ptr()) }
    }

    #[inline]
    pub fn info(&self, message: &str) {
        let message = to_cstr(message);
        unsafe { wrapped_os_log_info(self.inner, message.as_ptr()) }
    }

    #[inline]
    pub fn default(&self, message: &str) {
        let message = to_cstr(message);
        unsafe { wrapped_os_log_default(self.inner, message.as_ptr()) }
    }

    #[inline]
    pub fn error(&self, message: &str) {
        let message = to_cstr(message);
        unsafe { wrapped_os_log_error(self.inner, message.as_ptr()) }
    }

    #[inline]
    pub fn fault(&self, message: &str) {
        let message = to_cstr(message);
        unsafe { wrapped_os_log_fault(self.inner, message.as_ptr()) }
    }

    #[inline]
    pub fn level_is_enabled(&self, level: Level) -> bool {
        unsafe { os_log_type_enabled(self.inner, level as u8) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subsystem_interior_null() {
        let log = OsLog::new("com.example.oslog\0test", "category");
        log.with_level(Level::Debug, "Hi");
    }

    #[test]
    fn test_category_interior_null() {
        let log = OsLog::new("com.example.oslog", "category\0test");
        log.with_level(Level::Debug, "Hi");
    }

    #[test]
    fn test_message_interior_null() {
        let log = OsLog::new("com.example.oslog", "category");
        log.with_level(Level::Debug, "Hi\0test");
    }

    #[test]
    fn test_message_emoji() {
        let log = OsLog::new("com.example.oslog", "category");
        log.with_level(Level::Debug, "\u{1F601}");
    }

    #[test]
    fn test_global_log_with_level() {
        let log = OsLog::global();
        log.with_level(Level::Debug, "Debug");
        log.with_level(Level::Info, "Info");
        log.with_level(Level::Default, "Default");
        log.with_level(Level::Error, "Error");
        log.with_level(Level::Fault, "Fault");
    }

    #[test]
    fn test_global_log() {
        let log = OsLog::global();
        log.debug("Debug");
        log.info("Info");
        log.default("Default");
        log.error("Error");
        log.fault("Fault");
    }

    #[test]
    fn test_custom_log_with_level() {
        let log = OsLog::new("com.example.oslog", "testing");
        log.with_level(Level::Debug, "Debug");
        log.with_level(Level::Info, "Info");
        log.with_level(Level::Default, "Default");
        log.with_level(Level::Error, "Error");
        log.with_level(Level::Fault, "Fault");
    }

    #[test]
    fn test_custom_log() {
        let log = OsLog::new("com.example.oslog", "testing");
        log.debug("Debug");
        log.info("Info");
        log.default("Default");
        log.error("Error");
        log.fault("Fault");
    }
}
