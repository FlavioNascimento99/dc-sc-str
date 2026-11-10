#![allow(non_snake_case)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::core::{Interface, HSTRING};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::{IDirect3DDevice, IDirect3DSurface};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HMODULE, HWND, POINT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SAMPLER_DESC, D3D11_TEXTURE2D_DESC, D3D11_TEXTURE_ADDRESS_CLAMP,
    D3D11_SDK_VERSION, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11_VIEWPORT, ID3D11Device,
    ID3D11DeviceContext, ID3D11PixelShader, ID3D11RenderTargetView, ID3D11SamplerState,
    ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::RoGetActivationFactory;
use windows::core::GUID;

use crate::color::{self, HEIGHT, WIDTH};

const VS_SRC: &str = r#"
struct VsOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VsOut main(uint id : SV_VertexID) {
    float2 p = float2((id << 1) & 2, id & 2);
    VsOut o;
    o.pos = float4(p * float2(2.0, -2.0) + float2(-1.0, 1.0), 0, 1);
    o.uv = p;
    return o;
}
"#;

const PS_SRC: &str = r#"
Texture2D tex : register(t0);
SamplerState samp : register(s0);
struct VsOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
float4 main(VsOut i) : SV_Target { return tex.Sample(samp, i.uv); }
"#;

#[windows::core::interface("3628E81B-3CAC-4C60-B7F4-23CE0E0C3356")]
unsafe trait IGraphicsCaptureItemInterop: windows::core::IUnknown {
    unsafe fn CreateForWindow(
        &self,
        window: HWND,
        riid: *const GUID,
        result: *mut *mut core::ffi::c_void,
    ) -> windows::core::HRESULT;
    unsafe fn CreateForMonitor(
        &self,
        monitor: windows::Win32::Graphics::Gdi::HMONITOR,
        riid: *const GUID,
        result: *mut *mut core::ffi::c_void,
    ) -> windows::core::HRESULT;
}

struct Scaler {
    context: ID3D11DeviceContext,
    device: ID3D11Device,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    rt: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    staging: ID3D11Texture2D,
    scratch: Vec<u8>,
    source: Option<(u32, u32, ID3D11Texture2D)>,
}

impl Scaler {
    fn new(device: ID3D11Device, context: ID3D11DeviceContext) -> Result<Self> {
        let vs_blob = compile(VS_SRC, b"vs_5_0\0")?;
        let ps_blob = compile(PS_SRC, b"ps_5_0\0")?;
        let mut vs = None;
        let mut ps = None;
        let mut sampler = None;
        unsafe {
            device.CreateVertexShader(vs_blob.as_slice(), None, Some(&mut vs))?;
            device.CreatePixelShader(ps_blob.as_slice(), None, Some(&mut ps))?;
            device.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MaxLOD: f32::MAX,
                    ..Default::default()
                },
                Some(&mut sampler),
            )?;
        }
        let vs = vs.context("vertex shader")?;
        let ps = ps.context("pixel shader")?;
        let sampler = sampler.context("sampler")?;
        let rt = texture(
            &device,
            WIDTH,
            HEIGHT,
            D3D11_USAGE_DEFAULT,
            D3D11_BIND_RENDER_TARGET.0 as u32,
            0,
        )?;
        let mut rtv = None;
        unsafe { device.CreateRenderTargetView(&rt, None, Some(&mut rtv))? };
        let rtv = rtv.context("render target")?;
        let staging = texture(
            &device,
            WIDTH,
            HEIGHT,
            D3D11_USAGE_STAGING,
            0,
            D3D11_CPU_ACCESS_READ.0 as u32,
        )?;
        Ok(Self {
            context,
            device,
            vs,
            ps,
            sampler,
            rt,
            rtv,
            staging,
            scratch: vec![0u8; color::bgra_len()],
            source: None,
        })
    }

    fn scale_to_nv12(&mut self, frame: &ID3D11Texture2D) -> Result<Vec<u8>> {
        let desc = unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            frame.GetDesc(&mut desc);
            desc
        };
        if self
            .source
            .as_ref()
            .map(|(w, h, _)| *w != desc.Width || *h != desc.Height)
            .unwrap_or(true)
        {
            let copy = texture(
                &self.device,
                desc.Width,
                desc.Height,
                D3D11_USAGE_DEFAULT,
                D3D11_BIND_SHADER_RESOURCE.0 as u32,
                0,
            )?;
            self.source = Some((desc.Width, desc.Height, copy));
        }
        let source = &self.source.as_ref().unwrap().2;
        unsafe {
            self.context.CopyResource(source, frame);
            let mut srv = None;
            self.device
                .CreateShaderResourceView(source, None, Some(&mut srv))?;
            let srv = srv.context("srv")?;
            self.context.OMSetRenderTargets(Some(&[Some(self.rtv.clone())]), None);
            self.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: WIDTH as f32,
                Height: HEIGHT as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            self.context.VSSetShader(&self.vs, None);
            self.context.PSSetShader(&self.ps, None);
            self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
            self.context.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            self.context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.Draw(3, 0);
            self.context.CopyResource(&self.staging, &self.rt);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let pitch = mapped.RowPitch as usize;
            let src = mapped.pData as *const u8;
            for row in 0..HEIGHT as usize {
                let dst = row * WIDTH as usize * 4;
                std::ptr::copy_nonoverlapping(
                    src.add(row * pitch),
                    self.scratch[dst..].as_mut_ptr(),
                    WIDTH as usize * 4,
                );
            }
            self.context.Unmap(&self.staging, 0);
        }
        Ok(color::bgra_to_nv12(&self.scratch, WIDTH, HEIGHT, WIDTH * 4))
    }
}

fn texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE,
    bind: u32,
    cpu: u32,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: usage,
        BindFlags: bind,
        CPUAccessFlags: cpu,
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex))? };
    tex.context("textura D3D11")
}

fn compile(source: &str, target: &[u8]) -> Result<Vec<u8>> {
    use windows::Win32::Graphics::Direct3D::ID3DBlob;
    let mut blob: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let hr = unsafe {
        D3DCompile(
            source.as_ptr() as _,
            source.len(),
            None,
            None,
            None,
            windows::core::s!("main"),
            windows::core::PCSTR(target.as_ptr()),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut blob,
            Some(&mut errors),
        )
    };
    if let Err(err) = hr {
        let msg = errors
            .as_ref()
            .map(|e| unsafe {
                let p = e.GetBufferPointer() as *const u8;
                let n = e.GetBufferSize();
                String::from_utf8_lossy(std::slice::from_raw_parts(p, n)).into_owned()
            })
            .unwrap_or_default();
        return Err(err).context(msg);
    }
    let blob = blob.context("shader sem bytecode")?;
    let bytes = unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
    };
    Ok(bytes.to_vec())
}

pub struct Capture {
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    _session: GraphicsCaptureSession,
    _pool: Direct3D11CaptureFramePool,
    frames: Arc<Mutex<u64>>,
}

impl Capture {
    pub fn start() -> Result<Self> {
        let (device, context) = d3d_device()?;
        let winrt_device = winrt_device(&device)?;
        let item = primary_monitor_item()?;
        let size = item.Size()?;
        eprintln!(
            "captura: monitor {}x{} -> {WIDTH}x{HEIGHT}",
            size.Width, size.Height
        );
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsCursorCaptureEnabled(true);
        if session.SetIsBorderRequired(false).is_err() {
            eprintln!("captura: borda amarela do WGC não pode ser desligada neste Windows");
        }
        let latest = Arc::new(Mutex::new(None));
        let frames = Arc::new(Mutex::new(0u64));
        let scaler = Arc::new(Mutex::new(Scaler::new(device, context)?));
        let latest_cb = latest.clone();
        let frames_cb = frames.clone();
        let scaler_cb = scaler.clone();
        let last = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1)));
        let last_cb = last.clone();
        let handler = TypedEventHandler::<Direct3D11CaptureFramePool, windows::core::IInspectable>::new(
            move |pool, _| {
                let Some(pool) = pool.as_ref() else {
                    return Ok(());
                };
                let frame = pool.TryGetNextFrame()?;
                {
                    let mut last = last_cb.lock().unwrap_or_else(|e| e.into_inner());
                    if last.elapsed() < Duration::from_millis(33) {
                        return Ok(());
                    }
                    *last = Instant::now();
                }
                let surface: IDirect3DSurface = frame.Surface()?;
                let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
                let texture: ID3D11Texture2D = unsafe { access.GetInterface() }?;
                let nv12 = {
                    let mut scaler = scaler_cb.lock().unwrap_or_else(|e| e.into_inner());
                    match scaler.scale_to_nv12(&texture) {
                        Ok(buf) => buf,
                        Err(err) => {
                            eprintln!("captura: escala falhou: {err:#}");
                            return Ok(());
                        }
                    }
                };
                drop(frame);
                *latest_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(nv12);
                *frames_cb.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                Ok(())
            },
        );
        pool.FrameArrived(&handler)?;
        session.StartCapture()?;
        // The pool keeps the handler. Leak one Arc so the scaler outlives the callback.
        std::mem::forget(scaler);
        std::mem::forget(handler);
        Ok(Self {
            latest,
            _session: session,
            _pool: pool,
            frames,
        })
    }

    pub fn take(&self) -> Option<Vec<u8>> {
        self.latest.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn frames(&self) -> u64 {
        *self.frames.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn d3d_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut context = None;
    unsafe {
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
        )?
    };
    Ok((
        device.context("D3D11CreateDevice")?,
        context.context("device context")?,
    ))
}

fn winrt_device(device: &ID3D11Device) -> Result<IDirect3DDevice> {
    let dxgi: IDXGIDevice = device.cast()?;
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? };
    Ok(inspectable.cast()?)
}

fn primary_monitor_item() -> Result<GraphicsCaptureItem> {
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    if monitor.is_invalid() {
        anyhow::bail!("MonitorFromPoint não achou o monitor principal");
    }
    let class_name: HSTRING = "Windows.Graphics.Capture.GraphicsCaptureItem".into();
    let interop: IGraphicsCaptureItemInterop =
        unsafe { RoGetActivationFactory(&class_name)? };
    let mut raw = std::ptr::null_mut();
    unsafe {
        interop
            .CreateForMonitor(monitor, &GraphicsCaptureItem::IID, &mut raw)
            .ok()?
    };
    if raw.is_null() {
        anyhow::bail!("CreateForMonitor devolveu ponteiro nulo");
    }
    Ok(unsafe { GraphicsCaptureItem::from_raw(raw) })
}
