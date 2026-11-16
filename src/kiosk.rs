use anyhow::Result;

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() }

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VAL_NAME: &str = "PhotoKiosk";

#[cfg(windows)]
pub fn prevent_sleep(on: bool) {
    use windows_sys::Win32::System::Power::*;
    unsafe {
        SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED }
                                else { ES_CONTINUOUS });
    }
}
#[cfg(not(windows))] pub fn prevent_sleep(_: bool) {}

#[cfg(windows)]
pub fn set_autostart(enable: bool) -> Result<()> {
    use windows_sys::Win32::System::Registry::*;
    let cmd = if enable {
        Some(wide(&format!("\"{}\" --kiosk", std::env::current_exe()?.display())))
    } else { None };

    unsafe {
        let path = wide(RUN_KEY);
        let mut h: HKEY = std::ptr::null_mut();
        let rc = RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_SET_VALUE, &mut h);
        if rc != 0 { anyhow::bail!("RegOpenKeyExW(Run) = {rc}"); }

        let name = wide(VAL_NAME);
        let rc = match &cmd {
            Some(c) => RegSetValueExW(h, name.as_ptr(), 0, REG_SZ,
                                      c.as_ptr() as *const u8, (c.len() * 2) as u32),
            None => RegDeleteValueW(h, name.as_ptr()),
        };
        RegCloseKey(h);
        // 2 = ERROR_FILE_NOT_FOUND: при выключении это норма (значения и не было)
        if rc != 0 && !(rc == 2 && !enable) { anyhow::bail!("запись реестра = {rc}"); }
    }
    Ok(())
}
#[cfg(not(windows))] pub fn set_autostart(_: bool) -> Result<()> { Ok(()) }

#[cfg(windows)]
pub fn autostart_enabled() -> bool {
    use windows_sys::Win32::System::Registry::*;
    unsafe {
        let path = wide(RUN_KEY);
        let mut h: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_QUERY_VALUE, &mut h) != 0 {
            return false;
        }
        let name = wide(VAL_NAME);
        let mut len: u32 = 0;
        let ok = RegQueryValueExW(h, name.as_ptr(), std::ptr::null(),
                                  std::ptr::null_mut(), std::ptr::null_mut(), &mut len) == 0;
        RegCloseKey(h);
        ok
    }
}
#[cfg(not(windows))] pub fn autostart_enabled() -> bool { false }

// ── Опциональный перехват клавиатуры: cargo build --features kbdhook ──────────
#[cfg(all(windows, feature = "kbdhook"))]
mod hook {
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    static ON: AtomicBool = AtomicBool::new(false);
    static HOOK: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

    unsafe extern "system" fn proc(code: i32, wp: usize, lp: isize) -> isize {
        unsafe {
            if code >= 0 && ON.load(Ordering::Relaxed) {
                let kb = &*(lp as *const KBDLLHOOKSTRUCT);
                let vk = kb.vkCode;
                let alt = kb.flags & LLKHF_ALTDOWN != 0;
                let ctrl = (GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0;
                let block = vk == VK_LWIN as u32 || vk == VK_RWIN as u32
                    || (alt && (vk == VK_TAB as u32 || vk == VK_ESCAPE as u32 || vk == VK_F4 as u32))
                    || (ctrl && vk == VK_ESCAPE as u32);
                if block { return 1; }
            }
            CallNextHookEx(null_mut(), code, wp, lp)
        }
    }

    pub fn set(on: bool) {
        ON.store(on, Ordering::Relaxed);
        unsafe {
            if on && HOOK.load(Ordering::Relaxed).is_null() {
                let h = SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), null_mut(), 0);
                HOOK.store(h, Ordering::Relaxed);
            } else if !on {
                let h = HOOK.swap(null_mut(), Ordering::Relaxed);
                if !h.is_null() { UnhookWindowsHookEx(h); }
            }
        }
    }
}
#[cfg(all(windows, feature = "kbdhook"))] pub use hook::set as set_keyboard_hook;
#[cfg(not(all(windows, feature = "kbdhook")))] pub fn set_keyboard_hook(_: bool) {}
