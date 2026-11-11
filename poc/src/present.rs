use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::Common::DXGI_ALPHA_MODE_IGNORE;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT, DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory2, IDXGISwapChain1,
};
use windows::Win32::Graphics::Gdi::{GetStockObject, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, IsWindow, LoadCursorW, PeekMessageW, RegisterClassExW, SetWindowTextW,
    ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW, MSG,
    PM_REMOVE, SW_SHOW, WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_QUIT, WNDCLASSEXW,
    WS_OVERLAPPEDWINDOW,
};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Foundation::HMODULE;

use crate::color::{self, HEIGHT, WIDTH};

pub struct Viewer {
    hwnd: HWND,
    context: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    received: Arc<Mutex<u64>>,
    presented: u64,
    started: Instant,
    last_title: Instant,
}

impl Viewer {
    pub fn open(latest: Arc<Mutex<Option<Vec<u8>>>>, received: Arc<Mutex<u64>>) -> Result<Self> {
        unsafe { Self::open_window(latest, received) }
    }

    pub fn tick(&mut self) -> Result<bool> {
        unsafe { self.draw() }
    }

    unsafe fn open_window(latest: Arc<Mutex<Option<Vec<u8>>>>, received: Arc<Mutex<u64>>) -> Result<Self> {
    let instance = GetModuleHandleW(None)?;
    let class_name = w!("PocViewWindow");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        hbrBackground: HBRUSH(GetStockObject(windows::Win32::Graphics::Gdi::BLACK_BRUSH).0),
        lpszClassName: class_name,
        ..Default::default()
    };
    if RegisterClassExW(&wc) == 0 {
        let err = windows::core::Error::from_win32();
        if err.code().0 != 1410 {
            return Err(err).context("RegisterClassExW");
        }
    }
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: WIDTH as i32,
        bottom: HEIGHT as i32,
    };
    AdjustWindowRect(&mut rect, WS_OVERLAPPEDWINDOW, false)?;
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class_name,
        w!("poc view"),
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        rect.right - rect.left,
        rect.bottom - rect.top,
        None,
        None,
        Some(instance.into()),
        None,
    )?;
    let _ = ShowWindow(hwnd, SW_SHOW);
    let (_device, context, swap) = swap_chain(hwnd)?;
    Ok(Self {
        hwnd,
        context,
        swap,
        latest,
        received,
        presented: 0,
        started: Instant::now(),
        last_title: Instant::now(),
    })
    }

    unsafe fn draw(&mut self) -> Result<bool> {
        if !IsWindow(self.hwnd).as_bool() {
            return Ok(false);
        }
        if let Some(bgra) = self.latest.lock().unwrap_or_else(|e| e.into_inner()).take() {
            if bgra.len() == color::bgra_len() {
                present(&self.context, &self.swap, &bgra)?;
                self.presented += 1;
            }
        }
        if self.last_title.elapsed() >= Duration::from_secs(1) {
            let got = *self.received.lock().unwrap_or_else(|e| e.into_inner());
            let title = format!(
                "discord-companion  recebidos {got}  desenhados {}  {:.0}s",
                self.presented,
                self.started.elapsed().as_secs_f32()
            );
            let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
            let _ = SetWindowTextW(self.hwnd, windows::core::PCWSTR(wide.as_ptr()));
            self.last_title = Instant::now();
        }
        Ok(true)
    }
}

pub fn run(latest: Arc<Mutex<Option<Vec<u8>>>>, received: Arc<Mutex<u64>>) -> Result<()> {
    unsafe {
        let mut viewer = Viewer::open_window(latest, received)?;
        let mut msg = MSG::default();
        loop {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return Ok(());
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if !viewer.draw()? {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(4));
        }
    }
}

unsafe fn swap_chain(hwnd: HWND) -> Result<(ID3D11Device, ID3D11DeviceContext, IDXGISwapChain1)> {
    let mut device = None;
    let mut context = None;
    D3D11CreateDevice(
        None,
        D3D_DRIVER_TYPE_HARDWARE,
        HMODULE::default(),
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        Some(&[D3D_FEATURE_LEVEL_11_0]),
        D3D11_SDK_VERSION,
        Some(&mut device),
        None,
        Some(&mut context),
    )?;
    let device = device.context("device da janela")?;
    let context = context.context("context da janela")?;
    let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
    let mut client = RECT::default();
    GetClientRect(hwnd, &mut client)?;
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: (client.right - client.left) as u32,
        Height: (client.bottom - client.top) as u32,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_NONE,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        ..Default::default()
    };
    let swap = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)?;
    Ok((device, context, swap))
}

unsafe fn present(context: &ID3D11DeviceContext, swap: &IDXGISwapChain1, bgra: &[u8]) -> Result<()> {
    let back: ID3D11Texture2D = swap.GetBuffer(0)?;
    context.UpdateSubresource(
        &back,
        0,
        None,
        bgra.as_ptr() as *const _,
        WIDTH * 4,
        0,
    );
    swap.Present(1, DXGI_PRESENT(0)).ok()?;
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
