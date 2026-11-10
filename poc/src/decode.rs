use anyhow::{Context, Result};
use windows::core::{Interface, GUID};
use windows::Win32::Media::MediaFoundation::{
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFStartup, MFTEnumEx, IMF2DBuffer,
    IMFActivate, IMFMediaBuffer, IMFSample, IMFTransform, MFSTARTUP_NOSOCKET,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_E_TRANSFORM_TYPE_NOT_SET,
    MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_VERSION,
    MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_REGISTER_TYPE_INFO, MFMediaType_Video, MFVideoFormat_H264, MFVideoFormat_NV12,
};
use crate::color::{self, HEIGHT, WIDTH};
use crate::encode::{pack_u32, take_first_activate};

pub struct Decoder {
    transform: IMFTransform,
    started: bool,
}

impl Decoder {
    pub fn new() -> Result<Self> {
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)? };
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut activates,
                &mut count,
            )?
        };
        if count == 0 || activates.is_null() {
            anyhow::bail!("nenhum decodificador H.264");
        }
        let activate = unsafe { take_first_activate(activates, count) }.context("activate do decoder")?;
        let transform: IMFTransform = unsafe { activate.ActivateObject() }?;
        let in_type = media_type(MFVideoFormat_H264)?;
        unsafe {
            in_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u32(WIDTH, HEIGHT))?;
            in_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u32(1, 30))?;
            transform.SetInputType(0, &in_type, 0)?;
        }
        Ok(Self {
            transform,
            started: false,
        })
    }

    pub fn decode(&mut self, annex_b: &[u8]) -> Result<Option<Vec<u8>>> {
        if !self.started {
            unsafe {
                self.transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
                self.transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            }
            self.started = true;
        }
        let sample = memory_sample(annex_b)?;
        unsafe { self.transform.ProcessInput(0, &sample, 0)? };
        self.drain()
    }

    fn drain(&mut self) -> Result<Option<Vec<u8>>> {
        let mut latest = None;
        loop {
            let buffer =
                unsafe { MFCreateMemoryBuffer((color::nv12_len() * 2) as u32) }.context("buffer do decoder")?;
            let sample = unsafe { MFCreateSample() }.context("sample do decoder")?;
            unsafe { sample.AddBuffer(&buffer)? };
            let mut outputs = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(Some(sample)),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let result = unsafe { self.transform.ProcessOutput(0, &mut outputs, &mut status) };
            unsafe {
                std::mem::ManuallyDrop::drop(&mut outputs[0].pSample);
                std::mem::ManuallyDrop::drop(&mut outputs[0].pEvents);
            }
            match result {
                Ok(()) => {
                    if let Some(bgra) = read_nv12(&buffer)? {
                        latest = Some(bgra);
                    }
                }
                Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                Err(err)
                    if err.code() == MF_E_TRANSFORM_STREAM_CHANGE
                        || err.code() == MF_E_TRANSFORM_TYPE_NOT_SET =>
                {
                    self.set_output_type()?;
                }
                Err(err) => return Err(err).context("decoder ProcessOutput"),
            }
        }
        Ok(latest)
    }

    fn set_output_type(&self) -> Result<()> {
        let mt = unsafe { self.transform.GetOutputAvailableType(0, 0) }.context("tipo de saída do decoder")?;
        unsafe { self.transform.SetOutputType(0, &mt, 0)? };
        Ok(())
    }
}

fn media_type(subtype: GUID) -> Result<windows::Win32::Media::MediaFoundation::IMFMediaType> {
    let mt = unsafe { MFCreateMediaType() }.context("MFCreateMediaType")?;
    unsafe {
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        mt.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
    }
    Ok(mt)
}

fn memory_sample(data: &[u8]) -> Result<IMFSample> {
    let buffer = unsafe { MFCreateMemoryBuffer(data.len() as u32) }.context("buffer")?;
    let mut ptr = std::ptr::null_mut();
    let mut max = 0u32;
    let mut current = 0u32;
    unsafe {
        buffer.Lock(&mut ptr, Some(&mut max), Some(&mut current))?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        buffer.SetCurrentLength(data.len() as u32)?;
        buffer.Unlock()?;
    }
    let sample = unsafe { MFCreateSample() }.context("sample")?;
    unsafe { sample.AddBuffer(&buffer)? };
    Ok(sample)
}

fn read_nv12(buffer: &IMFMediaBuffer) -> Result<Option<Vec<u8>>> {
    if let Ok(buf2d) = buffer.cast::<IMF2DBuffer>() {
        let mut scan = std::ptr::null_mut();
        let mut pitch = 0i32;
        unsafe { buf2d.Lock2D(&mut scan, &mut pitch)? };
        let y_stride = pitch as u32;
        let uv_offset = y_stride as usize * HEIGHT as usize;
        let bytes = y_stride as usize * HEIGHT as usize * 3 / 2;
        let nv12 = unsafe { std::slice::from_raw_parts(scan, bytes) }.to_vec();
        unsafe { buf2d.Unlock2D()? };
        let _ = uv_offset;
        return Ok(Some(color::nv12_to_bgra(
            &nv12,
            WIDTH,
            HEIGHT,
            y_stride,
            y_stride,
        )));
    }
    let mut ptr = std::ptr::null_mut();
    let mut max = 0u32;
    let mut current = 0u32;
    unsafe { buffer.Lock(&mut ptr, Some(&mut max), Some(&mut current))? };
    if (current as usize) < color::nv12_len() {
        unsafe { buffer.Unlock()? };
        return Ok(None);
    }
    let nv12 = unsafe { std::slice::from_raw_parts(ptr, current as usize) }.to_vec();
    unsafe { buffer.Unlock()? };
    Ok(Some(color::nv12_to_bgra(
        &nv12,
        WIDTH,
        HEIGHT,
        WIDTH,
        WIDTH,
    )))
}
