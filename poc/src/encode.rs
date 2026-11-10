use anyhow::{Context, Result};
use windows::core::{Interface, GUID};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaBuffer, IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFTransform,
    METransformHaveOutput, METransformNeedInput, MFCreateAlignedMemoryBuffer, MFCreateMediaType,
    MFCreateSample, MFMediaType_Video, MFSTARTUP_NOSOCKET, MFStartup, MFVideoFormat_H264,
    MFVideoFormat_NV12, MFVideoInterlace_Progressive, MF_E_NO_EVENTS_AVAILABLE,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_FLAG_NO_WAIT,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC,
    MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION,
    MFTEnumEx, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_ENUM_FLAG_SYNCMFT, MFT_ENUM_FLAG_TRANSCODE_ONLY, MFT_FRIENDLY_NAME_Attribute,
    MFT_INPUT_STREAM_INFO, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_DATA_BUFFER_INCOMPLETE, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
};
use windows::Win32::Foundation::E_UNEXPECTED;
use windows::Win32::System::Com::CoTaskMemFree;

use crate::color::{self, HEIGHT, WIDTH};

static EVENT_LOG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub struct Encoder {
    transform: IMFTransform,
    events: Option<IMFMediaEventGenerator>,
    name: String,
    hardware: bool,
    sample_time: i64,
    force_keyframe: bool,
    alignment: u32,
    /// O MFT assíncrono já pediu entrada e ainda não recebeu ProcessInput.
    need_input: bool,
}

impl Encoder {
    pub fn new(allow_software: bool) -> Result<Self> {
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)? };
        let force_software = std::env::var_os("POC_SOFTWARE").is_some();
        let (activate, hardware) = pick_encoder(allow_software || force_software, force_software)?;
        let name = friendly_name(&activate);
        eprintln!(
            "encoder: {name} ({})",
            if hardware { "hardware" } else { "software" }
        );
        let transform: IMFTransform = unsafe { activate.ActivateObject() }?;
        let events = unlock_async(&transform)?;
        configure(&transform)?;
        let alignment = input_alignment(&transform);
        if let Ok(info) = unsafe { transform.GetOutputStreamInfo(0) } {
            eprintln!(
                "encoder: flags de saída {:#x} tamanho {} alinhamento {}",
                info.dwFlags, info.cbSize, info.cbAlignment
            );
        }
        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(Self {
            transform,
            events,
            name,
            hardware,
            sample_time: 0,
            force_keyframe: true,
            alignment,
            need_input: false,
        })
    }

    pub fn software() -> Result<Self> {
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)? };
        let (activate, hardware) = pick_encoder(true, true)?;
        let name = friendly_name(&activate);
        eprintln!(
            "encoder: {name} ({})",
            if hardware { "hardware" } else { "software" }
        );
        let transform: IMFTransform = unsafe { activate.ActivateObject() }?;
        let events = unlock_async(&transform)?;
        configure(&transform)?;
        let alignment = input_alignment(&transform);
        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(Self {
            transform,
            events,
            name,
            hardware,
            sample_time: 0,
            force_keyframe: true,
            alignment,
            need_input: false,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn hardware(&self) -> bool {
        self.hardware
    }

    pub fn force_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    pub fn encode(&mut self, nv12: &[u8]) -> Result<Option<Vec<u8>>> {
        if nv12.len() != color::nv12_len() {
            anyhow::bail!("NV12 com {} bytes, esperado {}", nv12.len(), color::nv12_len());
        }
        if self.force_keyframe {
            self.force_keyframe = false;
        }

        let sample = memory_sample(nv12, self.alignment)?;
        unsafe {
            sample.SetSampleTime(self.sample_time)?;
            sample.SetSampleDuration(10_000_000 / 30)?;
        }
        self.sample_time += 10_000_000 / 30;

        let events = self.events.clone();
        let mut output = if let Some(events) = events {
            self.encode_async(&events, &sample)?
        } else {
            unsafe { self.transform.ProcessInput(0, &sample, 0)? };
            drain_output(&self.transform)?
        };
        if output.is_empty() {
            return Ok(None);
        }
        let annex = color::to_annex_b(&output);
        output.clear();
        Ok(Some(annex))
    }

    fn encode_async(&mut self, events: &IMFMediaEventGenerator, sample: &IMFSample) -> Result<Vec<u8>> {
        let mut fed = false;
        let mut encoded = Vec::new();
        for _ in 0..16 {
            if self.need_input && !fed {
                unsafe { self.transform.ProcessInput(0, sample, 0)? };
                self.need_input = false;
                fed = true;
            }
            let flags = if fed {
                MF_EVENT_FLAG_NO_WAIT
            } else {
                MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)
            };
            let event = match unsafe { events.GetEvent(flags) } {
                Ok(event) => event,
                Err(err) if err.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                Err(err) => return Err(err).context("GetEvent do encoder"),
            };
            let kind = unsafe { event.GetType() }?;
            let n = EVENT_LOG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n < 12 {
                eprintln!("encoder: evento {kind}");
            }
            if kind == METransformNeedInput.0 as u32 {
                if !fed {
                    unsafe { self.transform.ProcessInput(0, sample, 0)? };
                    fed = true;
                } else {
                    // O encoder consumiu este quadro e quer o próximo antes de sair.
                    self.need_input = true;
                    break;
                }
            } else if kind == METransformHaveOutput.0 as u32 {
                encoded.extend(drain_output(&self.transform)?);
                if fed {
                    break;
                }
            }
        }
        if !fed {
            anyhow::bail!("encoder assíncrono não pediu entrada");
        }
        Ok(encoded)
    }
}

fn pick_encoder(allow_software: bool, force_software: bool) -> Result<(IMFActivate, bool)> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    if !force_software {
        if let Some(activate) = enum_mft(
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER | MFT_ENUM_FLAG_TRANSCODE_ONLY,
            &input,
            &output,
        )? {
            return Ok((activate, true));
        }
    }
    if !allow_software && !force_software {
        anyhow::bail!(
            "nenhum encoder H.264 de hardware. Rode de novo com --allow-software se estiver numa VM"
        );
    }
    let activate = enum_mft(
        MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
        &input,
        &output,
    )?
    .context("nenhum encoder H.264, nem de software")?;
    Ok((activate, false))
}

fn enum_mft(
    flags: windows::Win32::Media::MediaFoundation::MFT_ENUM_FLAG,
    input: &MFT_REGISTER_TYPE_INFO,
    output: &MFT_REGISTER_TYPE_INFO,
) -> Result<Option<IMFActivate>> {
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(input),
            Some(output),
            &mut activates,
            &mut count,
        )?
    };
    if count == 0 || activates.is_null() {
        return Ok(None);
    }
    let chosen = unsafe { take_first_activate(activates, count) };
    chosen.map(Some).context("MFT sem activate")
}

pub(crate) unsafe fn take_first_activate(
    activates: *mut Option<IMFActivate>,
    count: u32,
) -> Option<IMFActivate> {
    let items = std::slice::from_raw_parts_mut(activates, count as usize);
    let first = items[0].take();
    for extra in items.iter_mut().skip(1) {
        extra.take();
    }
    CoTaskMemFree(Some(activates.cast()));
    first
}

fn friendly_name(activate: &IMFActivate) -> String {
    let mut buf = [0u16; 256];
    let mut len = 0u32;
    if unsafe {
        activate.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, Some(&mut len))
    }
    .is_err()
    {
        return "H264".into();
    }
    let n = (len as usize).saturating_sub(1).min(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

fn configure(transform: &IMFTransform) -> Result<()> {
    let output = unsafe { transform.GetOutputAvailableType(0, 0) }
        .or_else(|_| media_type(MFVideoFormat_H264))?;
    unsafe {
        output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        output.SetUINT32(&MF_MT_AVG_BITRATE, 2_500_000)?;
        output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        output.SetUINT64(&MF_MT_FRAME_SIZE, pack_u32(WIDTH, HEIGHT))?;
        output.SetUINT64(&MF_MT_FRAME_RATE, pack_u32(1, 30))?;
        output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u32(1, 1))?;
        let _ = output.SetUINT32(&MF_MT_MPEG2_PROFILE, 77);
        if let Err(err) = transform.SetOutputType(0, &output, 0) {
            eprintln!("encoder: Main/level 4.0 recusado ({err}); tentando sem perfil");
            let plain = media_type(MFVideoFormat_H264)?;
            plain.SetUINT32(&MF_MT_AVG_BITRATE, 2_500_000)?;
            plain.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            plain.SetUINT64(&MF_MT_FRAME_SIZE, pack_u32(WIDTH, HEIGHT))?;
            plain.SetUINT64(&MF_MT_FRAME_RATE, pack_u32(1, 30))?;
            plain.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u32(1, 1))?;
            transform
                .SetOutputType(0, &plain, 0)
                .context("SetOutputType H264")?;
        }
    }
    let input = nv12_input_type(transform)?;
    unsafe {
        input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        input.SetUINT64(&MF_MT_FRAME_SIZE, pack_u32(WIDTH, HEIGHT))?;
        input.SetUINT64(&MF_MT_FRAME_RATE, pack_u32(1, 30))?;
        input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u32(1, 1))?;
        input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        transform
            .SetInputType(0, &input, 0)
            .context("SetInputType NV12")?;
    }
    Ok(())
}

fn nv12_input_type(transform: &IMFTransform) -> Result<IMFMediaType> {
    for index in 0..8 {
        let Ok(mt) = (unsafe { transform.GetInputAvailableType(0, index) }) else {
            break;
        };
        let subtype = unsafe { mt.GetGUID(&MF_MT_SUBTYPE) }.unwrap_or(GUID::zeroed());
        if subtype == MFVideoFormat_NV12 {
            return Ok(mt);
        }
    }
    media_type(MFVideoFormat_NV12)
}

fn input_alignment(transform: &IMFTransform) -> u32 {
    let mut info = MFT_INPUT_STREAM_INFO::default();
    if unsafe { transform.GetInputStreamInfo(0, &mut info) }.is_err() {
        return 16;
    }
    eprintln!(
        "encoder: entrada cbSize {} align {} lookahead {} flags {:#x}",
        info.cbSize, info.cbAlignment, info.cbMaxLookahead, info.dwFlags
    );
    let align = info.cbAlignment.max(16);
    if align.is_power_of_two() { align } else { 16 }
}

/// Empacota dois UINT32 no formato do Media Foundation: o de cima fica nos bits altos.
/// Frame size: largura embaixo, altura em cima. Frame rate: denominador embaixo, numerador em cima.
pub(crate) fn pack_u32(low: u32, high: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

fn media_type(subtype: GUID) -> Result<IMFMediaType> {
    let mt = unsafe { MFCreateMediaType() }.context("MFCreateMediaType")?;
    unsafe {
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        mt.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
    }
    Ok(mt)
}

fn memory_sample(data: &[u8], alignment: u32) -> Result<IMFSample> {
    let buffer = unsafe { MFCreateAlignedMemoryBuffer(data.len() as u32, alignment) }
        .context("MFCreateAlignedMemoryBuffer")?;
    copy_into_buffer(&buffer, data)?;
    let sample = unsafe { MFCreateSample() }.context("MFCreateSample")?;
    unsafe { sample.AddBuffer(&buffer)? };
    Ok(sample)
}

fn copy_into_buffer(buffer: &IMFMediaBuffer, data: &[u8]) -> Result<()> {
    let mut ptr = std::ptr::null_mut();
    let mut max = 0u32;
    let mut current = 0u32;
    unsafe {
        buffer.Lock(&mut ptr, Some(&mut max), Some(&mut current))?;
        if (max as usize) < data.len() {
            let _ = buffer.Unlock();
            anyhow::bail!("buffer MF menor que o quadro");
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        buffer.SetCurrentLength(data.len() as u32)?;
        buffer.Unlock()?;
    }
    Ok(())
}

fn allocated_output(cb_size: u32) -> Result<IMFSample> {
    let size = cb_size.max(2 << 20);
    let buffer = unsafe { MFCreateAlignedMemoryBuffer(size, 16) }.context("buffer de saída")?;
    let sample = unsafe { MFCreateSample() }.context("sample de saída")?;
    unsafe { sample.AddBuffer(&buffer)? };
    Ok(sample)
}

fn drain_output(transform: &IMFTransform) -> Result<Vec<u8>> {
    let info = unsafe { transform.GetOutputStreamInfo(0) }.unwrap_or_default();
    let provides = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
    let mut encoded = Vec::new();
    let mut force_alloc = false;
    loop {
        let supplied = if provides && !force_alloc {
            None
        } else {
            Some(allocated_output(info.cbSize)?)
        };
        let mut outputs = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(supplied),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        let result = unsafe { transform.ProcessOutput(0, &mut outputs, &mut status) };
        let sample = unsafe { std::mem::ManuallyDrop::take(&mut outputs[0].pSample) };
        unsafe { std::mem::ManuallyDrop::drop(&mut outputs[0].pEvents) };
        let incomplete =
            (outputs[0].dwStatus & (MFT_OUTPUT_DATA_BUFFER_INCOMPLETE.0 as u32)) != 0;
        match result {
            Ok(()) => {
                if let Some(sample) = sample {
                    encoded.extend(read_sample(&sample)?);
                }
                // Um MFT assíncrono entrega uma amostra por METransformHaveOutput.
                // Outro ProcessOutput sem evento novo devolve E_UNEXPECTED.
                if provides && !incomplete {
                    break;
                }
            }
            Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
            Err(err) if err.code() == E_UNEXPECTED && !encoded.is_empty() => break,
            Err(err) if err.code() == E_UNEXPECTED && provides && !force_alloc => {
                eprintln!("encoder: ProcessOutput recusou sample nulo ({err})");
                force_alloc = true;
            },
            Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                let mt = unsafe { transform.GetOutputAvailableType(0, 0) }.context("tipo de saída")?;
                unsafe { transform.SetOutputType(0, &mt, 0)? };
            }
            Err(err) => return Err(err).context("ProcessOutput"),
        }
    }
    Ok(encoded)
}

fn read_sample(sample: &IMFSample) -> Result<Vec<u8>> {
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.context("buffer do sample")?;
    let mut ptr = std::ptr::null_mut();
    let mut max = 0u32;
    let mut current = 0u32;
    unsafe {
        buffer.Lock(&mut ptr, Some(&mut max), Some(&mut current))?;
        let bytes = std::slice::from_raw_parts(ptr, current as usize).to_vec();
        buffer.Unlock()?;
        Ok(bytes)
    }
}

fn unlock_async(transform: &IMFTransform) -> Result<Option<IMFMediaEventGenerator>> {
    let attrs = unsafe { transform.GetAttributes() }.context("atributos do encoder")?;
    let async_mft = unsafe { attrs.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0;
    if !async_mft {
        return Ok(None);
    }
    unsafe { attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }.context("MF_TRANSFORM_ASYNC_UNLOCK")?;
    eprintln!("encoder: MFT assíncrono, destravado");
    Ok(Some(transform.cast()?))
}


