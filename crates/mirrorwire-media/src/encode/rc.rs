//! Rate-control helpers: which mode a backend really uses, how it is applied to the codec
//! context or the VAAPI option dictionary, and the one-line summary shown in the status.

use super::{EncoderBackend, EncoderConfig, RateControl};
use anyhow::Result;
use ffmpeg_next::{encoder, ffi, Dictionary};

pub(super) fn effective_rc(backend: EncoderBackend, rc: RateControl) -> RateControl {
    match (backend, rc) {
        (_, RateControl::Auto) => RateControl::Cbr,
        (EncoderBackend::X264, RateControl::Icq) => RateControl::Cqp,
        (EncoderBackend::VideoToolbox, RateControl::Icq) => RateControl::Cqp,
        (_, other) => other,
    }
}

pub(super) fn apply_rate_control(
    enc: &mut encoder::video::Video,
    backend: EncoderBackend,
    rc: RateControl,
    bitrate: i64,
    qp: u32,
) -> Result<()> {
    let bitrate = bitrate.max(100_000);
    let maxrate = match rc {
        RateControl::Vbr => bitrate + bitrate / 4,
        _ => bitrate,
    };
    // VBV of 100 ms of bitrate: a scene change or IDR can't produce a frame
    // that takes several hundred ms to send (the old 2 s buffer let one
    // frame grow to megabytes, which stalled Wi-Fi and the viewer).
    let buffer = (bitrate / 10).clamp(1, i32::MAX as i64) as i32;
    match rc {
        RateControl::Cqp | RateControl::Icq => {
            enc.set_bit_rate(0);
            enc.set_max_bit_rate(0);
            // SAFETY: `raw` points to the live `AVCodecContext` owned by `enc` (borrowed
            // mutably); only plain integer fields are set.
            unsafe {
                let raw = enc.as_mut_ptr();
                (*raw).qmin = qp as i32;
                (*raw).qmax = qp as i32;
                (*raw).rc_buffer_size = 0;
            }
        }
        _ => {
            enc.set_bit_rate(bitrate as usize);
            enc.set_max_bit_rate(maxrate as usize);
            // SAFETY: as above, plain integer fields of the live codec context.
            unsafe {
                let raw = enc.as_mut_ptr();
                (*raw).rc_buffer_size = buffer;
                (*raw).rc_initial_buffer_occupancy = buffer / 10 * 9;
            }
        }
    }
    let _ = backend;
    Ok(())
}

pub(super) fn apply_vaapi_opts(
    opts: &mut Dictionary,
    rc: RateControl,
    bitrate: i64,
    config: &EncoderConfig,
) {
    let mode = match rc {
        RateControl::Cbr => "CBR",
        RateControl::Vbr => "VBR",
        RateControl::Cqp => "CQP",
        RateControl::Icq => "ICQ",
        RateControl::Auto => "CBR",
    };
    opts.set("rc_mode", mode);
    opts.set("async_depth", "1");
    opts.set("profile", "high");
    opts.set("low_power", "0");
    if config.intra_refresh {
        opts.set("idr_interval", "0");
        opts.set("int_ref_type", "1");
    }
    match rc {
        RateControl::Cqp => {
            opts.set("qp", &config.qp.to_string());
        }
        RateControl::Icq => {
            opts.set("quality", &config.qp.to_string());
        }
        RateControl::Vbr => {
            opts.set("maxrate", &bitrate.to_string());
            opts.set("b", &bitrate.to_string());
        }
        _ => {
            opts.set("b", &bitrate.to_string());
            opts.set("maxrate", &bitrate.to_string());
        }
    }
}

pub(super) fn describe_rc(
    enc: &encoder::Video,
    name: &str,
    rc: RateControl,
    config: &EncoderConfig,
    conversion: &str,
) -> String {
    // SAFETY: `raw` points to the live codec context owned by `enc`; we only read integer fields.
    let (bit_rate, max_rate, buf, gop, profile, qmin, qmax) = unsafe {
        let raw = enc.as_ptr();
        (
            (*raw).bit_rate,
            (*raw).rc_max_rate,
            (*raw).rc_buffer_size,
            (*raw).gop_size,
            (*raw).profile,
            (*raw).qmin,
            (*raw).qmax,
        )
    };
    let profile = match profile {
        x if x == ffi::FF_PROFILE_H264_HIGH => "high".to_owned(),
        x if x == ffi::FF_PROFILE_H264_MAIN => "main".to_owned(),
        x if x == ffi::FF_PROFILE_H264_BASELINE => "baseline".to_owned(),
        x => format!("{x}"),
    };
    format!(
        "encoder {name} {}x{}  rc={} ({})  bitrate={:.2}/{:.2} Mbit/s  vbv={:.2} Mbit  qp={}/qmin={qmin}/qmax={qmax}  profile={profile}  gop={gop}  bframes=0  convert={conversion}",
        config.width,
        config.height,
        rc.as_str(),
        if matches!(rc, RateControl::Cqp | RateControl::Icq) {
            "quality"
        } else {
            "bitrate"
        },
        bit_rate as f64 / 1e6,
        max_rate as f64 / 1e6,
        buf as f64 / 1e6,
        config.qp,
    )
}
