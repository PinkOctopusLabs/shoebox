//! Movie playback: AVFoundation's `AVPlayer` (decoding, sound, timing) with
//! an `AVPlayerItemVideoOutput` handing each frame over as pixels for an
//! egui texture. Frames come in the display's colour space (Display P3 or
//! sRGB, as photos are drawn) at most `max_edge` long, as the track stores
//! them: [`Player::quarter_turns`] says how to turn them upright. macOS only.
#![cfg_attr(target_os = "macos", allow(unsafe_code))]

#[cfg(not(target_os = "macos"))]
use std::path::Path;

#[cfg(not(target_os = "macos"))]
use shoebox_decode::Target;

/// A frame: `width × height` RGBA pixels, as stored in the track.
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Whether the movie can play yet.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Loading,
    Ready,
    Failed(String),
}

#[cfg(target_os = "macos")]
pub use mac::{Player, run_main_loop};

#[cfg(not(target_os = "macos"))]
pub fn run_main_loop(_: f64) {}

#[cfg(not(target_os = "macos"))]
pub struct Player;

#[cfg(not(target_os = "macos"))]
impl Player {
    pub fn open(_: &Path, _: u32, _: Target) -> Result<Self, String> {
        Err("Playing movies needs macOS".into())
    }
    pub fn play(&self) {}
    pub fn pause(&self) {}
    pub fn is_playing(&self) -> bool {
        false
    }
    pub fn seek(&self, _: f64) {}
    pub fn time(&self) -> f64 {
        0.0
    }
    pub fn duration(&self) -> Option<f64> {
        None
    }
    pub fn status(&self) -> Status {
        Status::Failed("Playing movies needs macOS".into())
    }
    pub fn set_muted(&self, _: bool) {}
    pub fn set_volume(&self, _: f32) {}
    pub fn quarter_turns(&self) -> u8 {
        0
    }
    pub fn frame(&self) -> Option<Frame> {
        None
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::path::Path;

    use objc2::msg_send;
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2_foundation::{NSString, NSURL};
    use shoebox_decode::Target;

    use super::{Frame, Status};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CMTime {
        value: i64,
        timescale: i32,
        flags: u32,
        epoch: i64,
    }

    // SAFETY: CoreMedia's CMTime is {int64 value; int32 timescale; uint32
    // flags; int64 epoch}, an anonymous struct in the headers.
    unsafe impl objc2::Encode for CMTime {
        const ENCODING: objc2::Encoding = objc2::Encoding::Struct(
            "?",
            &[
                objc2::Encoding::LongLong,
                objc2::Encoding::Int,
                objc2::Encoding::UInt,
                objc2::Encoding::LongLong,
            ],
        );
    }

    // SAFETY: as above, behind a pointer.
    unsafe impl objc2::RefEncode for CMTime {
        const ENCODING_REF: objc2::Encoding =
            objc2::Encoding::Pointer(&<CMTime as objc2::Encode>::ENCODING);
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    // SAFETY: CoreGraphics' CGSize is two CGFloats (f64 on 64-bit).
    unsafe impl objc2::Encode for CGSize {
        const ENCODING: objc2::Encoding = objc2::Encoding::Struct(
            "CGSize",
            &[objc2::Encoding::Double, objc2::Encoding::Double],
        );
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGAffineTransform {
        a: f64,
        b: f64,
        c: f64,
        d: f64,
        tx: f64,
        ty: f64,
    }

    // SAFETY: CoreGraphics' CGAffineTransform is six CGFloats.
    unsafe impl objc2::Encode for CGAffineTransform {
        const ENCODING: objc2::Encoding =
            objc2::Encoding::Struct("CGAffineTransform", &[const { objc2::Encoding::Double }; 6]);
    }

    /// Opaque CVPixelBuffer.
    #[repr(C)]
    struct CVBuffer {
        _private: [u8; 0],
    }

    // SAFETY: an opaque CoreVideo struct, only used behind a pointer.
    unsafe impl objc2::RefEncode for CVBuffer {
        const ENCODING_REF: objc2::Encoding =
            objc2::Encoding::Pointer(&objc2::Encoding::Struct("__CVBuffer", &[]));
    }

    #[link(name = "AVFoundation", kind = "framework")]
    unsafe extern "C" {
        static AVVideoColorPropertiesKey: &'static NSString;
        static AVVideoColorPrimariesKey: &'static NSString;
        static AVVideoColorPrimaries_P3_D65: &'static NSString;
        static AVVideoColorPrimaries_ITU_R_709_2: &'static NSString;
        static AVVideoTransferFunctionKey: &'static NSString;
        static AVVideoTransferFunction_IEC_sRGB: &'static NSString;
        static AVVideoYCbCrMatrixKey: &'static NSString;
        static AVVideoYCbCrMatrix_ITU_R_709_2: &'static NSString;
        static AVMediaTypeVideo: &'static NSString;
    }

    #[link(name = "CoreMedia", kind = "framework")]
    unsafe extern "C" {
        fn CMTimeMakeWithSeconds(seconds: f64, preferred_timescale: i32) -> CMTime;
        fn CMTimeGetSeconds(time: CMTime) -> f64;
    }

    #[link(name = "QuartzCore", kind = "framework")]
    unsafe extern "C" {
        fn CACurrentMediaTime() -> f64;
    }

    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        static kCVPixelBufferPixelFormatTypeKey: &'static NSString;
        static kCVPixelBufferWidthKey: &'static NSString;
        static kCVPixelBufferHeightKey: &'static NSString;
        fn CVPixelBufferLockBaseAddress(buffer: *mut CVBuffer, flags: u64) -> i32;
        fn CVPixelBufferUnlockBaseAddress(buffer: *mut CVBuffer, flags: u64) -> i32;
        fn CVPixelBufferGetBaseAddress(buffer: *mut CVBuffer) -> *const u8;
        fn CVPixelBufferGetBytesPerRow(buffer: *mut CVBuffer) -> usize;
        fn CVPixelBufferGetWidth(buffer: *mut CVBuffer) -> usize;
        fn CVPixelBufferGetHeight(buffer: *mut CVBuffer) -> usize;
        fn CVBufferRelease(buffer: *mut CVBuffer);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: &'static NSString;
        fn CFRunLoopRunInMode(mode: &NSString, seconds: f64, return_after_source: u8) -> i32;
    }

    /// Run the main thread's run loop for up to `secs`: AVFoundation
    /// reports there. Only for tests on the main thread, which have no
    /// event loop (the app's runs all the time).
    pub fn run_main_loop(secs: f64) {
        // SAFETY: runs the current thread's run loop in the default mode.
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, secs, 0);
        }
    }

    /// `kCVPixelFormatType_32BGRA`.
    const BGRA: u32 = u32::from_be_bytes(*b"BGRA");
    const LOCK_READ_ONLY: u64 = 1;

    pub struct Player {
        player: Retained<AnyObject>,
        item: Retained<AnyObject>,
        output: Retained<AnyObject>,
        turns: u8,
    }

    fn class(name: &std::ffi::CStr) -> Result<&'static AnyClass, String> {
        AnyClass::get(name).ok_or_else(|| format!("{} is missing", name.to_string_lossy()))
    }

    impl Player {
        /// Open a movie for playing (paused at its start). Reads only the
        /// header here; AVFoundation reads the rest as it plays.
        pub fn open(path: &Path, max_edge: u32, target: Target) -> Result<Self, String> {
            let path = path.to_str().ok_or("The file's name isn't UTF-8")?;
            let url = NSURL::fileURLWithPath(&NSString::from_str(path));
            // SAFETY: standard AVFoundation, Foundation and CoreVideo calls
            // with valid objects; nil results are checked. The statics are
            // the frameworks' own constant strings.
            unsafe {
                let none: Option<&AnyObject> = None;
                let asset: Option<Retained<AnyObject>> =
                    msg_send![class(c"AVURLAsset")?, URLAssetWithURL: &*url, options: none];
                let asset = asset.ok_or("AVFoundation can't open it")?;
                let tracks: Option<Retained<AnyObject>> =
                    msg_send![&*asset, tracksWithMediaType: AVMediaTypeVideo];
                let track: Option<Retained<AnyObject>> = match &tracks {
                    Some(t) => msg_send![&**t, firstObject],
                    None => None,
                };
                let track = track.ok_or("It has no video AVFoundation can play")?;
                let natural: CGSize = msg_send![&*track, naturalSize];
                let t: CGAffineTransform = msg_send![&*track, preferredTransform];
                let turns = quarter_turns(t.b.atan2(t.a).to_degrees());

                // The frames' size: the track's, at most `max_edge` long.
                let (w, h) = (natural.width.abs().max(1.0), natural.height.abs().max(1.0));
                let k = (f64::from(max_edge) / w.max(h)).min(1.0);
                let (w, h) = ((w * k).round() as u32, (h * k).round() as u32);

                let dict =
                    |pairs: &[(&NSString, &AnyObject)]| -> Result<Retained<AnyObject>, String> {
                        let d: Option<Retained<AnyObject>> =
                            msg_send![class(c"NSMutableDictionary")?, dictionary];
                        let d = d.ok_or("NSMutableDictionary")?;
                        for (k, v) in pairs {
                            let _: () = msg_send![&*d, setObject: *v, forKey: *k];
                        }
                        Ok(d)
                    };
                let number = |n: u32| -> Result<Retained<AnyObject>, String> {
                    let n: Option<Retained<AnyObject>> =
                        msg_send![class(c"NSNumber")?, numberWithUnsignedInt: n];
                    n.ok_or_else(|| "NSNumber".to_owned())
                };
                let primaries = match target {
                    Target::DisplayP3 => AVVideoColorPrimaries_P3_D65,
                    Target::Srgb => AVVideoColorPrimaries_ITU_R_709_2,
                };
                let colour = dict(&[
                    (AVVideoColorPrimariesKey, primaries),
                    (AVVideoTransferFunctionKey, AVVideoTransferFunction_IEC_sRGB),
                    (AVVideoYCbCrMatrixKey, AVVideoYCbCrMatrix_ITU_R_709_2),
                ])?;
                let (format, width, height) = (number(BGRA)?, number(w)?, number(h)?);
                let settings = dict(&[
                    (kCVPixelBufferPixelFormatTypeKey, &*format),
                    (kCVPixelBufferWidthKey, &*width),
                    (kCVPixelBufferHeightKey, &*height),
                    (AVVideoColorPropertiesKey, &*colour),
                ])?;
                let output: Allocated<AnyObject> =
                    msg_send![class(c"AVPlayerItemVideoOutput")?, alloc];
                let output: Option<Retained<AnyObject>> =
                    msg_send![output, initWithOutputSettings: &*settings];
                let output = output.ok_or("AVFoundation won't make frames of it")?;
                let item: Option<Retained<AnyObject>> =
                    msg_send![class(c"AVPlayerItem")?, playerItemWithAsset: &*asset];
                let item = item.ok_or("AVFoundation can't play it")?;
                let _: () = msg_send![&*item, addOutput: &*output];
                let player: Option<Retained<AnyObject>> =
                    msg_send![class(c"AVPlayer")?, playerWithPlayerItem: &*item];
                let player = player.ok_or("AVFoundation can't play it")?;
                Ok(Self {
                    player,
                    item,
                    output,
                    turns,
                })
            }
        }

        pub fn play(&self) {
            // SAFETY: a message to the player this value owns.
            unsafe {
                let _: () = msg_send![&*self.player, play];
            }
        }

        pub fn pause(&self) {
            // SAFETY: as above.
            unsafe {
                let _: () = msg_send![&*self.player, pause];
            }
        }

        /// Playing, or waiting to (buffering).
        pub fn is_playing(&self) -> bool {
            // SAFETY: as above; `timeControlStatus` is an NSInteger enum
            // (0: paused).
            let status: isize = unsafe { msg_send![&*self.player, timeControlStatus] };
            status != 0
        }

        /// Go to `secs` exactly (not the nearest keyframe).
        pub fn seek(&self, secs: f64) {
            // SAFETY: as above, with CMTimes CoreMedia made.
            unsafe {
                let to = CMTimeMakeWithSeconds(secs.max(0.0), 600);
                let zero = CMTimeMakeWithSeconds(0.0, 600);
                let _: () = msg_send![
                    &*self.player,
                    seekToTime: to,
                    toleranceBefore: zero,
                    toleranceAfter: zero
                ];
            }
        }

        /// Where it's at, in seconds.
        pub fn time(&self) -> f64 {
            // SAFETY: as above.
            let t = unsafe {
                let t: CMTime = msg_send![&*self.player, currentTime];
                CMTimeGetSeconds(t)
            };
            if t.is_finite() { t.max(0.0) } else { 0.0 }
        }

        /// How long it is, once known.
        pub fn duration(&self) -> Option<f64> {
            // SAFETY: as above.
            let d = unsafe {
                let d: CMTime = msg_send![&*self.item, duration];
                CMTimeGetSeconds(d)
            };
            (d.is_finite() && d > 0.0).then_some(d)
        }

        pub fn status(&self) -> Status {
            // SAFETY: as above; `status` is an NSInteger enum (1: ready to
            // play, 2: failed), `error` an NSError or nil.
            unsafe {
                let status: isize = msg_send![&*self.item, status];
                match status {
                    1 => Status::Ready,
                    2 => {
                        let error: Option<Retained<AnyObject>> = msg_send![&*self.item, error];
                        let text: Option<Retained<NSString>> = match &error {
                            Some(e) => msg_send![&**e, localizedDescription],
                            None => None,
                        };
                        Status::Failed(text.map_or_else(
                            || "AVFoundation can't play it".to_owned(),
                            |t| t.to_string(),
                        ))
                    }
                    _ => Status::Loading,
                }
            }
        }

        pub fn set_muted(&self, muted: bool) {
            // SAFETY: as above.
            unsafe {
                let _: () = msg_send![&*self.player, setMuted: Bool::new(muted)];
            }
        }

        /// Volume from 0 to 1 (of the Mac's own).
        pub fn set_volume(&self, volume: f32) {
            // SAFETY: as above.
            unsafe {
                let _: () = msg_send![&*self.player, setVolume: volume.clamp(0.0, 1.0)];
            }
        }

        /// Quarter turns clockwise that make a frame upright.
        pub fn quarter_turns(&self) -> u8 {
            self.turns
        }

        /// The frame to show now, if it's new since the last one.
        pub fn frame(&self) -> Option<Frame> {
            // SAFETY: the output belongs to this value's item. The pixel
            // buffer comes back +1 and is released here; its memory is read
            // only between the lock and the unlock, within its height and
            // bytes per row.
            unsafe {
                let at: CMTime =
                    msg_send![&*self.output, itemTimeForHostTime: CACurrentMediaTime()];
                let new: Bool = msg_send![&*self.output, hasNewPixelBufferForItemTime: at];
                if !new.as_bool() {
                    return None;
                }
                let shown: *mut CMTime = std::ptr::null_mut();
                let buffer: *mut CVBuffer = msg_send![&*self.output, copyPixelBufferForItemTime: at, itemTimeForDisplay: shown];
                if buffer.is_null() {
                    return None;
                }
                let frame = if CVPixelBufferLockBaseAddress(buffer, LOCK_READ_ONLY) == 0 {
                    let base = CVPixelBufferGetBaseAddress(buffer);
                    let (width, height) = (
                        CVPixelBufferGetWidth(buffer),
                        CVPixelBufferGetHeight(buffer),
                    );
                    let stride = CVPixelBufferGetBytesPerRow(buffer);
                    let frame = (!base.is_null() && stride >= width * 4).then(|| {
                        let bytes = std::slice::from_raw_parts(base, stride * height);
                        Frame {
                            width,
                            height,
                            rgba: bgra_to_rgba(bytes, width, height, stride),
                        }
                    });
                    CVPixelBufferUnlockBaseAddress(buffer, LOCK_READ_ONLY);
                    frame
                } else {
                    None
                };
                CVBufferRelease(buffer);
                frame
            }
        }
    }

    impl Drop for Player {
        fn drop(&mut self) {
            // Stop the sound at once (the player may outlive this value
            // briefly inside AVFoundation).
            self.pause();
        }
    }

    /// The nearest quarter turn to a track's rotation, in degrees.
    fn quarter_turns(degrees: f64) -> u8 {
        ((degrees / 90.0).round() as i64).rem_euclid(4) as u8
    }

    fn bgra_to_rgba(bytes: &[u8], width: usize, height: usize, stride: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(width * height * 4);
        for row in bytes.chunks(stride).take(height) {
            for p in row[..width * 4].as_chunks::<4>().0 {
                out.extend_from_slice(&[p[2], p[1], p[0], 255]);
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn quarter_turns() {
            assert_eq!(super::quarter_turns(0.0), 0);
            assert_eq!(super::quarter_turns(90.0), 1);
            assert_eq!(super::quarter_turns(180.0), 2);
            assert_eq!(super::quarter_turns(-180.0), 2);
            assert_eq!(super::quarter_turns(-90.0), 3);
        }
    }
}
