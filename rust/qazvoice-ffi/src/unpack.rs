//! Decompression of downloaded `.zst` model files, with SHA-256 of the output,
//! for the platform downloaders (Kotlin through JNI, Swift through the C ABI).
//! Runs on the downloader's background thread, never on the UI thread.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

/// Decompresses `src` (zstd) into `dst`, returning (bytes written, hex SHA-256).
/// `dst` is removed on error. `max_out` caps the output (catalog size).
pub fn unzstd_file(src: &Path, dst: &Path, max_out: u64) -> Result<(u64, String), String> {
    let run = || -> Result<(u64, String), String> {
        let input = BufReader::with_capacity(1 << 20, File::open(src).map_err(|e| format!("open: {e}"))?);
        let mut dec = ruzstd::decoding::StreamingDecoder::new(input).map_err(|e| format!("zstd: {e}"))?;
        let mut out = BufWriter::with_capacity(1 << 20, File::create(dst).map_err(|e| format!("create: {e}"))?);
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut total = 0u64;
        loop {
            let n = dec.read(&mut buf).map_err(|e| format!("zstd: {e}"))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > max_out {
                return Err(format!("output larger than {max_out} bytes"));
            }
            h.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(|e| format!("write: {e}"))?;
        }
        out.flush().map_err(|e| format!("write: {e}"))?;
        out.get_ref().sync_all().map_err(|e| format!("sync: {e}"))?;
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        Ok((total, hex))
    };
    let r = run();
    if r.is_err() {
        let _ = std::fs::remove_file(dst);
    }
    r
}

/// JNI entry for `kz.qazvoice.download.NativeUnpack.unzstd(src, dst, maxOut)`:
/// returns `"<size> <sha256>"` or `"error: …"`. Uses the JNI function table
/// directly (indices are fixed by the JNI spec), so no JNI crate is needed.
#[cfg(target_os = "android")]
#[no_mangle]
pub unsafe extern "system" fn Java_kz_qazvoice_download_NativeUnpack_unzstd(
    env: *mut *const *const std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    src: *mut std::ffi::c_void,
    dst: *mut std::ffi::c_void,
    max_out: i64,
) -> *mut std::ffi::c_void {
    use std::ffi::{c_char, c_void};
    type NewStringUtf = unsafe extern "system" fn(*mut *const *const c_void, *const c_char) -> *mut c_void;
    type GetStringUtfChars = unsafe extern "system" fn(*mut *const *const c_void, *mut c_void, *mut u8) -> *const c_char;
    type ReleaseStringUtfChars = unsafe extern "system" fn(*mut *const *const c_void, *mut c_void, *const c_char);
    let table = *env;
    let new_string: NewStringUtf = std::mem::transmute(*table.add(167));
    let get_chars: GetStringUtfChars = std::mem::transmute(*table.add(169));
    let release_chars: ReleaseStringUtfChars = std::mem::transmute(*table.add(170));
    let get = |s: *mut c_void| -> Option<String> {
        if s.is_null() {
            return None;
        }
        let p = get_chars(env, s, std::ptr::null_mut());
        if p.is_null() {
            return None;
        }
        let v = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
        release_chars(env, s, p);
        Some(v)
    };
    let msg = match (get(src), get(dst)) {
        (Some(a), Some(b)) => std::panic::catch_unwind(|| unzstd_file(Path::new(&a), Path::new(&b), max_out.max(0) as u64))
            .unwrap_or_else(|_| Err("panic".into()))
            .map(|(n, h)| format!("{n} {h}"))
            .unwrap_or_else(|e| format!("error: {e}")),
        _ => "error: bad arguments".into(),
    };
    let c = std::ffi::CString::new(msg).unwrap_or_default();
    new_string(env, c.as_ptr())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompresses_and_hashes() {
        let dir = std::env::temp_dir().join("qv_unzstd_test");
        std::fs::create_dir_all(&dir).unwrap();
        // Fixture made with the zstd CLI: `printf 'hello qazvoice\n' | zstd -19`.
        let src = dir.join("a.zst");
        std::fs::write(&src, include_bytes!("../tests/hello.txt.zst")).unwrap();
        let (n, h) = unzstd_file(&src, &dir.join("a"), 1 << 20).unwrap();
        assert_eq!(n, 15);
        assert_eq!(std::fs::read(dir.join("a")).unwrap(), b"hello qazvoice\n");
        assert_eq!(h, "47c601c3ca5143c8cf8bc0be886ca81b089c1b413d16deb751dbfc28e47078ec");
        // Too large → error and no output left behind.
        assert!(unzstd_file(&src, &dir.join("b"), 4).is_err());
        assert!(!dir.join("b").exists());
        // Corrupt input.
        std::fs::write(&src, b"not zstd").unwrap();
        assert!(unzstd_file(&src, &dir.join("c"), 1 << 20).is_err());
    }
}
