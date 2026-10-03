//! Hardware H.264 encoding through a Media Foundation transform (NVENC, Quick Sync and AMF
//! all register one). Frames are converted to NV12 on the CPU and passed in system memory;
//! a GPU conversion path is a later optimisation.

use crate::capture::Frame;
use crate::convert;
use crate::encode::{Encoder, EncoderSettings};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::VARIANT;

/// Longest we wait for the encoder to return a frame before giving up on it.
const OUTPUT_TIMEOUT: Duration = Duration::from_millis(250);

pub struct MfEncoder {
    settings: EncoderSettings,
    session: Option<Session>,
    size: (usize, usize),
    want_idr: bool,
}

impl MfEncoder {
    /// Create the encoder and prove a hardware MFT works at `width` x `height`, so callers can
    /// fall back to software straight away rather than on the first frame.
    pub fn probe(settings: EncoderSettings, width: u32, height: u32) -> Result<Self> {
        let (w, h) = ((width & !1) as usize, (height & !1) as usize);
        let session = Session::new(settings, w, h)?;
        tracing::info!("hardware encoder: {}", session.name);
        Ok(Self { settings, session: Some(session), size: (w, h), want_idr: true })
    }
}

impl Encoder for MfEncoder {
    fn encode(&mut self, frame: &Frame) -> Result<Option<Vec<u8>>> {
        let (w, h) = ((frame.width & !1) as usize, (frame.height & !1) as usize);
        if self.session.is_none() || self.size != (w, h) {
            self.session = None; // release the old encoder before opening a new one
            self.session = Some(Session::new(self.settings, w, h)?);
            self.size = (w, h);
            self.want_idr = true;
        }
        let session = self.session.as_mut().unwrap();
        if std::mem::take(&mut self.want_idr) {
            session.force_keyframe();
        }
        session.encode(frame)
    }

    fn force_keyframe(&mut self) {
        self.want_idr = true;
    }
}

struct Session {
    name: String,
    mft: IMFTransform,
    events: Option<IMFMediaEventGenerator>,
    codec: Option<ICodecAPI>,
    w: usize,
    h: usize,
    frame_dur: i64,
    next_time: i64,
    seq_header: Vec<u8>,
    provides_samples: bool,
    out_size: u32,
    nv12: Vec<u8>,
    /// Number of `METransformNeedInput` events not yet answered (async MFTs).
    credits: u32,
    ready: VecDeque<Vec<u8>>,
}

fn pack(a: u32, b: u32) -> u64 {
    ((a as u64) << 32) | b as u64
}

fn variant_u32(v: u32) -> VARIANT {
    VARIANT::from(v)
}

impl Session {
    fn new(settings: EncoderSettings, w: usize, h: usize) -> Result<Self> {
        unsafe {
            // Per-thread COM init (the capture thread owns the encoder). S_FALSE is fine.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).context("MFStartup")?;

            let reg = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
            let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                None,
                Some(&reg),
                &mut list,
                &mut count,
            )
            .context("enumerating hardware H.264 encoders")?;
            let activates: Vec<IMFActivate> = if list.is_null() {
                Vec::new()
            } else {
                let v = std::slice::from_raw_parts(list, count as usize).iter().flatten().cloned().collect();
                CoTaskMemFree(Some(list as *const _));
                v
            };
            if activates.is_empty() {
                bail!("no hardware H.264 encoder found");
            }

            let mut last_err = anyhow!("no encoder tried");
            for act in activates {
                match Self::configure(&act, settings, w, h) {
                    Ok(s) => return Ok(s),
                    Err(e) => {
                        tracing::debug!("hardware MFT rejected: {e:#}");
                        last_err = e;
                    }
                }
            }
            Err(last_err.context("no hardware H.264 encoder accepted the configuration"))
        }
    }

    unsafe fn configure(act: &IMFActivate, settings: EncoderSettings, w: usize, h: usize) -> Result<Self> {
        let name = {
            let mut p = windows::core::PWSTR::null();
            let mut len = 0u32;
            match act.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut p, &mut len) {
                Ok(()) => {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const _));
                    s
                }
                Err(_) => "unknown".to_string(),
            }
        };
        let mft: IMFTransform = act.ActivateObject().context("activating MFT")?;

        let attrs = mft.GetAttributes().ok();
        let mut events = None;
        if let Some(a) = &attrs {
            if a.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0 {
                a.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
                events = Some(mft.cast::<IMFMediaEventGenerator>()?);
            }
        }

        let codec = mft.cast::<ICodecAPI>().ok();
        if let Some(c) = &codec {
            // Every setting is best-effort; encoders ignore the ones they do not support.
            let _ = c.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
            let _ = c.SetValue(&CODECAPI_AVEncCommonRateControlMode, &variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32));
            let _ = c.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(settings.bitrate_bps));
            let _ = c.SetValue(&CODECAPI_AVEncMPVGOPSize, &variant_u32(settings.fps * 10));
            let _ = c.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &variant_u32(0));
        }

        let set_common = |t: &IMFMediaType| -> windows::core::Result<()> {
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w as u32, h as u32))?;
            t.SetUINT64(&MF_MT_FRAME_RATE, pack(settings.fps, 1))?;
            t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            Ok(())
        };
        let set_color = |t: &IMFMediaType| {
            let _ = t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32);
            let _ = t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32);
            let _ = t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32);
            let _ = t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32);
        };

        let out = MFCreateMediaType()?;
        set_common(&out)?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        out.SetUINT32(&MF_MT_AVG_BITRATE, settings.bitrate_bps)?;
        out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)?;
        set_color(&out);
        mft.SetOutputType(0, &out, 0).context("SetOutputType")?;

        let inp = MFCreateMediaType()?;
        set_common(&inp)?;
        inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        set_color(&inp);
        mft.SetInputType(0, &inp, 0).context("SetInputType (NV12)")?;

        let info = mft.GetOutputStreamInfo(0)?;
        let provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;

        let mut seq_header = Vec::new();
        if let Ok(cur) = mft.GetOutputCurrentType(0) {
            if let Ok(size) = cur.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) {
                seq_header = vec![0u8; size as usize];
                cur.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut seq_header, None)?;
            }
        }

        mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
        mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

        Ok(Session {
            name,
            mft,
            events,
            codec,
            w,
            h,
            frame_dur: 10_000_000 / settings.fps.max(1) as i64,
            next_time: 0,
            seq_header,
            provides_samples,
            out_size: info.cbSize,
            nv12: Vec::new(),
            credits: 0,
            ready: VecDeque::new(),
        })
    }

    fn force_keyframe(&mut self) {
        if let Some(c) = &self.codec {
            let _ = unsafe { c.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_u32(1)) };
        }
    }

    fn encode(&mut self, frame: &Frame) -> Result<Option<Vec<u8>>> {
        convert::bgra_to_nv12(&frame.bgra, frame.width as usize * 4, self.w, self.h, &mut self.nv12);
        let sample = unsafe { self.make_sample()? };

        unsafe {
            if self.events.is_some() {
                // Async MFT: wait until it asks for input, feed, then wait for output.
                self.pump(|s| s.credits > 0)?;
                self.mft.ProcessInput(0, &sample, 0).context("ProcessInput")?;
                self.credits -= 1;
                self.pump(|s| !s.ready.is_empty())?;
            } else {
                self.mft.ProcessInput(0, &sample, 0).context("ProcessInput")?;
                while let Some(data) = self.process_output()? {
                    self.ready.push_back(data);
                }
            }
        }
        Ok(self.ready.pop_front())
    }

    /// Service MFT events until `done` holds or the timeout passes.
    fn pump(&mut self, done: impl Fn(&Session) -> bool) -> Result<()> {
        let start = Instant::now();
        let events = self.events.clone().unwrap();
        while !done(self) {
            if start.elapsed() > OUTPUT_TIMEOUT {
                return Ok(());
            }
            let ev = match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(ev) => ev,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    std::thread::sleep(Duration::from_micros(300));
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let kind = unsafe { ev.GetType()? };
            if kind == METransformNeedInput.0 as u32 {
                self.credits += 1;
            } else if kind == METransformHaveOutput.0 as u32 {
                if let Some(data) = unsafe { self.process_output()? } {
                    self.ready.push_back(data);
                }
            } else if kind == METransformDrainComplete.0 as u32 || kind == MEError.0 as u32 {
                bail!("encoder reported event {kind}");
            }
        }
        Ok(())
    }

    unsafe fn make_sample(&mut self) -> Result<IMFSample> {
        let buf = MFCreateMemoryBuffer(self.nv12.len() as u32)?;
        let mut ptr = std::ptr::null_mut();
        buf.Lock(&mut ptr, None, None)?;
        std::ptr::copy_nonoverlapping(self.nv12.as_ptr(), ptr, self.nv12.len());
        buf.Unlock()?;
        buf.SetCurrentLength(self.nv12.len() as u32)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buf)?;
        sample.SetSampleTime(self.next_time)?;
        self.next_time += self.frame_dur;
        sample.SetSampleDuration(self.frame_dur)?;
        Ok(sample)
    }

    /// Fetch one encoded frame, or `None` if the MFT wants more input.
    unsafe fn process_output(&mut self) -> Result<Option<Vec<u8>>> {
        let mut buffer = MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, ..Default::default() };
        if !self.provides_samples {
            let buf = MFCreateMemoryBuffer(self.out_size.max(1 << 20))?;
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            buffer.pSample = std::mem::ManuallyDrop::new(Some(s));
        }
        let mut status = 0u32;
        let mut bufs = [buffer];
        let r = self.mft.ProcessOutput(0, &mut bufs, &mut status);
        let out = &mut bufs[0];
        let sample = std::mem::ManuallyDrop::take(&mut out.pSample);
        let _events = std::mem::ManuallyDrop::take(&mut out.pEvents);
        match r {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // Output type changed (e.g. after a size change); renegotiate.
                let t = self.mft.GetOutputAvailableType(0, 0)?;
                self.mft.SetOutputType(0, &t, 0)?;
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        }
        let Some(sample) = sample else { return Ok(None) };
        let contiguous = sample.ConvertToContiguousBuffer()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        contiguous.Lock(&mut ptr, None, Some(&mut len))?;
        let mut data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        contiguous.Unlock()?;

        let key = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
        if key && !self.seq_header.is_empty() && !has_sps(&data) {
            let mut with = self.seq_header.clone();
            with.extend_from_slice(&data);
            data = with;
        }
        Ok(Some(data))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = MFShutdown();
        }
    }
}

/// True if an Annex-B access unit contains an SPS NAL (type 7).
fn has_sps(data: &[u8]) -> bool {
    data.windows(4).any(|w| (w[..3] == [0, 0, 1]) && (w[3] & 0x1f) == 7)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_sps() {
        assert!(has_sps(&[0, 0, 0, 1, 0x67, 1, 2]));
        assert!(has_sps(&[0, 0, 1, 0x27, 1]));
        assert!(!has_sps(&[0, 0, 0, 1, 0x65, 1, 2]));
    }
}
