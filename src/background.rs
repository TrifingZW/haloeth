//! 后台能力：托盘图标与「按住说话」全局快捷键。
//!
//! GPUI 自己拥有窗口消息循环，因此托盘与键盘钩子运行在独立线程上，
//! 该线程创建隐藏窗口并泵消息；事件通过通道回到界面线程轮询。
//!
//! 按住说话使用低级键盘钩子而不是 `RegisterHotKey`：后者只能收到按下，
//! 无法知道松开的时刻，而按住说话必须在松开时结束录音。

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// 后台事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundEvent {
    /// 显示主窗口。
    Show,
    /// 打开偏好设置。
    ShowPreferences,
    /// 切换到指定会话。
    OpenSession(String),
    /// 退出应用。
    Quit,
    /// 按住说话开始。
    TalkStart,
    /// 按住说话结束。
    TalkStop,
}

/// 托盘菜单展示的状态；界面线程写入，后台线程读取。
///
/// 会话列表不缓存：菜单展开时由后台线程直接扫描会话目录，
/// 避免每次保存会话都把所有会话文件读一遍。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayState {
    /// 当前会话标识符。
    pub current_id: String,
    /// 当前会话标题。
    pub current: String,
}

/// 解析后的快捷键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    /// 虚拟键码。
    pub key: u32,
    /// 是否要求 Ctrl。
    pub ctrl: bool,
    /// 是否要求 Alt。
    pub alt: bool,
    /// 是否要求 Shift。
    pub shift: bool,
}

impl Hotkey {
    /// 解析 `ctrl+alt+v` 这样的描述；无法识别的组合返回 [`None`]。
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_ascii_lowercase();
        if text.is_empty() {
            return None;
        }
        let mut hotkey = Hotkey {
            key: 0,
            ctrl: false,
            alt: false,
            shift: false,
        };
        for part in text.split('+').map(str::trim).filter(|part| !part.is_empty()) {
            match part {
                "ctrl" | "control" => hotkey.ctrl = true,
                "alt" => hotkey.alt = true,
                "shift" => hotkey.shift = true,
                "win" | "meta" | "platform" => {}
                other => {
                    let key = named_key(other)?;
                    hotkey.key = key;
                }
            }
        }
        (hotkey.key != 0).then_some(hotkey)
    }
}

/// 把符合 `ctrl+v` 写法的按键名转成虚拟键码。
fn named_key(name: &str) -> Option<u32> {
    let named = match name {
        "space" => 0x20,
        "tab" => 0x09,
        "enter" | "return" => 0x0D,
        "escape" | "esc" => 0x1B,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "home" => 0x24,
        "end" => 0x23,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        other => {
            if let Some(number) = other.strip_prefix('f')
                && let Ok(function) = number.parse::<u32>()
                && (1..=24).contains(&function)
            {
                return Some(0x70 + function - 1);
            }
            let mut chars = other.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            match ch {
                'a'..='z' => ch.to_ascii_uppercase() as u32,
                '0'..='9' => ch as u32,
                _ => return None,
            }
        }
    };
    Some(named)
}

/// 后台服务：持有事件接收端、快捷键更新端与托盘状态。
pub struct Background {
    events: Receiver<BackgroundEvent>,
    hotkey: Sender<Option<Hotkey>>,
    tray: Arc<Mutex<TrayState>>,
}

impl Background {
    /// 启动后台线程；`sessions_root` 是会话目录，用于在托盘菜单里列出会话。
    pub fn start(hotkey: Option<Hotkey>, sessions_root: std::path::PathBuf) -> Self {
        let (event_tx, events) = channel();
        let (hotkey_tx, hotkey_rx) = channel();
        let tray = Arc::new(Mutex::new(TrayState::default()));
        #[cfg(windows)]
        windows_impl::spawn(event_tx, hotkey_rx, hotkey, tray.clone(), sessions_root);
        #[cfg(not(windows))]
        {
            // 其它平台暂不提供托盘与全局快捷键：保留接收端，避免发送端移动两次。
            let _ = (event_tx, hotkey_rx, hotkey, &tray, sessions_root);
        }
        Self {
            events,
            hotkey: hotkey_tx,
            tray,
        }
    }

    /// 更新快捷键；`None` 表示关闭。
    pub fn set_hotkey(&self, hotkey: Option<Hotkey>) {
        let _ = self.hotkey.send(hotkey);
    }

    /// 更新托盘菜单展示的会话信息。
    pub fn set_tray(&self, state: TrayState) {
        if let Ok(mut tray) = self.tray.lock() {
            *tray = state;
        }
    }

    /// 取出一个待处理事件；没有事件时立即返回 [`None`]。
    pub fn next_event(&self) -> Option<BackgroundEvent> {
        self.events.try_recv().ok()
    }
}

// 窗口的显示 / 还原因平台负责：`Window::activate_window` 已经会在窗口最小化时
// 用 `ShowWindowAsync` 还原。这里不要自己调用同步的 `ShowWindow`——它会在界面
// 线程正在执行更新时同步派发 `WM_SIZE` / `WM_MOVE`，触发 GPUI 的借用冲突。

#[cfg(windows)]
#[allow(unsafe_op_in_unsafe_fn)]
mod windows_impl {
    use std::cell::RefCell;
    use std::sync::mpsc::{Receiver, Sender};
    use std::sync::{Arc, Mutex};

    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT,
    };
    use windows::Win32::UI::Shell::{
        NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CallNextHookEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DispatchMessageW, GetCursorPos, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
        IDI_APPLICATION, KBDLLHOOKSTRUCT, LoadIconW, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING,
        PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer,
        SetWindowsHookExW, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TrackPopupMenu,
        TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_APP, WM_CONTEXTMENU, WM_DESTROY,
        WM_KEYDOWN, WM_KEYUP, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
        WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
    };
    use windows::core::{HSTRING, PCWSTR, w};

    use super::{BackgroundEvent, Hotkey, TrayState};

    const WM_TRAY: u32 = WM_APP + 1;
    const ID_SHOW: usize = 1001;
    const ID_PREFERENCES: usize = 1003;
    const ID_QUIT: usize = 1004;
    /// 会话菜单项的起始命令号。
    const ID_SESSION: usize = 2000;

    /// 后台线程状态。
    struct State {
        events: Sender<BackgroundEvent>,
        hotkey_rx: Receiver<Option<Hotkey>>,
        hotkey: Option<Hotkey>,
        tray: Arc<Mutex<TrayState>>,
        sessions_root: std::path::PathBuf,
        talking: bool,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    /// 启动后台线程。
    pub fn spawn(
        events: Sender<BackgroundEvent>,
        hotkey_rx: Receiver<Option<Hotkey>>,
        hotkey: Option<Hotkey>,
        tray: Arc<Mutex<TrayState>>,
        sessions_root: std::path::PathBuf,
    ) {
        let _ = std::thread::Builder::new()
            .name("haloeth-background".to_owned())
            .spawn(move || unsafe { run(events, hotkey_rx, hotkey, tray, sessions_root) });
    }

    unsafe fn run(
        events: Sender<BackgroundEvent>,
        hotkey_rx: Receiver<Option<Hotkey>>,
        hotkey: Option<Hotkey>,
        tray: Arc<Mutex<TrayState>>,
        sessions_root: std::path::PathBuf,
    ) {
        STATE.with(|cell| {
            *cell.borrow_mut() = Some(State {
                events,
                hotkey_rx,
                hotkey,
                tray,
                sessions_root,
                talking: false,
            });
        });

        let instance = GetModuleHandleW(None).map(|module| HINSTANCE(module.0)).unwrap_or_default();
        let class = HSTRING::from("HaloethBackground");
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&window_class);
        let hwnd = match CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(class.as_ptr()),
            w!("Haloeth"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        ) {
            Ok(hwnd) => hwnd,
            Err(error) => {
                tracing::warn!("创建后台窗口失败：{error}");
                return;
            }
        };

        add_tray(hwnd, instance);

        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), None, 0).ok();
        // 快捷键设置通过通道从界面线程送来；没有消息时循环会一直阻塞在 GetMessage 上，
        // 因此靠一个 200 ms 的线程定时器让 `sync_hotkey` 定期执行。
        let _ = SetTimer(None, 0, 200, None);
        tracing::info!("后台线程就绪，键盘钩子：{}", hook.is_some());

        let mut message = Default::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
            sync_hotkey();
        }

        if let Some(hook) = hook {
            let _ = UnhookWindowsHookEx(hook);
        }
        remove_tray(hwnd);
    }

    /// 从界面线程同步快捷键设置。
    fn sync_hotkey() {
        STATE.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                while let Ok(next) = state.hotkey_rx.try_recv() {
                    state.hotkey = next;
                    tracing::info!("按住说话快捷键已更新：{next:?}");
                }
            }
        });
    }

    fn send(event: BackgroundEvent) {
        STATE.with(|cell| {
            if let Some(state) = cell.borrow().as_ref() {
                let _ = state.events.send(event);
            }
        });
    }

    /// 低级键盘钩子：检测按住说话。
    unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 {
            let event = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            let message = wparam.0 as u32;
            let pressed = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
            let released = message == WM_KEYUP || message == WM_SYSKEYUP;
            if pressed || released {
                let action = STATE.with(|cell| {
                    let mut guard = cell.borrow_mut();
                    let state = guard.as_mut()?;
                    let hotkey = state.hotkey?;
                    if event.vkCode != hotkey.key {
                        return None;
                    }
                    // 主窗口在前台时让快捷键交给输入框，避免 ctrl+v 既是粘贴又触发录音。
                    // 松开必须始终处理：按住说话期间窗口可能已经被带到前台。
                    if pressed && app_is_foreground() {
                        return None;
                    }
                    let matches = hotkey.ctrl == key_down(VK_CONTROL)
                        && hotkey.alt == key_down(VK_MENU)
                        && hotkey.shift == key_down(VK_SHIFT);
                    if pressed && !state.talking && matches {
                        state.talking = true;
                        return Some(BackgroundEvent::TalkStart);
                    }
                    if released && state.talking {
                        state.talking = false;
                        return Some(BackgroundEvent::TalkStop);
                    }
                    None
                });
                if let Some(action) = action {
                    send(action);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    /// 当前前台窗口是否属于本进程。
    fn app_is_foreground() -> bool {
        unsafe {
            let window = GetForegroundWindow();
            if window.0.is_null() {
                return false;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(window, Some(&mut pid));
            pid == GetCurrentProcessId()
        }
    }

    fn key_down(key: VIRTUAL_KEY) -> bool {
        unsafe { (GetAsyncKeyState(key.0 as i32) as u16 & 0x8000) != 0 }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_TRAY => {
                if matches!(lparam.0 as u32, WM_RBUTTONUP | WM_CONTEXTMENU) {
                    show_menu(hwnd);
                } else if lparam.0 as u32 == WM_LBUTTONUP {
                    send(BackgroundEvent::Show);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn add_tray(hwnd: HWND, instance: HINSTANCE) {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            ..Default::default()
        };
        // 优先使用可执行文件里的应用图标（app.rc 中的 ID 1）。
        let resource =
            LoadIconW(Some(instance), PCWSTR(std::ptr::without_provenance::<u16>(1))).ok();
        let fallback = LoadIconW(None, IDI_APPLICATION).ok();
        if let Some(icon) = resource.or(fallback) {
            data.hIcon = icon;
        }
        let tip: Vec<u16> = "Haloeth".encode_utf16().collect();
        let len = tip.len().min(data.szTip.len() - 1);
        data.szTip[..len].copy_from_slice(&tip[..len]);
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
    }

    unsafe fn remove_tray(hwnd: HWND) {
        let data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            ..Default::default()
        };
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }

    unsafe fn show_menu(hwnd: HWND) {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let state = STATE
            .with(|cell| {
                cell.borrow()
                    .as_ref()
                    .and_then(|state| state.tray.lock().ok().map(|tray| tray.clone()))
            })
            .unwrap_or_default();
        // 展开菜单时才读会话目录，避免每次保存会话都扫描一遍所有会话文件。
        let root = STATE
            .with(|cell| cell.borrow().as_ref().map(|state| state.sessions_root.clone()))
            .unwrap_or_default();
        let mut sessions: Vec<(String, String)> = crate::session_store::SessionStore::new(&root)
            .list()
            .into_iter()
            .map(|summary| (summary.id, summary.title))
            .collect();
        // 刚新建、尚未落盘的会话也要出现在菜单里。
        if !sessions.iter().any(|(id, _)| *id == state.current_id) {
            sessions.insert(0, (state.current_id.clone(), state.current.clone()));
        }

        let _ = AppendMenuW(menu, MF_STRING, ID_SHOW, w!("显示界面"));

        // 「当前对话」是一个子菜单：标题显示当前会话，展开后可以切换到任何会话。
        let sessions_menu = CreatePopupMenu().ok();
        if let Some(sessions_menu) = sessions_menu {
            for (index, (id, title)) in sessions.iter().enumerate().take(24) {
                let label = HSTRING::from(truncate(title, 40));
                let flags = if *id == state.current_id {
                    MF_STRING | MF_CHECKED
                } else {
                    MF_STRING
                };
                let _ =
                    AppendMenuW(sessions_menu, flags, ID_SESSION + index, PCWSTR(label.as_ptr()));
            }
            if sessions.len() > 24 {
                let _ = AppendMenuW(sessions_menu, MF_SEPARATOR, ID_SESSION + 24, PCWSTR::null());
                let _ =
                    AppendMenuW(sessions_menu, MF_STRING, ID_SHOW, w!("更多会话请在界面里查看"));
            }
            let label = HSTRING::from(format!("当前对话：{}", truncate(&state.current, 32)));
            let _ = AppendMenuW(menu, MF_POPUP, sessions_menu.0 as usize, PCWSTR(label.as_ptr()));
        }

        let _ = AppendMenuW(menu, MF_STRING, ID_PREFERENCES, w!("偏好设置"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("退出"));

        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        // 没有 `TPM_RETURNCMD` 时菜单只发 WM_COMMAND，而隐藏窗口没有处理它，
        // 于是所有菜单项都不会有任何反应。
        let command = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD,
            point.x,
            point.y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        // 菜单关闭后把前台还给系统，否则菜单会一直停留在屏幕上。
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));

        let command = command.0 as usize;
        match command {
            ID_SHOW => send(BackgroundEvent::Show),
            ID_PREFERENCES => send(BackgroundEvent::ShowPreferences),
            ID_QUIT => send(BackgroundEvent::Quit),
            command if command >= ID_SESSION => {
                if let Some((id, _)) = sessions.get(command - ID_SESSION) {
                    send(BackgroundEvent::OpenSession(id.clone()));
                }
            }
            _ => {}
        }
    }

    /// 截断菜单文字，避免超长标题把菜单撑到屏幕外。
    fn truncate(text: &str, max_chars: usize) -> String {
        let mut output: String = text.chars().take(max_chars).collect();
        if text.chars().count() > max_chars {
            output.push('…');
        }
        if output.trim().is_empty() {
            "未命名会话".to_owned()
        } else {
            output
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Hotkey;

    #[test]
    fn parses_modifier_combinations() {
        let hotkey = Hotkey::parse("ctrl+v").expect("ctrl+v");
        assert_eq!(hotkey.key, 'V' as u32);
        assert!(hotkey.ctrl);
        assert!(!hotkey.alt);

        let hotkey = Hotkey::parse("Ctrl+Alt+Space").expect("space");
        assert_eq!(hotkey.key, 0x20);
        assert!(hotkey.alt);

        assert!(Hotkey::parse("").is_none());
        assert!(Hotkey::parse("ctrl+").is_none());
        assert!(Hotkey::parse("ctrl+f25").is_none());

        // 功能键与命名键也能作为快捷键。
        assert_eq!(Hotkey::parse("f5").expect("f5").key, 0x74);
        assert_eq!(Hotkey::parse("alt+space").expect("space").key, 0x20);
        assert_eq!(Hotkey::parse("ctrl+shift+enter").expect("enter").key, 0x0D);
        assert!(Hotkey::parse("ctrl+??").is_none());
    }
}
