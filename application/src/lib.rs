pub mod commands;
pub mod config;
pub mod deserialize;
pub mod extensions;
pub mod io;
pub mod models;
pub mod remote;
pub mod response;
pub mod routes;
pub mod server;
pub mod ssh;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_COMMIT: &str = env!("CARGO_GIT_COMMIT");
pub const BUFFER_SIZE: usize = 32 * 1024;

#[inline]
pub fn is_valid_utf8_slice(s: &[u8]) -> bool {
    let mut idx = s.len();
    while idx > s.len().saturating_sub(4) {
        if str::from_utf8(&s[..idx]).is_ok() {
            return true;
        }

        idx -= 1;
    }

    false
}

#[inline(always)]
#[cold]
fn cold_path() {}

#[inline(always)]
pub fn likely(b: bool) -> bool {
    if b {
        true
    } else {
        cold_path();
        false
    }
}

#[inline(always)]
pub fn unlikely(b: bool) -> bool {
    if b {
        cold_path();
        true
    } else {
        false
    }
}
