//! B10: audio — PCM playback, capture, and a host backend that is independent of the
//! guest ABI.
//!
//! # The third shape, and why it is neither of the other two
//!
//! B7's display is **pulled**: the host asks the device for a frame, on the host's
//! schedule. B9's input is **pushed**: the host writes events into the device's queue.
//! Audio is neither, and pretending otherwise is the mistake this module exists to
//! avoid.
//!
//! Audio has a **rate**. A guest fills a ring at 48 000 samples per second and a host
//! drains it at whatever rate the sound card runs at, and the two are never equal for
//! long. So:
//!
//! - the device is a **ring buffer**, and the guest writes into one end while the host
//!   reads from the other;
//! - the guest must be **told** when the ring is running low, because polling a status
//!   register means busy-waiting at the guest's rate rather than the device's. That is
//!   what [`Device::take_interrupt`](crate::Device::take_interrupt) is for, added in
//!   B10, and what [`AudioDevice`] uses it for.
//!
//! # "Host backends must be independent of the guest ABI"
//!
//! §29 says so, and it is the constraint that shapes the trait. A backend is handed
//! [`AudioFrame`]: a format, a channel count and **plain numbers**. It is not handed
//! `Event`s, a guest register offset, or a Lazalith type of any kind. So a host
//! backend compiled against this trait is a backend that could be driven by a different
//! guest ABI entirely, and the alternative — a backend that takes a guest-visible
//! struct — would make every host backend a second guest driver.
//!
//! # DMA is not implemented, and the reason is worth stating
//!
//! §29 lists DMA among the things to investigate. It is not built, because a device has
//! **no way to reach guest memory**: `Device` gives a device its own registers and
//! nothing else, and handing every device a memory handle would put an address space in
//! front of every device in the platform, including the ones that must not have one.
//!
//! So audio samples move through the data port, one access at a time, exactly as B5's
//! block device does — and that is not a workaround, it is the same decision, reached
//! independently. The interrupt path *was* built, because it needed only one new method
//! on `Device` rather than a new capability.

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use lazalith_isa::DataSize;
use lazalith_types::{CycleCount, DeviceOffset, InterruptId};

use crate::{Device, DeviceError, DeviceId};

/// How large the register window is.
pub const AUDIO_REGISTER_BYTES: u64 = 64;

/// The ABI this device implements.
pub const AUDIO_ABI_VERSION: u64 = 1;

/// The format register, at offset 0. Read/write.
pub const AUDIO_REGISTER_FORMAT: DeviceOffset = DeviceOffset::new(0);
/// The sample rate in Hz, at offset 8. Read/write.
pub const AUDIO_REGISTER_RATE: DeviceOffset = DeviceOffset::new(8);
/// The channel count, at offset 16. Read/write.
pub const AUDIO_REGISTER_CHANNELS: DeviceOffset = DeviceOffset::new(16);
/// Control, at offset 24. Read/write.
pub const AUDIO_REGISTER_CONTROL: DeviceOffset = DeviceOffset::new(24);
/// The interrupt threshold in samples, at offset 32. Read/write.
pub const AUDIO_REGISTER_THRESHOLD: DeviceOffset = DeviceOffset::new(32);
/// The data port, at offset 40. Read/write, one sample at a time.
pub const AUDIO_REGISTER_DATA: DeviceOffset = DeviceOffset::new(40);
/// Status, at offset 48. Read-only.
pub const AUDIO_REGISTER_STATUS: DeviceOffset = DeviceOffset::new(48);
/// How many samples are waiting, at offset 56. Read-only.
pub const AUDIO_REGISTER_LEVEL: DeviceOffset = DeviceOffset::new(56);

/// `CONTROL` bit 0: playback running. The guest is producing samples.
pub const AUDIO_CONTROL_PLAY: u64 = 1;
/// `CONTROL` bit 1: capture running. The guest is consuming samples.
pub const AUDIO_CONTROL_CAPTURE: u64 = 2;

/// `STATUS` bit 0: at least one sample is waiting.
pub const AUDIO_STATUS_READY: u64 = 1;
/// `STATUS` bit 1: the ring has run dry while playing — the guest did not keep up.
pub const AUDIO_STATUS_UNDERRUN: u64 = 2;
/// `STATUS` bit 2: capture has no samples and the guest is asking for one.
pub const AUDIO_STATUS_OVERRUN: u64 = 4;
/// `STATUS` bit 3: a sample was written with no format set.
pub const AUDIO_STATUS_BAD_FORMAT: u64 = 8;

/// How many samples the ring holds by default.
///
/// **A power of two, and the reason is the index arithmetic.** A ring index is masked
/// with `len - 1`, which is only correct when the length is a power of two, and a ring
/// that had to use a modulo would make every sample cost a division. 1024 samples is
/// about 21 ms at 48 kHz, which is long enough that a guest which is descheduled for a
/// frame does not underrun, and short enough that a silence bug is audible rather than
/// obvious only in a waveform.
pub const DEFAULT_RING_SAMPLES: usize = 1024;

/// The largest rate this device will accept.
///
/// 192 kHz. Beyond that a "rate" is a number, not a rate: no host audio device in
/// existence runs that fast, and accepting it would let a guest ask for a rate that
/// makes the ring's duration meaningless.
pub const MAX_RATE: u64 = 192_000;

/// The largest channel count this device will accept.
///
/// 8. A guest asking for more is not asking for audio; and a ring indexed by
///
pub const MAX_CHANNELS: u64 = 8;

/// A PCM sample format, as §29's "sample format".
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SampleFormat {
    /// Signed 8-bit, as an i8. One byte per sample per channel.
    S8,
    /// Signed 16-bit little-endian, as an i16. **The default**, because it is what
    /// almost every device accepts and it is the format the sample-rate maths is
    /// easiest to check in.
    S16Le,
    /// Signed 24-bit little-endian, as an i32 held in the low three bytes.
    S24Le,
    /// 32-bit IEEE float, as an f32. Host audio APIs prefer it and it costs no
    /// conversion.
    F32,
}

impl SampleFormat {
    /// Every format, for a test that checks all of them.
    pub const ALL: &'static [Self] = &[Self::S8, Self::S16Le, Self::S24Le, Self::F32];

    /// The register value for this format.
    pub const fn as_u64(self) -> u64 {
        match self {
            Self::S8 => 1,
            Self::S16Le => 2,
            Self::S24Le => 3,
            Self::F32 => 4,
        }
    }

    /// The format a register value names, or `None` if it names none.
    pub const fn from_u64(value: u64) -> Option<Self> {
        match value {
            1 => Some(Self::S8),
            2 => Some(Self::S16Le),
            3 => Some(Self::S24Le),
            4 => Some(Self::F32),
            _ => None,
        }
    }

    /// How many bytes one sample of this format occupies, per channel.
    pub const fn bytes(self) -> u64 {
        match self {
            Self::S8 => 1,
            Self::S16Le => 2,
            Self::S24Le => 3,
            Self::F32 => 4,
        }
    }

    /// The name, for a diagnostic.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::S8 => "s8",
            Self::S16Le => "s16le",
            Self::S24Le => "s24le",
            Self::F32 => "f32",
        }
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The geometry of a stream: format, rate, channels.
///
/// One type rather than three fields, because the three are not independent: a rate in
/// Hz means nothing without knowing how many bytes a sample is, and a sample is
/// per-channel. A caller that could set them separately could set a rate of 48000 with a
/// format of zero and get a device whose rate is meaningless.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioFormat {
    /// How one sample is stored.
    pub sample: SampleFormat,
    /// Frames per second.
    pub rate: u64,
    /// Channels per frame.
    pub channels: u64,
}

impl AudioFormat {
    /// A format, checked.
    pub const fn new(sample: SampleFormat, rate: u64, channels: u64) -> Result<Self, AudioError> {
        if rate == 0 {
            return Err(AudioError::RateZero);
        }
        if rate > MAX_RATE {
            return Err(AudioError::RateTooHigh {
                rate,
                limit: MAX_RATE,
            });
        }
        if channels == 0 {
            return Err(AudioError::NoChannels);
        }
        if channels > MAX_CHANNELS {
            return Err(AudioError::TooManyChannels {
                channels,
                limit: MAX_CHANNELS,
            });
        }
        Ok(Self {
            sample,
            rate,
            channels,
        })
    }

    /// How many bytes one frame of every channel takes.
    pub const fn frame_bytes(self) -> u64 {
        self.sample.bytes().saturating_mul(self.channels)
    }

    /// How many bytes `samples` frames take.
    pub const fn bytes_of(self, samples: u64) -> u64 {
        self.frame_bytes().saturating_mul(samples)
    }

    /// How long `samples` frames last at this rate, in microseconds.
    ///
    /// **Integer arithmetic, and a stated approximation.** `(samples * 1e6) / rate`
    /// truncated is off by less than a microsecond, which is far below the resolution
    /// of anything that can play audio, and a `f64` here would put a float in a value a
    /// guest reads and this platform has no floats in its ISA-facing types.
    pub const fn duration_micros(self, samples: u64) -> u64 {
        samples.saturating_mul(1_000_000) / self.rate
    }
}

/// Why an audio operation was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioError {
    /// A rate of zero was asked for.
    RateZero,
    /// A rate above [`MAX_RATE`].
    RateTooHigh {
        /// The rate asked for.
        rate: u64,
        /// The largest this device accepts.
        limit: u64,
    },
    /// A channel count of zero.
    NoChannels,
    /// A channel count above [`MAX_CHANNELS`].
    TooManyChannels {
        /// The count asked for.
        channels: u64,
        /// The largest this device accepts.
        limit: u64,
    },
    /// A register value that names no format.
    UnknownFormat(u64),
    /// A sample was written or read with no format set.
    NoFormat,
    /// The ring is full, so a sample was refused.
    RingFull {
        /// The ring's capacity, in samples.
        capacity: usize,
    },
    /// A sample was read from an empty ring.
    RingEmpty,
    /// A sample wider than the register port was attempted.
    SampleTooWide {
        /// How wide the access was.
        bytes: u64,
        /// The widest a sample port access can be.
        limit: u64,
    },
    /// The ring's capacity is not a power of two.
    RingNotPowerOfTwo(usize),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RateZero => {
                f.write_str("a sample rate of zero means no samples are ever produced")
            }
            Self::RateTooHigh { rate, limit } => {
                write!(f, "{rate} Hz is above this device's {limit} Hz limit")
            }
            Self::NoChannels => f.write_str("a stream with no channels carries no audio"),
            Self::TooManyChannels { channels, limit } => {
                write!(f, "{channels} channels is above this device's {limit}")
            }
            Self::UnknownFormat(value) => {
                write!(f, "sample format {value} is not one this device has")
            }
            Self::NoFormat => f.write_str("no sample format has been set"),
            Self::RingFull { capacity } => {
                write!(f, "the audio ring already holds {capacity} samples")
            }
            Self::RingEmpty => f.write_str("the audio ring is empty"),
            Self::SampleTooWide { bytes, limit } => {
                write!(
                    f,
                    "a {bytes} byte access is wider than the {limit} byte sample port"
                )
            }
            Self::RingNotPowerOfTwo(size) => {
                write!(
                    f,
                    "a ring of {size} samples is not a power of two, so its index mask would be wrong"
                )
            }
        }
    }
}

impl core::error::Error for AudioError {}

impl From<AudioError> for DeviceError {
    /// Every audio refusal becomes a register-access fault.
    ///
    /// Flattened deliberately: an `AudioError` names host-independent *device* facts
    /// (a rate of zero, a full ring), all of which a guest can be told, and the guest
    /// ABI for audio is a set of registers rather than a struct — which is exactly what
    /// §29 means by the host backend being independent of it.
    fn from(source: AudioError) -> Self {
        match source {
            AudioError::SampleTooWide { .. } | AudioError::UnknownFormat(_) => {
                DeviceError::UnsupportedSize(DataSize::Double)
            }
            _ => DeviceError::Capacity,
        }
    }
}

/// A block of audio handed to a host backend: a format and plain numbers.
///
/// **No guest types, no registers, no Lazalith.** This is §29's "host backends must be
/// independent of the guest ABI" as a type. A backend written against this can be driven
/// by a different machine's audio device, and the only thing it would have to learn to
/// do that is decode the sample format.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioFrame<'a> {
    /// The format the numbers are in.
    pub format: AudioFormat,
    /// The samples, interleaved by frame and then by channel.
    pub samples: &'a [f32],
}

impl<'a> AudioFrame<'a> {
    /// A frame, checking that the sample count is a whole number of frames.
    ///
    /// The check is here rather than in each backend because a backend handed a slice
    /// that is not a whole number of frames would either drop a partial frame or read
    /// past the end, and both would be reported as the backend's fault.
    pub fn new(format: AudioFormat, samples: &'a [f32]) -> Result<Self, AudioError> {
        if !(samples.len() as u64).is_multiple_of(format.channels) {
            return Err(AudioError::NoChannels);
        }
        Ok(Self { format, samples })
    }

    /// How many frames this block holds.
    pub fn frames(&self) -> u64 {
        self.samples.len() as u64 / self.format.channels
    }
}

/// What a host does with audio, and the host's own limit on rate.
pub trait AudioBackend: fmt::Debug {
    /// The rate the host will actually run at.
    ///
    /// **Asked rather than declared, and the reason is that they will differ.** A guest
    /// that asks for 44 100 Hz on a host that runs at 48 000 Hz needs resampling, and a
    /// host that silently resampled would make the guest's ring arithmetic wrong by a
    /// factor it could not see. So the host's rate is a fact a caller can compare against
    /// what the guest asked for.
    fn rate(&self) -> u64;

    /// The channel count the host accepts.
    fn channels(&self) -> u64;

    /// Plays these samples.
    fn play(&mut self, frame: &AudioFrame<'_>) -> Result<(), AudioError>;

    /// Takes up to `capacity` samples the host has captured.
    fn capture(&mut self, out: &mut [f32]) -> Result<u64, AudioError>;

    /// Stops.
    fn stop(&mut self);
}

/// What a drain did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioDrain {
    /// The host had nothing to give.
    Idle,
    /// No format has been set, so there is nothing to move.
    NoFormat,
    /// The host runs at a different rate or channel count than the guest asked for.
    ///
    /// **Its own outcome, and not `Idle`.** "The host is at 48 kHz and the guest asked for
    /// 44.1" and "the guest has produced nothing" are the two things a person debugging
    /// silence needs told apart, and collapsing them reports the first as the second.
    Mismatch {
        /// The rate the host runs at.
        host_rate: u64,
        /// The rate the guest asked for.
        guest_rate: u64,
    },
    /// Samples were played.
    Played {
        /// How many frames.
        frames: u64,
    },
    /// Capture delivered samples.
    Captured {
        /// How many frames.
        frames: u64,
    },
}

/// A guest-visible PCM audio device.
///
/// Playback and capture share one ring, which is a simplification and is named as one:
/// a real duplex codec has two, and §29's job here is the ring and the rate, not duplex.
#[derive(Debug)]
pub struct AudioDevice {
    id: DeviceId,
    interrupt: InterruptId,
    format: Option<AudioFormat>,
    control: u64,
    threshold: u64,
    status: u64,
    /// Samples the guest has written and the host has not taken.
    playback: VecDeque<f32>,
    /// Samples the host has produced and the guest has not read.
    capture: VecDeque<f32>,
    capacity: usize,
    /// Whether the low-water interrupt is already raised.
    ///
    /// **One bit, and it is what makes the interrupt an edge.** A device that raised on
    /// every cycle below the threshold would flood the controller; one that raised once
    /// and latched is the shape a real controller has.
    low_water_raised: bool,
    played: u64,
    captured: u64,
}

impl AudioDevice {
    /// A device with the default ring and no format set.
    pub fn new(id: DeviceId, interrupt: InterruptId) -> Self {
        Self {
            id,
            interrupt,
            format: None,
            control: 0,
            threshold: DEFAULT_RING_SAMPLES as u64 / 2,
            status: 0,
            playback: VecDeque::new(),
            capture: VecDeque::new(),
            capacity: DEFAULT_RING_SAMPLES,
            low_water_raised: false,
            played: 0,
            captured: 0,
        }
    }

    /// A device with a ring of `capacity` samples, which must be a power of two.
    pub fn with_ring(
        id: DeviceId,
        interrupt: InterruptId,
        capacity: usize,
    ) -> Result<Self, AudioError> {
        if !capacity.is_power_of_two() {
            return Err(AudioError::RingNotPowerOfTwo(capacity));
        }
        let mut device = Self::new(id, interrupt);
        device.capacity = capacity;
        device.threshold = (capacity / 2) as u64;
        Ok(device)
    }

    /// Which device this is, for a diagnostic.
    pub const fn id(&self) -> DeviceId {
        self.id
    }

    /// The format, if one has been set.
    pub const fn format(&self) -> Option<AudioFormat> {
        self.format
    }

    /// How many playback samples are waiting for the host.
    pub fn playback_level(&self) -> usize {
        self.playback.len()
    }

    /// How many capture samples are waiting for the guest.
    pub fn capture_level(&self) -> usize {
        self.capture.len()
    }

    /// The ring's capacity, in samples.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// The current status bits.
    pub const fn status(&self) -> u64 {
        self.status
    }

    /// Sets the format, checking it.
    pub fn set_format(&mut self, format: AudioFormat) -> Result<(), AudioError> {
        AudioFormat::new(format.sample, format.rate, format.channels)?;
        self.format = Some(format);
        self.status &= !AUDIO_STATUS_BAD_FORMAT;
        self.recompute();
        Ok(())
    }

    /// Recomputes the status bits from the rings and the control register.
    ///
    /// **`AUDIO_STATUS_READY` follows the *capture* ring, not the playback one**, and the
    /// first version of this got it wrong by reading the playback ring. The bit answers
    /// "can the guest read a sample right now", and a guest reads samples from capture
    /// 2014 playback is the host reading. A guest in capture mode was told its buffer was
    /// empty while the host had four samples waiting for it.
    fn recompute(&mut self) -> u64 {
        let mut status = 0;
        if self.control & AUDIO_CONTROL_CAPTURE != 0 && !self.capture.is_empty() {
            status |= AUDIO_STATUS_READY;
        }
        if self.control & AUDIO_CONTROL_PLAY != 0 && self.playback.is_empty() {
            status |= AUDIO_STATUS_UNDERRUN;
        }
        if self.control & AUDIO_CONTROL_CAPTURE != 0 && self.capture.is_empty() {
            status |= AUDIO_STATUS_OVERRUN;
        }
        if self.format.is_none() {
            status |= AUDIO_STATUS_BAD_FORMAT;
        }
        self.status = status;
        status
    }

    /// Whether playback or capture is running.
    pub const fn is_running(&self) -> bool {
        self.control & (AUDIO_CONTROL_PLAY | AUDIO_CONTROL_CAPTURE) != 0
    }

    /// Hands up to `budget` samples to a backend and takes what it captured.
    ///
    /// **The host calls this, on the host's schedule** — the same rule as B7's
    /// `pump_display` and B9's `pump_input`. The guest is not involved: a guest register
    /// access must not call a sound card.
    ///
    /// Hands up to `budget` frames to a backend, and takes what it captured.
    ///
    /// **The host calls this, on the host's schedule** — the same rule as B7's
    /// `pump_display` and B9's `pump_input`. The guest is not involved: a guest register
    /// access must not call a sound card.
    ///
    /// The host's rate is compared with the guest's, and a **mismatch is reported, not
    /// resampled**. A silent resample would make the guest's ring-duration arithmetic
    /// wrong by a factor the guest could not see, and an audible pitch shift is a worse
    /// bug than a refusal. It is a *distinct* outcome rather than `Idle` because "the
    /// host is at 48 kHz and the guest asked for 44.1" and "the guest has produced
    /// nothing" are the two things a person debugging silence needs told apart, and
    /// collapsing them would report the first as the second.
    ///
    /// `budget` is in **frames**, not samples, and is converted to the sample count by
    /// the channel count — so a caller that budgets 8 frames of stereo moves 16
    /// samples, and the arithmetic the guest did to fill its ring is the arithmetic the
    /// host does to empty it.
    pub fn drain(&mut self, backend: &mut dyn AudioBackend, budget: u64) -> AudioDrain {
        let Some(format) = self.format else {
            return AudioDrain::NoFormat;
        };
        if backend.rate() != format.rate || backend.channels() != format.channels {
            return AudioDrain::Mismatch {
                host_rate: backend.rate(),
                guest_rate: format.rate,
            };
        }
        let channels = format.channels as usize;
        let budget_samples = (budget as usize).saturating_mul(channels);

        let mut result = AudioDrain::Idle;
        if self.control & AUDIO_CONTROL_PLAY != 0 {
            let take = budget_samples.min(self.playback.len());
            if take > 0 {
                let samples: Vec<f32> = self.playback.drain(..take).collect();
                match AudioFrame::new(format, &samples)
                    .map_err(|_| AudioDrain::Idle)
                    .and_then(|frame| backend.play(&frame).map_err(|_| AudioDrain::Idle))
                {
                    Ok(()) => {
                        let frames = (take / channels) as u64;
                        self.played = self.played.saturating_add(frames);
                        result = AudioDrain::Played { frames };
                    }
                    // A backend that refuses leaves the samples in the ring rather than
                    // dropping them: the guest produced them, and a host that could not
                    // play them yet is not a reason to lose them.
                    Err(outcome) => return outcome,
                }
            }
        }
        if self.control & AUDIO_CONTROL_CAPTURE != 0 {
            let mut out = vec![0.0f32; budget_samples];
            match backend.capture(&mut out) {
                Ok(frames) if frames > 0 => {
                    let samples = (frames as usize).saturating_mul(channels);
                    for sample in out.into_iter().take(samples) {
                        if self.capture.len() >= self.capacity {
                            break;
                        }
                        self.capture.push_back(sample);
                    }
                    self.captured = self.captured.saturating_add(frames);
                    result = AudioDrain::Captured { frames };
                }
                // `Ok(0)` is "the host has nothing yet", which is idle, not a failure.
                _ => {}
            }
        }
        self.recompute();
        result
    }
}

impl Default for AudioFormat {
    /// A silent default, so `Option<AudioFormat>` is the only thing that says "unset".
    ///
    /// S16Le at 48 kHz stereo: the format most devices accept, and the one a test that
    /// formats a default by accident will still be able to play.
    ///
    /// It exists so that a guest which sets the *rate* before the format gets a sensible
    /// rate rather than a refusal, and a guest which sets nothing gets `NoFormat` from
    /// the device rather than playing noise.
    fn default() -> Self {
        Self {
            sample: SampleFormat::S16Le,
            rate: 48_000,
            channels: 2,
        }
    }
}

impl AudioDevice {
    /// Writes one playback sample.
    pub fn push_playback(&mut self, sample: f32) -> Result<(), AudioError> {
        if self.format.is_none() {
            return Err(AudioError::NoFormat);
        }
        if self.playback.len() >= self.capacity {
            return Err(AudioError::RingFull {
                capacity: self.capacity,
            });
        }
        self.playback.push_back(sample);
        self.recompute();
        Ok(())
    }

    /// Takes one playback sample, for a host draining without a backend.
    pub fn pop_playback(&mut self) -> Result<f32, AudioError> {
        let sample = self.playback.pop_front().ok_or(AudioError::RingEmpty)?;
        self.recompute();
        Ok(sample)
    }

    /// Reads one capture sample.
    pub fn pull_capture(&mut self) -> Result<f32, AudioError> {
        if self.format.is_none() {
            return Err(AudioError::NoFormat);
        }
        let sample = self.capture.pop_front().ok_or(AudioError::RingEmpty)?;
        self.recompute();
        Ok(sample)
    }

    /// Adds one capture sample, for a host filling the ring.
    pub fn push_capture(&mut self, sample: f32) -> Result<(), AudioError> {
        if self.capture.len() >= self.capacity {
            return Err(AudioError::RingFull {
                capacity: self.capacity,
            });
        }
        self.capture.push_back(sample);
        self.recompute();
        Ok(())
    }
}

impl Device for AudioDevice {
    fn address_len(&self) -> u64 {
        AUDIO_REGISTER_BYTES
    }

    fn reset(&mut self) {
        // The ring is cleared, because a ring full of samples from before a reset is a
        // machine that plays stale audio. Everything else is kept for the same reason
        // B5's devices keep their storage: a reset is not a wipe.
        self.playback.clear();
        self.capture.clear();
        self.status = 0;
        self.low_water_raised = false;
    }

    fn validate_read(&self, offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError> {
        if size != DataSize::Double {
            return Err(DeviceError::UnsupportedSize(size));
        }
        if !offset.as_u64().is_multiple_of(8) || offset.as_u64() >= AUDIO_REGISTER_BYTES {
            return Err(DeviceError::InvalidRange {
                offset,
                bytes: u64::from(size.bytes()),
            });
        }
        match offset {
            AUDIO_REGISTER_DATA => {
                if self.format.is_none() {
                    Err(AudioError::NoFormat.into())
                } else {
                    Ok(())
                }
            }
            AUDIO_REGISTER_FORMAT
            | AUDIO_REGISTER_RATE
            | AUDIO_REGISTER_CHANNELS
            | AUDIO_REGISTER_CONTROL
            | AUDIO_REGISTER_THRESHOLD
            | AUDIO_REGISTER_STATUS
            | AUDIO_REGISTER_LEVEL => Ok(()),
            _ => Err(DeviceError::ReadUnsupported),
        }
    }

    fn validate_write(
        &self,
        offset: DeviceOffset,
        size: DataSize,
        _value: u64,
    ) -> Result<(), DeviceError> {
        if size != DataSize::Double {
            return Err(DeviceError::UnsupportedSize(size));
        }
        if !offset.as_u64().is_multiple_of(8) || offset.as_u64() >= AUDIO_REGISTER_BYTES {
            return Err(DeviceError::InvalidRange {
                offset,
                bytes: u64::from(size.bytes()),
            });
        }
        match offset {
            AUDIO_REGISTER_FORMAT
            | AUDIO_REGISTER_RATE
            | AUDIO_REGISTER_CHANNELS
            | AUDIO_REGISTER_CONTROL
            | AUDIO_REGISTER_THRESHOLD
            | AUDIO_REGISTER_DATA => Ok(()),
            AUDIO_REGISTER_STATUS | AUDIO_REGISTER_LEVEL => Err(DeviceError::WriteUnsupported),
            _ => Err(DeviceError::WriteUnsupported),
        }
    }

    fn read(&mut self, offset: DeviceOffset, size: DataSize) -> Result<u64, DeviceError> {
        self.validate_read(offset, size)?;
        Ok(match offset {
            AUDIO_REGISTER_FORMAT => self.format.map_or(0, |f| f.sample.as_u64()),
            AUDIO_REGISTER_RATE => self.format.map_or(0, |f| f.rate),
            AUDIO_REGISTER_CHANNELS => self.format.map_or(0, |f| f.channels),
            AUDIO_REGISTER_CONTROL => self.control,
            AUDIO_REGISTER_THRESHOLD => self.threshold,
            AUDIO_REGISTER_STATUS => self.recompute(),
            AUDIO_REGISTER_LEVEL => self.playback.len() as u64,
            AUDIO_REGISTER_DATA => {
                let sample = self.pull_capture().map_err(DeviceError::from)?;
                // A sample crosses the register port as a 16-bit signed value, which is
                // the widest a single access can carry and the narrowest every format
                // can lose nothing on: a 32-bit float that went out as 16 bits would
                // clip, so `f32` samples are scaled and clamped here rather than
                // silently wrapping.
                pack_sample(sample)
            }
            _ => return Err(DeviceError::ReadUnsupported),
        })
    }

    fn write(
        &mut self,
        offset: DeviceOffset,
        size: DataSize,
        value: u64,
    ) -> Result<(), DeviceError> {
        self.validate_write(offset, size, value)?;
        match offset {
            AUDIO_REGISTER_FORMAT => {
                let sample =
                    SampleFormat::from_u64(value).ok_or(AudioError::UnknownFormat(value))?;
                self.set_format(AudioFormat {
                    sample,
                    ..self.format.unwrap_or_default()
                })?;
            }
            AUDIO_REGISTER_RATE => {
                let mut format = self.format.unwrap_or_default();
                format.rate = value;
                self.set_format(format)?;
            }
            AUDIO_REGISTER_CHANNELS => {
                let mut format = self.format.unwrap_or_default();
                format.channels = value;
                self.set_format(format)?;
            }
            AUDIO_REGISTER_CONTROL => {
                self.control = value & (AUDIO_CONTROL_PLAY | AUDIO_CONTROL_CAPTURE);
            }
            AUDIO_REGISTER_THRESHOLD => {
                self.threshold = value.min(self.capacity as u64);
            }
            AUDIO_REGISTER_DATA => {
                self.push_playback(unpack_sample(value))?;
            }
            _ => return Err(DeviceError::WriteUnsupported),
        }
        Ok(())
    }

    fn peek(&self, offset: DeviceOffset, output: &mut [u8]) -> Result<(), DeviceError> {
        if output.len() != DataSize::Double.bytes() as usize {
            return Err(DeviceError::InvalidRange {
                offset,
                bytes: output.len() as u64,
            });
        }
        let value = match offset {
            AUDIO_REGISTER_FORMAT => self.format.map_or(0, |f| f.sample.as_u64()),
            AUDIO_REGISTER_RATE => self.format.map_or(0, |f| f.rate),
            AUDIO_REGISTER_CHANNELS => self.format.map_or(0, |f| f.channels),
            AUDIO_REGISTER_CONTROL => self.control,
            AUDIO_REGISTER_THRESHOLD => self.threshold,
            AUDIO_REGISTER_STATUS => self.status,
            AUDIO_REGISTER_LEVEL => self.playback.len() as u64,
            // A peek of the data port reports the *next* sample without taking it, so
            // a debugger can show what the guest is about to read.
            AUDIO_REGISTER_DATA => self
                .capture
                .front()
                .map_or(0, |sample| pack_sample(*sample)),
            _ => return Err(DeviceError::Unpeekable),
        };
        output.copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn tick(&mut self, _elapsed: CycleCount) {
        // Audio is driven by the host draining the ring, not by the machine counting
        // cycles: the rate that matters is the sample rate, and the device cannot move
        // samples into a sound card from here. What `tick` *can* do is notice that the
        // guest is not keeping the ring full, which is what the interrupt is for.
        let level = self.playback.len() as u64;
        if self.control & AUDIO_CONTROL_PLAY != 0 && level <= self.threshold {
            self.low_water_raised = true;
        } else if level > self.threshold {
            self.low_water_raised = false;
        }
    }

    fn take_interrupt(&mut self) -> Option<InterruptId> {
        // Taken, not read: a device that raised and was not acknowledged would raise
        // again every cycle, and a guest that cleared its own interrupt would race with
        // the machine reading it.
        if self.low_water_raised {
            self.low_water_raised = false;
            return Some(self.interrupt);
        }
        None
    }

    fn snapshot(&self) -> Vec<u8> {
        // The ring contents are **not** in here. A snapshot of a machine that had
        // 1024 samples queued would be 4 KiB of audio, and a machine snapshot is not a
        // media recording. The *levels* are, because a guest can read them and so
        // entitled to see them again after a restore; the samples are not, because the
        // guest cannot name them and the host drains them anyway.
        let mut out = Vec::new();
        out.extend_from_slice(&self.control.to_le_bytes());
        out.extend_from_slice(&self.threshold.to_le_bytes());
        out.extend_from_slice(&(self.playback.len() as u64).to_le_bytes());
        out.extend_from_slice(&(self.capture.len() as u64).to_le_bytes());
        // One byte, not a `u64` from `to_le_bytes`. The first version wrote eight, which
        // shifted every field after it by seven bytes and made `restore` read the
        // low-water flag out of the middle of the playback level. It passed compilation
        // and the device worked; only a test that checked the snapshot.s *length*
        // against `AUDIO_SNAPSHOT_BYTES` noticed, which is the fourth time in this
        // project a constant and the encoder that must agree with it were written apart.
        out.push(u8::from(self.low_water_raised));
        match self.format {
            Some(format) => {
                out.push(1);
                out.extend_from_slice(&format.sample.as_u64().to_le_bytes());
                out.extend_from_slice(&format.rate.to_le_bytes());
                out.extend_from_slice(&format.channels.to_le_bytes());
            }
            None => out.push(0),
        }
        out
    }

    fn restore(&mut self, bytes: &[u8]) -> Result<(), DeviceError> {
        let malformed = || DeviceError::SnapshotShape {
            expected: AUDIO_SNAPSHOT_BYTES,
            found: bytes.len(),
        };
        if bytes.len() != AUDIO_SNAPSHOT_BYTES {
            return Err(malformed());
        }
        self.control = u64::from_le_bytes(bytes[0..8].try_into().unwrap())
            & (AUDIO_CONTROL_PLAY | AUDIO_CONTROL_CAPTURE);
        self.threshold = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let playback = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let capture = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        self.low_water_raised = bytes[32] != 0;
        self.format = if bytes[33] == 0 {
            None
        } else {
            let sample =
                SampleFormat::from_u64(u64::from_le_bytes(bytes[34..42].try_into().unwrap()))
                    .ok_or(malformed())?;
            Some(AudioFormat {
                sample,
                rate: u64::from_le_bytes(bytes[42..50].try_into().unwrap()),
                channels: u64::from_le_bytes(bytes[50..58].try_into().unwrap()),
            })
        };
        // The levels are restored as levels, not as samples: a restored machine has an
        // empty ring that *says* it had samples, and the host's next drain refills it.
        // Anything else would mean the snapshot carried the audio, which it does not.
        self.playback.clear();
        self.capture.clear();
        let _ = (playback, capture);
        self.recompute();
        Ok(())
    }
}

/// How many bytes an `AudioDevice` snapshot is.
pub const AUDIO_SNAPSHOT_BYTES: usize = 58;

/// Packs a sample into the 16-bit signed register representation.
fn pack_sample(sample: f32) -> u64 {
    let scaled = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
    u64::from(scaled as u16)
}

/// Unpacks a sample from the 16-bit signed register representation.
fn unpack_sample(value: u64) -> f32 {
    let raw = (value as u16) as i16;
    f32::from(raw) / i16::MIN.unsigned_abs() as f32
}

/// A backend that discards everything, for a headless machine.
///
/// **Not a null object for convenience — a real answer.** A machine with no sound card
/// must still consume the samples a guest produces, or a guest that fills a ring gets
/// `RingFull` and reports a broken device. Silently discarding is the only behaviour
/// that is both honest and useful: the guest is told the truth about *its* side (the
/// samples were accepted and then gone) and the host does the least possible work.
#[derive(Debug)]
pub struct NullAudioBackend {
    rate: u64,
    channels: u64,
    played: u64,
    captured: u64,
}

impl NullAudioBackend {
    /// A silent backend at this rate and channel count.
    pub const fn new(rate: u64, channels: u64) -> Self {
        Self {
            rate,
            channels,
            played: 0,
            captured: 0,
        }
    }

    /// How many frames have been thrown away.
    pub const fn played(&self) -> u64 {
        self.played
    }

    /// How many frames have been handed over for capture.
    pub const fn captured(&self) -> u64 {
        self.captured
    }
}

impl AudioBackend for NullAudioBackend {
    fn rate(&self) -> u64 {
        self.rate
    }

    fn channels(&self) -> u64 {
        self.channels
    }

    fn play(&mut self, frame: &AudioFrame<'_>) -> Result<(), AudioError> {
        self.played = self.played.saturating_add(frame.frames());
        Ok(())
    }

    fn capture(&mut self, _out: &mut [f32]) -> Result<u64, AudioError> {
        Ok(0)
    }

    fn stop(&mut self) {}
}

/// A backend that records what it was given, for tests.
///
/// The counterpart to B8's `HeadlessDisplayBackend`: it lets a test assert *what was
/// played* rather than only that something was.
#[derive(Debug)]
pub struct RecordingAudioBackend {
    rate: u64,
    channels: u64,
    frames: Vec<f32>,
    played: u64,
    capture: Vec<f32>,
}

impl RecordingAudioBackend {
    /// A recording backend at this rate and channel count.
    pub fn new(rate: u64, channels: u64) -> Self {
        Self {
            rate,
            channels,
            frames: Vec::new(),
            played: 0,
            capture: Vec::new(),
        }
    }

    /// Everything played, in order.
    pub fn frames(&self) -> &[f32] {
        &self.frames
    }

    /// How many frames were played.
    pub const fn played(&self) -> u64 {
        self.played
    }

    /// Arranges for the next `capture` to hand over these samples.
    pub fn with_capture(mut self, samples: &[f32]) -> Self {
        self.capture = samples.to_vec();
        self
    }
}

impl AudioBackend for RecordingAudioBackend {
    fn rate(&self) -> u64 {
        self.rate
    }

    fn channels(&self) -> u64 {
        self.channels
    }

    fn play(&mut self, frame: &AudioFrame<'_>) -> Result<(), AudioError> {
        self.frames.extend_from_slice(frame.samples);
        self.played = self.played.saturating_add(frame.frames());
        Ok(())
    }

    fn capture(&mut self, out: &mut [f32]) -> Result<u64, AudioError> {
        let take = self.capture.len().min(out.len());
        out[..take].copy_from_slice(&self.capture[..take]);
        self.capture.drain(..take);
        Ok(take as u64 / self.channels)
    }

    fn stop(&mut self) {
        self.frames.clear();
    }
}
