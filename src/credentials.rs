use std::ffi::c_void;

use crate::diagnose;

const CRED_TYPE_GENERIC: u32 = 1;

#[repr(C)]
struct CredentialW {
    flags: u32,
    type_: u32,
    target_name: *mut u16,
    comment: *mut u16,
    last_written: u64,
    credential_blob_size: u32,
    credential_blob: *mut u8,
    persist: u32,
    attribute_count: u32,
    attributes: *mut c_void,
    target_alias: *mut u16,
    user_name: *mut u16,
}

#[link(name = "Advapi32")]
extern "system" {
    fn CredReadW(
        target_name: *const u16,
        type_: u32,
        reserved_flags: u32,
        credential: *mut *mut CredentialW,
    ) -> i32;
    fn CredFree(buffer: *mut c_void);
}

pub fn read_generic(target: &str) -> Option<String> {
    let target_wide = wide(target);
    let mut credential: *mut CredentialW = std::ptr::null_mut();
    let ok = unsafe { CredReadW(target_wide.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if ok == 0 || credential.is_null() {
        diagnose::log(format!(
            "unable to read Windows generic credential target {target}"
        ));
        return None;
    }

    let secret = unsafe { credential_secret(&*credential) };
    unsafe { CredFree(credential as *mut c_void) };
    secret
}

unsafe fn credential_secret(credential: &CredentialW) -> Option<String> {
    if credential.credential_blob_size == 0 || credential.credential_blob.is_null() {
        return None;
    }
    decode_secret(std::slice::from_raw_parts(
        credential.credential_blob,
        credential.credential_blob_size as usize,
    ))
}

fn decode_secret(bytes: &[u8]) -> Option<String> {
    if bytes.len() >= 2
        && bytes.len().is_multiple_of(2)
        && bytes.iter().skip(1).step_by(2).all(|byte| *byte == 0)
    {
        let units = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16(&units).ok();
    }
    String::from_utf8(bytes.to_vec()).ok()
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
