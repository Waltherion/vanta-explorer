//! Camera RAW through the system libraw.
//!
//! A RAW file is not an image yet: it is one brightness per photosite behind a colour filter,
//! plus the metadata needed to turn that into colour. libraw does the turning, and the settings
//! below decide what kind of image comes out:
//!
//! - the camera's own white balance, because that is what the photographer saw;
//! - no automatic brightening, so two frames of the same scene stay comparable;
//! - 16 bits with a linear curve, since this pipeline works in linear light and would only
//!   have to undo a curve applied here.
//!
//! The result is display-referred: white in the file is white on the screen, so it is treated
//! as SDR. A RAW holds more shadow detail than an SDR file, but nothing above white.

use crate::color::{Primaries, Tf};
use crate::decode::{sample_rgba, DecodeError, HdrImage};
use std::path::Path;

#[allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code)]
#[allow(unnecessary_transmutes, clippy::all)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/raw.rs"));
}

/// Frees the processor no matter which way the function leaves.
struct Processor(*mut sys::libraw_data_t);

impl Drop for Processor {
    fn drop(&mut self) {
        unsafe { sys::libraw_close(self.0) };
    }
}

/// The file extensions libraw is asked to handle. Anything not here goes to another decoder.
pub const EXTENSIONS: &[&str] = &[
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "sr2", "srf", "raf", "rw2", "orf", "pef", "srw", "x3f", "raw", "3fr", "erf", "kdc",
    "dcr", "mos", "mef", "mrw", "iiq", "rwl",
];

pub fn decode(path: &Path, max_dim: u32) -> Result<HdrImage, DecodeError> {
    let cpath = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| DecodeError::Decode("path contains a null byte".into()))?;
    unsafe {
        let raw = sys::libraw_init(0);
        if raw.is_null() {
            return Err(DecodeError::Decode("libraw_init failed".into()));
        }
        let proc = Processor(raw);
        check(sys::libraw_open_file(proc.0, cpath.as_ptr()), "open")?;
        check(sys::libraw_unpack(proc.0), "unpack")?;

        let p = &mut (*proc.0).params;
        p.output_bps = 16;
        p.output_color = 1; // sRGB primaries, which is what the rest of the pipeline expects
        p.use_camera_wb = 1;
        p.no_auto_bright = 1;
        p.gamm[0] = 1.0; // linear in, linear out
        p.gamm[1] = 1.0;
        // Half size is a quarter of the work and is plenty when the caller only wants a
        // thumbnail; full detail is decoded whenever the size is not capped that far down.
        let full = ((*proc.0).sizes.iwidth as u32).max((*proc.0).sizes.iheight as u32);
        p.half_size = i32::from(max_dim > 0 && full > max_dim * 2);

        check(sys::libraw_dcraw_process(proc.0), "process")?;
        let mut err = 0;
        let mem = sys::libraw_dcraw_make_mem_image(proc.0, &mut err);
        if mem.is_null() {
            return Err(DecodeError::Decode(format!("libraw could not build the image ({err})")));
        }
        let image = &*mem;
        let (w, h, colors, bits) = (image.width as u32, image.height as u32, image.colors as usize, image.bits);
        if colors < 3 || bits != 16 {
            sys::libraw_dcraw_clear_mem(mem);
            return Err(DecodeError::Decode(format!("unexpected {colors} channels at {bits} bits")));
        }
        let count = w as usize * h as usize * colors;
        let samples = std::slice::from_raw_parts(image.data.as_ptr().cast::<u16>(), count);
        // Read at the size wanted, straight from libraw's buffer.
        let (rgba, w, h) = sample_rgba(w, h, max_dim, |x, y| {
            let px = &samples[(y * w as usize + x) * colors..];
            [px[0] as f32 / 65535.0, px[1] as f32 / 65535.0, px[2] as f32 / 65535.0, 1.0]
        });
        sys::libraw_dcraw_clear_mem(mem);
        Ok(HdrImage::from_linear_f32(w, h, &rgba, Tf::Srgb, Primaries::Bt709))
    }
}

fn check(code: i32, what: &str) -> Result<(), DecodeError> {
    if code == 0 {
        return Ok(());
    }
    let msg = unsafe { std::ffi::CStr::from_ptr(sys::libraw_strerror(code)) }.to_string_lossy().into_owned();
    Err(DecodeError::Decode(format!("{what}: {msg}")))
}
