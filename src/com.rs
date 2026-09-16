//! COM apartment lifetime and shell notification.

// Opt in to `unsafe` for this file only; the crate root denies it.
#![allow(unsafe_code)]

use windows_core::Result;

use crate::bindings::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};

/// RAII apartment guard.
///
/// Declare this *first* in whatever function owns COM work: locals drop in
/// reverse declaration order, so every interface pointer releases before
/// `CoUninitialize` runs. A `Release` after `CoUninitialize` is the top way to
/// crash on exit.
pub struct ComGuard {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ComGuard {
    pub fn new() -> Result<Self> {
        let flags = (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).cast_unsigned();
        // SAFETY: paired with CoUninitialize in Drop; single-threaded process.
        unsafe { CoInitializeEx(None, flags).ok()? };
        Ok(ComGuard {
            _not_send: std::marker::PhantomData,
        })
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: balances the CoInitializeEx in `new`.
        unsafe { CoUninitialize() };
    }
}

/// Repaint the desktop Recycle Bin icon so it shows as full.
///
/// `SHUpdateRecycleBinIcon` is not in the Win32 metadata, so resolve it by
/// name. shell32 is already mapped (this binary links it for
/// `SHCreateItemFromParsingName`), so `GetModuleHandleW` finds it without
/// taking a new reference. Purely cosmetic: failure is ignored.
pub fn update_recycle_bin_icon() {
    use windows_core::{s, w};

    use crate::bindings::{GetModuleHandleW, GetProcAddress};

    // SAFETY: the module handle is only used for a by-name lookup and the
    // function is only called when found. It takes no arguments.
    unsafe {
        let module = GetModuleHandleW(w!("shell32.dll"));
        if module.0.is_null() {
            return;
        }
        if let Some(proc) = GetProcAddress(module, s!("SHUpdateRecycleBinIcon")) {
            let update: extern "system" fn() -> i32 = std::mem::transmute(proc);
            update();
        }
    }
}
