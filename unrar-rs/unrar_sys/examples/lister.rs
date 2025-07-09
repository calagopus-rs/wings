use libc::{c_int, c_uint};
use std::str;
use unrar_sys::*;

use std::ffi::{CStr, CString};
use std::fs;
use std::io::Write;

fn main() {
    let args = std::env::args();
    let mut stderr = std::io::stderr();
    let file = args.skip(1).next().unwrap_or_else(|| {
        writeln!(&mut stderr, "Please pass an archive as argument!").unwrap();
        std::process::exit(0)
    });
    extern "C" fn callback(msg: c_uint, user_data: LPARAM, p1: LPARAM, p2: LPARAM) -> c_int {
        match (msg, p2) {
            (UCM_CHANGEVOLUME, RAR_VOL_ASK) => {
                let ptr = p1 as *const _;
                let next = str::from_utf8(unsafe { CStr::from_ptr(ptr) }.to_bytes()).unwrap();
                let our_string = unsafe { &mut *(user_data as *mut String) };
                our_string.push_str(next);
                -1
            }
            (UCM_CHANGEVOLUME, RAR_VOL_NOTIFY) => 1,
            _ => 0,
        }
    }
    let file_cstr = CString::new(file).unwrap();
    let mut data = OpenArchiveData::new(file_cstr.as_ptr(), RAR_OM_LIST_INCSPLIT);
    let handle = unsafe { RAROpenArchive(&mut data as *mut _) };
    assert_eq!(data.open_result, 0);
    assert_eq!(handle.is_null(), false);
    let mut next_path = String::with_capacity(1024);
    unsafe {
        RARSetCallback(
            handle,
            Some(callback),
            &mut next_path as *mut String as LPARAM,
        )
    };
    let mut header = HeaderDataEx::default();
    let mut result = 0;
    let mut process_result;
    let mut first = true;
    while result == 0 {
        result = unsafe { RARReadHeaderEx(handle, &mut header as *mut _) };
        if result != ERAR_SUCCESS {
            if result != ERAR_END_ARCHIVE {
                writeln!(&mut stderr, "Error opening: {}", result).unwrap();
            }
            break;
        }
        if first && header.flags & RHDF_SPLITBEFORE != 0 {
            writeln!(&mut stderr, "Not beginning of archive! Still continuing").unwrap();
        }
        first = false;
        let s =
            str::from_utf8(unsafe { CStr::from_ptr(header.filename.as_ptr()) }.to_bytes()).unwrap();
        process_result =
            unsafe { RARProcessFile(handle, RAR_SKIP, std::ptr::null(), std::ptr::null()) };
        println!("{}", s);
        match process_result {
            ERAR_SUCCESS => (),
            ERAR_EOPEN => {
                if let Err(err) = fs::metadata(&next_path) {
                    writeln!(&mut stderr, "Couldn't find volume {}: {}", next_path, err).unwrap();
                    break;
                }
            }
            x => writeln!(&mut stderr, "Error: {}", x).unwrap(),
        }
    }
}
