//! プラットフォーム固有のウィンドウ処理（アニメーション無効化など）

pub const OVERLAY_WINDOW_TITLE: &str = "Screen QR Reader Overlay";

#[cfg(windows)]
mod windows_impl {
    use super::OVERLAY_WINDOW_TITLE;
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, TRUE, WPARAM};
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThreadId, SetProcessWorkingSetSize,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, EnumThreadWindows, GetWindowTextW, SetWindowsHookExW, UnhookWindowsHookEx,
        CBT_CREATEWNDW, HHOOK, HCBT_ACTIVATE, HCBT_CREATEWND, WH_CBT, WS_CAPTION, WS_CHILD,
        WS_EX_TOOLWINDOW, WS_POPUP, WS_THICKFRAME, WS_VISIBLE,
    };

    static HOOK_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    static IS_CAPTURING: AtomicBool = AtomicBool::new(false);

    pub fn set_capturing_active(active: bool) {
        IS_CAPTURING.store(active, Ordering::SeqCst);
    }

    /// `CreateWindowExW` のタイトルポインタがオーバーレイウィンドウのものか判定
    unsafe fn is_overlay_title_ptr(ptr: *const u16) -> bool {
        if ptr.is_null() || (ptr as usize) < 0x10000 {
            return false;
        }
        let mut idx = 0usize;
        for expected in OVERLAY_WINDOW_TITLE.encode_utf16() {
            if *ptr.add(idx) != expected {
                return false;
            }
            idx += 1;
        }
        *ptr.add(idx) == 0
    }

    /// 既存ウィンドウがオーバーレイウィンドウかタイトルで判定
    unsafe fn is_overlay_window(hwnd: HWND) -> bool {
        let mut buf = [0u16; 64];
        let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if len <= 0 {
            return false;
        }
        let actual = &buf[..len as usize];
        let mut expected = OVERLAY_WINDOW_TITLE.encode_utf16();
        actual.iter().all(|&c| expected.next() == Some(c)) && expected.next().is_none()
    }

    /// ウィンドウのDWMトランジション（表示・非表示・拡大縮小等のアニメーション）を強制的に無効化する
    pub fn disable_window_transitions(hwnd: HWND) {
        unsafe {
            let disable: BOOL = TRUE;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED as u32,
                &disable as *const _ as *const c_void,
                size_of::<BOOL>() as u32,
            );
        }
    }

    /// CBTフックプロシージャ: オーバーレイウィンドウ生成時およびアクティブ化直前にアニメーションを無効化
    unsafe extern "system" fn cbt_hook_proc(
        n_code: i32,
        w_param: WPARAM,
        l_param: LPARAM,
    ) -> LRESULT {
        if IS_CAPTURING.load(Ordering::SeqCst) {
            if n_code == HCBT_CREATEWND as i32 {
                let create_struct = l_param as *mut CBT_CREATEWNDW;
                if !create_struct.is_null() && !(*create_struct).lpcs.is_null() {
                    let cs = &mut *(*create_struct).lpcs;
                    if (cs.style as u32 & WS_CHILD) == 0 && is_overlay_title_ptr(cs.lpszName) {
                        let mut style = cs.style as u32;
                        // 1. 最初は非表示で作成（位置・サイズ調整完了まで画面に出さない）
                        style &= !WS_VISIBLE;

                        // 2. WS_POPUP に設定し、枠線・キャプションを除去（OSのアプリ起動アニメーションを遮断）
                        style = (style & !(WS_CAPTION | WS_THICKFRAME)) | WS_POPUP;

                        cs.style = style as i32;

                        // 3. WS_EX_TOOLWINDOW を付加（タスクバー・アニメーションから除外）
                        cs.dwExStyle = (cs.dwExStyle as u32 | WS_EX_TOOLWINDOW) as _;
                    }
                }
            } else if n_code == HCBT_ACTIVATE as i32 {
                let hwnd = w_param as HWND;
                if is_overlay_window(hwnd) {
                    // 表示・アクティブ化直前にDWMトランジションを確実に無効化（HWNDが有効なタイミング）
                    disable_window_transitions(hwnd);
                }
            }
        }

        let hook = HOOK_HANDLE.load(Ordering::SeqCst) as HHOOK;
        CallNextHookEx(hook, n_code, w_param, l_param)
    }

    /// スレッドフックを設定
    pub fn init_window_animation_hook() {
        unsafe {
            let thread_id = GetCurrentThreadId();
            let hook = SetWindowsHookExW(
                WH_CBT,
                Some(cbt_hook_proc),
                0 as HMODULE,
                thread_id,
            );
            if !hook.is_null() {
                HOOK_HANDLE.store(hook as *mut c_void, Ordering::SeqCst);
            }
        }
    }

    /// スレッドフックの解除
    pub fn cleanup_window_animation_hook() {
        let hook = HOOK_HANDLE.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !hook.is_null() {
            unsafe {
                UnhookWindowsHookEx(hook as HHOOK);
            }
        }
    }

    /// 自スレッドのオーバーレイウィンドウのみDWMアニメーションを無効化
    pub fn disable_overlay_window_animations() {
        unsafe {
            unsafe extern "system" fn enum_proc(hwnd: HWND, _l_param: LPARAM) -> BOOL {
                if is_overlay_window(hwnd) {
                    disable_window_transitions(hwnd);
                }
                TRUE
            }

            EnumThreadWindows(GetCurrentThreadId(), Some(enum_proc), 0);
        }
    }

    /// 未使用の物理メモリ（ワーキングセット）をOSへ返却
    pub fn trim_memory() {
        unsafe {
            SetProcessWorkingSetSize(GetCurrentProcess(), !0, !0);
        }
    }
}

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(not(windows))]
pub fn set_capturing_active(_active: bool) {}

#[cfg(not(windows))]
pub fn init_window_animation_hook() {}

#[cfg(not(windows))]
pub fn cleanup_window_animation_hook() {}

#[cfg(not(windows))]
pub fn disable_overlay_window_animations() {}

#[cfg(not(windows))]
pub fn trim_memory() {}
