use anyhow::Result;

#[cfg(windows)]
pub fn prevent_sleep(on: bool) {
    use windows_sys::Win32::System::Power::*;
    unsafe {
        SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED }
                                else { ES_CONTINUOUS });
    }
}
#[cfg(not(windows))] pub fn prevent_sleep(_: bool) {}

fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() }

/// HKCU\...\Run → "C:\...\photokiosk.exe" --kiosk
#[cfg(windows)]
pub fn set_autostart(enable: bool) -> Result<()> {
    use windows_sys::Win32::System::Registry::*;
    unsafe {
        let path = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        let mut h: HKEY = std::ptr::null_mut();
        if RegCreateKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, std::ptr::null(), 0,
                           KEY_SET_VALUE, std::ptr::null(), &mut h, std::ptr::null_mut()) != 0 {
            anyhow::bail!("RegCreateKeyEx failed");
        }
        let name = wide("PhotoKiosk");
        if enable {
            let cmd = wide(&format!("\"{}\" --kiosk", std::env::current_exe()?.display()));
            RegSetValueExW(h, name.as_ptr(), 0, REG_SZ, cmd.as_ptr() as *const u8, (cmd.len() * 2) as u32);
        } else {
            RegDeleteValueW(h, name.as_ptr());
        }
        RegCloseKey(h);
    }
    Ok(())
}
#[cfg(not(windows))] pub fn set_autostart(_: bool) -> Result<()> { Ok(()) }

// ── Опционально: низкоуровневый хук клавиатуры (feature = "kbdhook") ──────────
// Блокирует Win, Alt+Tab, Alt+Esc, Alt+F4, Ctrl+Esc. НЕ блокирует Ctrl+Alt+Del.
// Ставить только на выделенной машине: включается из настроек, снимается паролем.
#[cfg(all(windows, feature = "kbdhook"))]
mod hook {
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    static ON: AtomicBool = AtomicBool::new(false);
    static mut HHOOK: isize = 0;

    unsafe extern "system" fn proc(code: i32, wp: usize, lp: isize) -> isize {
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
        CallNextHookEx(0, code, wp, lp)
    }
    pub fn set(on: bool) {
        unsafe {
            ON.store(on, Ordering::Relaxed);
            if on && HHOOK == 0 {
                // поток должен качать сообщения — это главный поток winit
                HHOOK = SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), std::ptr::null_mut(), 0);
            } else if !on && HHOOK != 0 {
                UnhookWindowsHookEx(HHOOK); HHOOK = 0;
            }
        }
    }
}
#[cfg(all(windows, feature = "kbdhook"))] pub use hook::set as set_keyboard_hook;
#[cfg(not(all(windows, feature = "kbdhook")))] pub fn set_keyboard_hook(_: bool) {}
