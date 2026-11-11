use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Console::{FreeConsole, GetConsoleWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    REG_VALUE_TYPE,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetCursorPos, GetMessageW, GetWindowTextW, LoadCursorW, MessageBoxW, PeekMessageW, RegisterClassExW,
    SetForegroundWindow, ShowWindow, TrackPopupMenu, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, HMENU, HWND_MESSAGE, IDC_ARROW, MB_OK, MF_CHECKED, MF_STRING, MSG, PM_REMOVE, SW_HIDE,
    TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COMMAND, WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP,
    WM_RBUTTONUP, WNDCLASSEXW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_SYSMENU, WS_VISIBLE,
};

use crate::present::Viewer;

const WM_TRAY: u32 = 0x8001;
const CMD_HOST: usize = 1;
const CMD_WATCH: usize = 2;
const CMD_AUTOSTART: usize = 3;
const CMD_QUIT: usize = 4;
const HOTKEY_ID: i32 = 1;
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("DiscordScreenCompanion");

enum Mode {
    Idle,
    Hosting(Arc<AtomicBool>, JoinHandle<()>),
    Watching(Arc<AtomicBool>, JoinHandle<()>, Viewer),
}

pub fn run(signal: String) -> Result<()> {
    crate::update::prompt(&signal);
    unsafe {
        let _ = FreeConsole();
        let _ = ShowWindow(GetConsoleWindow(), SW_HIDE);
        save_signal(&signal)?;
        message_loop(&signal)
    }
}

unsafe fn message_loop(signal: &str) -> Result<()> {
    let instance = GetModuleHandleW(None)?;
    let class_name = w!("DiscordCompanionTray");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        lpszClassName: class_name,
        ..Default::default()
    };
    if RegisterClassExW(&wc) == 0 {
        return Err(windows::core::Error::from_win32()).context("classe da bandeja");
    }
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class_name,
        w!("discord-companion"),
        WINDOW_STYLE::default(),
        0,
        0,
        0,
        0,
        Some(HWND_MESSAGE),
        None,
        Some(instance.into()),
        None,
    )?;
    let icon = icon();
    notify(hwnd, icon, NIM_ADD, "discord-companion")?;
    let _ = windows::Win32::UI::WindowsAndMessaging::RegisterHotKey(
        Some(hwnd),
        HOTKEY_ID,
        windows::Win32::UI::WindowsAndMessaging::MOD_CONTROL | windows::Win32::UI::WindowsAndMessaging::MOD_SHIFT,
        0x53,
    );
    let mut mode = Mode::Idle;
    let mut msg = MSG::default();
    loop {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if msg.message == WM_HOTKEY {
                show_room_code(toggle_host(&mut mode, signal), hwnd, icon);
            }
            if msg.message == WM_COMMAND {
                match (msg.wParam.0 & 0xffff) as usize {
                    CMD_HOST => show_room_code(toggle_host(&mut mode, signal), hwnd, icon),
                    CMD_WATCH => {
                        if let Some(code) = ask_code(instance.into()) {
                            start_watch(&mut mode, signal, code);
                        }
                    }
                    CMD_AUTOSTART => {
                        if let Err(err) = toggle_autostart(signal) {
                            eprintln!("autostart: {err:#}");
                        }
                    }
                    CMD_QUIT => {
                        stop(&mut mode);
                        let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd, icon, ""));
                        return Ok(());
                    }
                    _ => {}
                }
            }
            if msg.message == WM_TRAY && (msg.lParam.0 as u32 == WM_RBUTTONUP || msg.lParam.0 as u32 == WM_LBUTTONUP) {
                show_menu(hwnd, matches!(mode, Mode::Hosting(_, _)));
            }
        }
        if let Mode::Watching(_, _, viewer) = &mut mode {
            if !viewer.tick()? {
                stop(&mut mode);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
}

fn toggle_host(mode: &mut Mode, signal: &str) -> Option<String> {
    if matches!(mode, Mode::Hosting(_, _)) {
        stop(mode);
        return None;
    }
    stop(mode);
    let stop_flag = Arc::new(AtomicBool::new(false));
    let flag = stop_flag.clone();
    let base = signal.to_string();
    let slot = Arc::new(std::sync::Mutex::new(None));
    let code_out = slot.clone();
    let handle = std::thread::spawn(move || {
        if let Err(err) = crate::host(9876, false, Some(base), &flag, Some(code_out)) {
            eprintln!("host: {err:#}");
        }
    });
    for _ in 0..200 {
        if let Some(code) = slot.lock().unwrap_or_else(|err| err.into_inner()).clone() {
            *mode = Mode::Hosting(stop_flag, handle);
            return Some(code);
        }
        if handle.is_finished() {
            let _ = handle.join();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    *mode = Mode::Hosting(stop_flag, handle);
    None
}

unsafe fn show_room_code(code: Option<String>, hwnd: HWND, icon: windows::Win32::UI::WindowsAndMessaging::HICON) {
    let Some(code) = code else {
        return;
    };
    let _ = notify(hwnd, icon, NIM_MODIFY, &format!("sala {code}"));
    let text = wide(&format!("Código da sala: {code}\nQuem for assistir usa este código."));
    MessageBoxW(None, PCWSTR(text.as_ptr()), w!("discord-companion"), MB_OK);
}

fn start_watch(mode: &mut Mode, signal: &str, code: String) {
    stop(mode);
    let latest = std::sync::Arc::new(std::sync::Mutex::new(None));
    let received = std::sync::Arc::new(std::sync::Mutex::new(0u64));
    let frames = latest.clone();
    let count = received.clone();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let flag = stop_flag.clone();
    let base = signal.to_string();
    let handle = std::thread::spawn(move || {
        if let Err(err) = crate::view_net(9876, Some(base), Some(code), frames, count, &flag) {
            eprintln!("view: {err:#}");
        }
    });
    match Viewer::open(latest, received) {
        Ok(viewer) => *mode = Mode::Watching(stop_flag, handle, viewer),
        Err(err) => {
            stop_flag.store(true, Ordering::Relaxed);
            let _ = handle.join();
            eprintln!("janela: {err:#}");
        }
    }
}

fn stop(mode: &mut Mode) {
    match std::mem::replace(mode, Mode::Idle) {
        Mode::Hosting(flag, handle) => {
            flag.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
        Mode::Watching(flag, handle, _) => {
            flag.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
        Mode::Idle => {}
    }
}

unsafe fn show_menu(hwnd: HWND, hosting: bool) {
    let menu = CreatePopupMenu().unwrap_or_default();
    let host_label: Vec<u16> = if hosting {
        wide("Encerrar transmissão")
    } else {
        wide("Transmitir minha tela")
    };
    let watch = wide("Conectar a uma transmissão");
    let boot = wide("Iniciar com o Windows");
    let quit = wide("Sair");
    let _ = AppendMenuW(menu, MF_STRING, CMD_HOST, PCWSTR(host_label.as_ptr()));
    let _ = AppendMenuW(menu, MF_STRING, CMD_WATCH, PCWSTR(watch.as_ptr()));
    let mut flags = MF_STRING;
    if autostart_enabled() {
        flags |= MF_CHECKED;
    }
    let _ = AppendMenuW(menu, flags, CMD_AUTOSTART, PCWSTR(boot.as_ptr()));
    let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, PCWSTR(quit.as_ptr()));
    let mut point = POINT::default();
    let _ = GetCursorPos(&mut point);
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, point.x, point.y, None, hwnd, None);
    let _ = windows::Win32::UI::WindowsAndMessaging::DestroyMenu(menu);
}

unsafe fn ask_code(instance: windows::Win32::Foundation::HINSTANCE) -> Option<String> {
    let class_name = w!("DiscordCompanionCode");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(code_proc),
        hInstance: instance,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        lpszClassName: class_name,
        ..Default::default()
    };
    let _ = RegisterClassExW(&wc);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class_name,
        w!("Código da sala"),
        WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        320,
        140,
        None,
        None,
        Some(instance),
        None,
    ).ok()?;
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        w!("EDIT"),
        w!(""),
        WS_CHILD | WS_VISIBLE | WS_BORDER,
        16,
        16,
        270,
        24,
        Some(hwnd),
        Some(windows::Win32::UI::WindowsAndMessaging::HMENU(101isize as _)),
        Some(instance),
        None,
    );
    let ok = wide("Entrar");
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        w!("BUTTON"),
        PCWSTR(ok.as_ptr()),
        WS_CHILD | WS_VISIBLE,
        16,
        56,
        120,
        28,
        Some(hwnd),
        Some(HMENU(1isize as _)),
        Some(instance),
        None,
    );
    let mut msg = MSG::default();
    while hwnd_alive(hwnd) && GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    CODE.with(|slot| slot.take())
}

fn hwnd_alive(hwnd: HWND) -> bool {
    unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd).as_bool() }
}

thread_local! {
    static CODE: std::cell::Cell<Option<String>> = const { std::cell::Cell::new(None) };
}

unsafe extern "system" fn code_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_COMMAND && (wparam.0 & 0xffff) == 1 {
        let edit = windows::Win32::UI::WindowsAndMessaging::GetDlgItem(hwnd, 101);
        let mut buf = [0u16; 16];
        let n = GetWindowTextW(edit, &mut buf);
        let text = String::from_utf16_lossy(&buf[..n as usize]).trim().to_ascii_uppercase();
        if text.len() == 6 {
            CODE.with(|slot| slot.set(Some(text)));
            let _ = DestroyWindow(hwnd);
        }
        return LRESULT(0);
    }
    if msg == WM_DESTROY {
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe fn notify(hwnd: HWND, icon: windows::Win32::UI::WindowsAndMessaging::HICON, action: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE, tip: &str) -> Result<()> {
    Shell_NotifyIconW(action, &icon_data(hwnd, icon, tip)).context("Shell_NotifyIconW")?;
    Ok(())
}

unsafe fn icon_data(hwnd: HWND, icon: windows::Win32::UI::WindowsAndMessaging::HICON, tip: &str) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    let wide = wide(tip);
    let n = wide.len().min(data.szTip.len());
    data.szTip[..n].copy_from_slice(&wide[..n]);
    data
}

unsafe fn icon() -> windows::Win32::UI::WindowsAndMessaging::HICON {
    use windows::Win32::Graphics::Gdi::{
        CreateBitmap, CreateCompatibleBitmap, CreateIconIndirect, GetDC, ICONINFO,
    };
    let screen = GetDC(None);
    let color = CreateCompatibleBitmap(screen, 16, 16);
    let mask = CreateBitmap(16, 16, 1, 1, None);
    let info = ICONINFO {
        fIcon: true.into(),
        hbmColor: color,
        hbmMask: mask,
        ..Default::default()
    };
    CreateIconIndirect(&info).unwrap_or_default()
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn settings_path() -> Result<std::path::PathBuf> {
    let root = std::env::var("LOCALAPPDATA").context("LOCALAPPDATA")?;
    Ok(std::path::PathBuf::from(root).join("discord-screen-companion").join("settings.json"))
}

fn save_signal(signal: &str) -> Result<()> {
    let path = settings_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::json!({ "signal": signal }).to_string())?;
    Ok(())
}

unsafe fn toggle_autostart(signal: &str) -> Result<()> {
    if autostart_enabled() {
        let key = open_run(KEY_SET_VALUE)?;
        let _ = RegDeleteValueW(key, RUN_VALUE);
        let _ = RegCloseKey(key);
        return Ok(());
    }
    let exe = std::env::current_exe().context("executável")?;
    let command = format!("\"{}\" tray --signal {signal}", exe.display());
    let wide = wide(&command);
    let key = open_run(KEY_SET_VALUE)?;
    let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
    RegSetValueExW(key, RUN_VALUE, Some(0), REG_SZ, Some(bytes)).context("RegSetValueExW")?;
    let _ = RegCloseKey(key);
    Ok(())
}

unsafe fn autostart_enabled() -> bool {
    let Ok(key) = open_run(KEY_QUERY_VALUE) else {
        return false;
    };
    let mut kind = REG_VALUE_TYPE::default();
    let mut size = 0u32;
    let found = RegQueryValueExW(key, RUN_VALUE, None, Some(&mut kind), None, Some(&mut size)).is_ok();
    let _ = RegCloseKey(key);
    found
}

unsafe fn open_run(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<HKEY> {
    let mut key = HKEY::default();
    RegCreateKeyExW(
        HKEY_CURRENT_USER,
        RUN_KEY,
        Some(0),
        None,
        REG_OPTION_NON_VOLATILE,
        access,
        None,
        &mut key,
        None,
    )
    .context("Run")?;
    Ok(key)
}


