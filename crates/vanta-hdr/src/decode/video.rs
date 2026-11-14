//! Video first-frame decode for thumbnails and the preview pane.
//!
//! The colour rules for video frames:
//! - Read transfer/primaries/range from the DECODED FRAME, not the container; containers
//!   routinely lie or say nothing.
//! - Full-range flag matters: AV1 files from many encoders ARE full range, and treating them as
//!   limited lifts black and dulls white.
//! - PQ/HLG frames must be linearised, or the thumbnail comes out grey/washed. The same
//!   failure an ffmpeg thumbnail without zscale+tonemap shows.
//!
//! Pixel-format plumbing is delegated to swscale (every YUV layout, bit depth and matrix in
//! one battle-tested place); we keep the transfer-function and primaries knowledge.

use crate::color::{Primaries, Tf};
use crate::decode::{linearise_rgba, DecodeError, HdrImage};
use ffmpeg_next as ffmpeg;
use std::path::Path;
use std::sync::Once;

static INIT: Once = Once::new();

pub fn decode_first_frame(path: &Path, max_dim: u32, page: u32) -> Result<HdrImage, DecodeError> {
    INIT.call_once(|| {
        ffmpeg::init().ok();
        // Decoder chatter (SEI warnings etc.) is noise at thumbnail volume.
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
    });

    let mut ictx = ffmpeg::format::input(&path).map_err(|e| DecodeError::Io(e.to_string()))?;

    let stream = ictx
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or_else(|| DecodeError::Unsupported("no video stream".into()))?;
    let stream_index = stream.index();

    let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| DecodeError::Decode(e.to_string()))?;
    let mut decoder = ctx
        .decoder()
        .video()
        .map_err(|e| DecodeError::Decode(e.to_string()))?;

    // Seek ~1s in when the file is long enough: frame zero of real footage is often a black
    // fade-in, which makes a useless thumbnail. Failure is fine; we just take frame zero.
    let duration = ictx.duration(); // AV_TIME_BASE units, negative when unknown
    let total_pages = if duration > 0 { 100 } else { 1 };
    if duration > 0 {
        let ts = if page > 0 {
            let pct = (page.min(100) as f64) / 100.0;
            (duration as f64 * pct) as i64
        } else if duration > 3 * ffmpeg::ffi::AV_TIME_BASE as i64 {
            ffmpeg::ffi::AV_TIME_BASE as i64
        } else {
            0
        };
        if ts > 0 {
            let _ = ictx.seek(ts, ..ts);
        }
    }

    // Pull packets until one frame comes out.
    let mut frame = ffmpeg::util::frame::Video::empty();
    let mut got = false;
    for (s, packet) in ictx.packets() {
        if s.index() != stream_index {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        if decoder.receive_frame(&mut frame).is_ok() {
            got = true;
            break;
        }
    }
    if !got {
        // Flush: short files may need the EOF drain.
        let _ = decoder.send_eof();
        got = decoder.receive_frame(&mut frame).is_ok();
    }
    if !got {
        return Err(DecodeError::Decode("no decodable frame".into()));
    }

    // --- colour metadata, from the frame ---
    use ffmpeg::util::color;
    let tf = match frame.color_transfer_characteristic() {
        color::TransferCharacteristic::SMPTE2084 => Tf::Pq,
        color::TransferCharacteristic::ARIB_STD_B67 => Tf::Hlg,
        color::TransferCharacteristic::Linear => Tf::Linear,
        _ => Tf::Srgb,
    };
    let primaries = match frame.color_primaries() {
        color::Primaries::BT2020 => Primaries::Bt2020,
        color::Primaries::SMPTE431 | color::Primaries::SMPTE432 => Primaries::DisplayP3,
        _ => Primaries::Bt709,
    };
    let full_range = frame.color_range() == color::Range::JPEG;
    let src_space = frame.color_space();

    // --- fit within max_dim while scaling; swscale does both in one pass ---
    let (w, h) = (frame.width(), frame.height());
    let (dw, dh) = if max_dim > 0 && (w > max_dim || h > max_dim) {
        let s = max_dim as f32 / w.max(h) as f32;
        (
            ((w as f32 * s) as u32).max(1),
            ((h as f32 * s) as u32).max(1),
        )
    } else {
        (w, h)
    };

    let mut scaler = ffmpeg::software::scaling::Context::get(
        frame.format(),
        w,
        h,
        ffmpeg::format::Pixel::RGBA64LE, // 16-bit so >8-bit HDR survives
        dw,
        dh,
        ffmpeg::software::scaling::Flags::BILINEAR,
    )
    .map_err(|e| DecodeError::Decode(e.to_string()))?;

    // swscale defaults to BT.601 coefficients; tell it the actual matrix and range or
    // BT.2020/BT.709 material shifts in hue. (sws_setColorspaceDetails is not wrapped by
    // ffmpeg-next, so this goes through the sys layer.)
    unsafe {
        use ffmpeg::ffi::*;
        let cs = match src_space {
            color::Space::BT2020NCL | color::Space::BT2020CL => SWS_CS_BT2020,
            color::Space::BT470BG | color::Space::SMPTE170M => SWS_CS_ITU601,
            _ => SWS_CS_ITU709,
        };
        sws_setColorspaceDetails(
            scaler.as_mut_ptr(),
            sws_getCoefficients(cs),
            full_range as i32,
            sws_getCoefficients(cs), // dst matrix unused for RGB output
            1,                       // RGB is always full range
            0,
            1 << 16,
            1 << 16,
        );
    }

    let mut rgb = ffmpeg::util::frame::Video::empty();
    scaler
        .run(&frame, &mut rgb)
        .map_err(|e| DecodeError::Decode(e.to_string()))?;

    // RGBA64LE, respecting the stride swscale may pad rows with.
    let stride = rgb.stride(0);
    let data = rgb.data(0);
    let mut encoded = vec![0.0f32; dw as usize * dh as usize * 4];
    for y in 0..dh as usize {
        let row = &data[y * stride..];
        for x in 0..dw as usize {
            for c in 0..4 {
                let i = (x * 4 + c) * 2;
                let v = u16::from_le_bytes([row[i], row[i + 1]]);
                encoded[(y * dw as usize + x) * 4 + c] = v as f32 / 65535.0;
            }
        }
    }

    let linear = linearise_rgba(&encoded, tf, true);
    let mut img = HdrImage::from_linear_f32(dw, dh, &linear, tf, primaries);
    img.pages = total_pages;
    Ok(img)
}
