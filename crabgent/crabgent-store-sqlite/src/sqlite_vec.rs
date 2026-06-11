//! sqlite-vec extension registration helpers.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::sync::OnceLock;

use sqlx::SqliteConnection;

static SQLITE_VEC_INIT: OnceLock<c_int> = OnceLock::new();

type SqliteExtensionInit = unsafe extern "C" fn(
    *mut libsqlite3_sys::sqlite3,
    *mut *mut c_char,
    *const libsqlite3_sys::sqlite3_api_routines,
) -> c_int;

pub fn register_sqlite_vec_once() -> Result<(), sqlx::Error> {
    let code = *SQLITE_VEC_INIT.get_or_init(|| {
        // SAFETY: sqlite3_auto_extension registers a process-global extension
        // callback. The function pointer is sqlite-vec's extension init.
        unsafe { libsqlite3_sys::sqlite3_auto_extension(Some(sqlite_vec_init())) }
    });
    sqlite_result(code, "sqlite-vec auto-extension registration")
}

pub async fn register_sqlite_vec_connection(
    conn: &mut SqliteConnection,
) -> Result<(), sqlx::Error> {
    let mut locked = conn.lock_handle().await?;
    let mut error_message: *mut c_char = ptr::null_mut();
    // SAFETY: the locked SQLx handle gives exclusive access to sqlite3*, and
    // sqlite-vec's init function matches the SQLite extension-init ABI.
    let code = unsafe {
        sqlite_vec_init()(
            locked.as_raw_handle().as_ptr(),
            &raw mut error_message,
            ptr::null(),
        )
    };
    if code == libsqlite3_sys::SQLITE_OK {
        return Ok(());
    }

    Err(sqlx::Error::Protocol(format!(
        "sqlite-vec connection registration failed with code {code}: {}",
        take_sqlite_error_message(error_message)
    )))
}

fn sqlite_result(code: c_int, context: &str) -> Result<(), sqlx::Error> {
    if code == libsqlite3_sys::SQLITE_OK {
        Ok(())
    } else {
        Err(sqlx::Error::Protocol(format!(
            "{context} failed with code {code}"
        )))
    }
}

fn sqlite_vec_init() -> SqliteExtensionInit {
    // SAFETY:
    // - The `sqlite-vec` crate declares `sqlite3_vec_init` with a placeholder
    //   Rust FFI signature, but the underlying C symbol is the canonical
    //   SQLite extension entry point with signature
    //     int sqlite3_vec_init(sqlite3 *db, char **pzErrMsg,
    //                          const sqlite3_api_routines *pApi);
    //   which matches the `SqliteExtensionInit` type alias above.
    // - Both function-pointer types use the C calling convention
    //   (`unsafe extern "C" fn`); the linker resolves the symbol to the
    //   same instruction address regardless of the Rust-side type, so the
    //   transmute reinterprets only the static type and leaves the runtime
    //   ABI untouched.
    // - The source pointer is non-null because it originates from a
    //   non-null extern symbol.
    unsafe { std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ()) }
}

fn take_sqlite_error_message(message: *mut c_char) -> String {
    if message.is_null() {
        return "no sqlite error message".to_owned();
    }

    // SAFETY: SQLite owns this null-terminated message.
    let text = unsafe { CStr::from_ptr(message).to_string_lossy().into_owned() };
    // SAFETY: SQLite requires sqlite3_free after reading the message.
    unsafe { libsqlite3_sys::sqlite3_free(message.cast()) };
    text
}

#[cfg(test)]
mod tests {
    #[test]
    fn sqlite_result_maps_ok_to_unit() {
        super::sqlite_result(libsqlite3_sys::SQLITE_OK, "ok-context")
            .expect("OK should map to Ok(())");
    }

    #[test]
    fn sqlite_result_maps_error_code_to_protocol_error() {
        let err = super::sqlite_result(libsqlite3_sys::SQLITE_ERROR, "ctx")
            .expect_err("non-OK should map to Err");
        assert!(matches!(err, sqlx::Error::Protocol(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("ctx"),
            "context should appear in message: {msg}"
        );
        assert!(
            msg.contains(&libsqlite3_sys::SQLITE_ERROR.to_string()),
            "error code should appear in message: {msg}"
        );
    }

    #[test]
    fn take_sqlite_error_message_handles_null_pointer() {
        let text = super::take_sqlite_error_message(std::ptr::null_mut());
        assert_eq!(text, "no sqlite error message");
    }
}
