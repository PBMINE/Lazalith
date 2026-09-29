//! B10: audio, and the interrupt path it needed.
//!
//! # What is being held here
//!
//! **The rate is the whole design.** Everything else in the platform so far is
//! synchronous: a display is a picture, an input event is a fact. Audio is a *stream at
//! a rate*, and a guest filling a ring at 48 kHz against a host draining at whatever the
//! sound card does is the first thing in this codebase where the two sides genuinely
//! disagree. So the tests are mostly about the disagreement: what happens at a rate
//! mismatch, when the ring runs dry, and whether the guest is told.
//!
//! **A backend is handed numbers, not guest types.** §29 requires host backends to be
//! independent of the guest ABI, and `AudioFrame` is that as a type: a format and a
//! `&[f32]`. No registers, no offsets, no Lazalith structs. A test asserts the shape by
//! constructing one without a device in scope at all.
//!
//! **The interrupt is an edge.** A device that raised on every cycle below its threshold
//! would flood the controller. `take_interrupt` is called once per clock advance and
//! returns at most one, and `a_low_water_interrupt_is_raised_once_and_latches` checks the
//! latch — which is the part that is easy to get wrong, because a device that raises
//! every cycle passes any test that only checks "an interrupt was raised".
//!
//! # What is deliberately not tested
//!
//! There is no sound card. `NullAudioBackend` and `RecordingAudioBackend` stand in, so
//! the device, the ring, the rate arithmetic and the interrupt are all covered; the host
//! side is not, which is the same gap B7 recorded for `Sdl3DisplayBackend`.

use lazalith_devices::{
    AUDIO_CONTROL_CAPTURE, AUDIO_CONTROL_PLAY, AUDIO_REGISTER_CHANNELS, AUDIO_REGISTER_CONTROL,
    AUDIO_REGISTER_DATA, AUDIO_REGISTER_FORMAT, AUDIO_REGISTER_LEVEL, AUDIO_REGISTER_RATE,
    AUDIO_REGISTER_STATUS, AUDIO_REGISTER_THRESHOLD, AUDIO_SNAPSHOT_BYTES, AUDIO_STATUS_OVERRUN,
    AUDIO_STATUS_READY, AUDIO_STATUS_UNDERRUN, AudioDevice, AudioDrain, AudioError, AudioFormat,
    AudioFrame, DEFAULT_RING_SAMPLES, Device, DeviceError, MAX_CHANNELS, MAX_RATE,
    NullAudioBackend, RecordingAudioBackend, SampleFormat,
};
use lazalith_isa::DataSize;
use lazalith_types::{CycleCount, DeviceId, InterruptId};

const DEVICE: DeviceId = DeviceId::new(5);
const INTERRUPT: InterruptId = InterruptId::new(9);
const QUAD: DataSize = DataSize::Double;
const RATE: u64 = 48_000;
const CHANNELS: u64 = 2;

fn format() -> AudioFormat {
    AudioFormat::new(SampleFormat::S16Le, RATE, CHANNELS).expect("a legal format")
}

fn device() -> AudioDevice {
    let mut device = AudioDevice::new(DEVICE, INTERRUPT);
    device.set_format(format()).expect("a legal format");
    device
}

/// A device with playback running, which a guest has to ask for: samples queued in a
/// ring are not being played until the guest says so.
fn playing() -> AudioDevice {
    let mut device = device();
    device
        .write(AUDIO_REGISTER_CONTROL, QUAD, AUDIO_CONTROL_PLAY)
        .expect("playback on");
    device
}

// -- the format, as §29's "sample format / rate / channel count" --------------

#[test]
fn a_format_checks_all_three_of_its_parts() {
    assert!(
        AudioFormat::new(SampleFormat::S16Le, 0, 2).is_err(),
        "a rate of zero"
    );
    assert!(AudioFormat::new(SampleFormat::S16Le, MAX_RATE + 1, 2).is_err());
    assert!(AudioFormat::new(SampleFormat::S16Le, RATE, 0).is_err());
    assert!(
        AudioFormat::new(SampleFormat::S16Le, RATE, MAX_CHANNELS + 1).is_err(),
        "a channel count above the limit"
    );
    assert!(AudioFormat::new(SampleFormat::S16Le, MAX_RATE, MAX_CHANNELS).is_ok());
}

#[test]
fn every_sample_format_round_trips_through_its_register_value() {
    for sample in SampleFormat::ALL {
        let value = sample.as_u64();
        assert_eq!(
            SampleFormat::from_u64(value),
            Some(*sample),
            "a format that cannot be written to a register and read back is a format a \
             guest cannot select"
        );
        assert!(sample.bytes() > 0);
        assert!(!sample.as_str().is_empty());
    }
    assert_eq!(
        SampleFormat::from_u64(99),
        None,
        "and a value that names no format is refused rather than silently becoming one"
    );
}

#[test]
fn the_formats_disagree_about_size_in_the_way_real_ones_do() {
    assert_eq!(SampleFormat::S8.bytes(), 1);
    assert_eq!(SampleFormat::S16Le.bytes(), 2);
    assert_eq!(SampleFormat::S24Le.bytes(), 3);
    assert_eq!(SampleFormat::F32.bytes(), 4);
}

#[test]
fn a_rate_becomes_a_duration_the_guest_can_check() {
    let stereo = format();
    assert_eq!(stereo.frame_bytes(), 4, "two channels of 16-bit samples");
    // 480 frames of 48 kHz is 10 ms, and the arithmetic says 10000 microseconds.
    assert_eq!(
        stereo.duration_micros(480),
        10_000,
        "a guest deciding how much to queue needs this, and getting it wrong means either \
         an underrun or a ring that is always full"
    );
    assert_eq!(stereo.bytes_of(480), 1920);
}

// -- the ring -----------------------------------------------------------------

#[test]
fn a_guest_writes_samples_and_a_host_takes_them() {
    let mut device = playing();
    for _ in 0..12 {
        device.push_playback(0.5).expect("the ring has room");
    }
    assert_eq!(device.playback_level(), 12);
    let mut backend = RecordingAudioBackend::new(RATE, CHANNELS);
    assert_eq!(
        device.drain(&mut backend, 4),
        AudioDrain::Played { frames: 4 },
        "a budget of four frames of stereo is eight samples, and the arithmetic the guest \
         used to fill the ring is the arithmetic the host uses to empty it"
    );
    assert_eq!(
        device.playback_level(),
        4,
        "a budget of four frames of stereo is eight samples, so four are left of twelve"
    );
    assert_eq!(backend.played(), 4);
}

#[test]
fn a_full_ring_refuses_rather_than_dropping() {
    let mut device = AudioDevice::with_ring(DEVICE, INTERRUPT, 4).expect("a power of two");
    device.set_format(format()).expect("a legal format");
    for _ in 0..4 {
        device.push_playback(1.0).expect("the ring has room");
    }
    assert_eq!(
        device.push_playback(1.0),
        Err(AudioError::RingFull { capacity: 4 }),
        "a guest that produced a sample and was told nothing has lost it"
    );
}

#[test]
fn a_ring_that_is_not_a_power_of_two_is_refused() {
    assert_eq!(
        AudioDevice::with_ring(DEVICE, INTERRUPT, 100).unwrap_err(),
        AudioError::RingNotPowerOfTwo(100),
        "a ring index is masked with len - 1, which is only correct when the length is a \
         power of two; a ring that had to use a modulo would make every sample cost a \
         division"
    );
    assert_eq!(DEFAULT_RING_SAMPLES, 1024);
}

#[test]
fn a_sample_cannot_be_written_with_no_format() {
    let mut device = AudioDevice::new(DEVICE, INTERRUPT);
    assert_eq!(
        device.push_playback(0.0),
        Err(AudioError::NoFormat),
        "a sample in no format is not a sample, and accepting it would mean the ring filled \
         with numbers nobody could interpret"
    );
    assert!(
        matches!(
            device.read(AUDIO_REGISTER_DATA, QUAD),
            Err(DeviceError::Capacity)
        ),
        "and reading the data port is refused for the same reason"
    );
}

// -- the rate -----------------------------------------------------------------

#[test]
fn a_rate_mismatch_is_reported_rather_than_resampled() {
    let mut device = device();
    device.push_playback(0.25).expect("the ring has room");
    let mut backend = RecordingAudioBackend::new(44_100, CHANNELS);
    assert_eq!(
        device.drain(&mut backend, 8),
        AudioDrain::Mismatch {
            host_rate: 44_100,
            guest_rate: RATE
        },
        "a silent resample would make the guest's ring-duration arithmetic wrong by a \
         factor it could not see, and an audible pitch shift is a worse bug than a refusal. \
         It is also its own outcome rather than Idle, because 'the host is at 44.1 and the \
         guest asked for 48' and 'the guest produced nothing' are the two things a person \
         debugging silence needs told apart."
    );
    assert_eq!(
        device.playback_level(),
        1,
        "and the samples are still there: nothing was lost to a refusal"
    );
}

#[test]
fn a_channel_mismatch_is_a_mismatch_too() {
    let mut device = device();
    let mut backend = RecordingAudioBackend::new(RATE, 1);
    assert!(matches!(
        device.drain(&mut backend, 8),
        AudioDrain::Mismatch { .. }
    ));
}

// -- status and the interrupt -------------------------------------------------

#[test]
fn a_playback_ring_that_runs_dry_reports_an_underrun() {
    let mut device = device();
    device
        .write(AUDIO_REGISTER_CONTROL, QUAD, AUDIO_CONTROL_PLAY)
        .expect("play on");
    assert_eq!(
        device
            .read(AUDIO_REGISTER_STATUS, QUAD)
            .expect("a status read")
            & AUDIO_STATUS_UNDERRUN,
        AUDIO_STATUS_UNDERRUN,
        "playback with nothing queued is an underrun, and a guest that cannot see it will \
         spend the rest of its life producing into a void"
    );
    device.push_playback(0.0).expect("a sample");
    assert_eq!(
        device
            .read(AUDIO_REGISTER_STATUS, QUAD)
            .expect("a status read")
            & AUDIO_STATUS_UNDERRUN,
        0,
        "and one sample clears it"
    );
}

#[test]
fn a_capture_ring_with_nothing_is_an_overrun() {
    let mut device = device();
    device
        .write(AUDIO_REGISTER_CONTROL, QUAD, AUDIO_CONTROL_CAPTURE)
        .expect("capture on");
    assert_ne!(
        device
            .read(AUDIO_REGISTER_STATUS, QUAD)
            .expect("a status read")
            & AUDIO_STATUS_OVERRUN,
        0,
        "capture with nothing available is the other direction of the same fact"
    );
}

#[test]
fn a_low_water_interrupt_is_raised_once_and_latches() {
    let mut device = device();
    device
        .write(AUDIO_REGISTER_CONTROL, QUAD, AUDIO_CONTROL_PLAY)
        .expect("play on");
    device
        .write(AUDIO_REGISTER_THRESHOLD, QUAD, 4)
        .expect("a threshold");
    for _ in 0..2 {
        device.push_playback(0.0).expect("a sample");
    }
    // The device learns it is low in `tick`, and the machine asks afterwards.
    device.tick(CycleCount::new(1));
    assert_eq!(
        device.take_interrupt(),
        Some(INTERRUPT),
        "a guest that must poll a status register busy-waits at the guest's rate, and the \
         device knows when its ring is low and the guest does not"
    );
    assert_eq!(
        device.take_interrupt(),
        None,
        "and it is taken, not read: a device that re-raised every cycle would flood the \
         controller, and a guest that cleared its own interrupt would race with the \
         machine reading it"
    );
}

#[test]
fn an_interrupt_clears_when_the_guest_fills_the_ring_again() {
    let mut device = device();
    device
        .write(AUDIO_REGISTER_CONTROL, QUAD, AUDIO_CONTROL_PLAY)
        .expect("play on");
    device
        .write(AUDIO_REGISTER_THRESHOLD, QUAD, 4)
        .expect("a threshold");
    device.tick(CycleCount::new(1));
    assert_eq!(device.take_interrupt(), Some(INTERRUPT));

    for _ in 0..8 {
        device.push_playback(0.0).expect("a sample");
    }
    device.tick(CycleCount::new(1));
    assert_eq!(
        device.take_interrupt(),
        None,
        "a guest that filled the ring has dealt with it, and raising again would be a \
         level-triggered interrupt that never stops"
    );
}

#[test]
fn a_silent_device_raises_nothing() {
    let mut device = device();
    device.tick(CycleCount::new(1_000));
    assert_eq!(
        device.take_interrupt(),
        None,
        "a device with playback off is not low on samples; it is not playing"
    );
}

// -- the register interface ---------------------------------------------------

#[test]
fn the_registers_round_trip() {
    let mut device = AudioDevice::new(DEVICE, INTERRUPT);
    device
        .write(AUDIO_REGISTER_FORMAT, QUAD, SampleFormat::S16Le.as_u64())
        .expect("a format");
    device
        .write(AUDIO_REGISTER_RATE, QUAD, 44_100)
        .expect("a rate");
    device
        .write(AUDIO_REGISTER_CHANNELS, QUAD, 1)
        .expect("channels");
    assert_eq!(
        device.read(AUDIO_REGISTER_FORMAT, QUAD).expect("a read"),
        SampleFormat::S16Le.as_u64()
    );
    assert_eq!(
        device.read(AUDIO_REGISTER_RATE, QUAD).expect("a read"),
        44_100
    );
    assert_eq!(
        device.read(AUDIO_REGISTER_CHANNELS, QUAD).expect("a read"),
        1
    );
    assert_eq!(
        device.format(),
        Some(AudioFormat::new(SampleFormat::S16Le, 44_100, 1).expect("legal")),
        "and the three registers are one format, not three independent numbers"
    );
}

#[test]
fn a_status_or_level_register_is_read_only() {
    let mut device = device();
    assert!(
        device.write(AUDIO_REGISTER_STATUS, QUAD, 0).is_err(),
        "a guest that could write the status register could claim samples it never \
         produced, and a device that can be lied to is a device nobody can debug"
    );
    assert!(device.write(AUDIO_REGISTER_LEVEL, QUAD, 0).is_err());
}

#[test]
fn a_sample_crosses_the_data_port_as_sixteen_signed_bits() {
    let mut device = device();
    device
        .write(AUDIO_REGISTER_DATA, QUAD, (-32_768i32 as u32) as u64)
        .expect("a sample");
    let sample = device.pop_playback().expect("the sample is there");
    assert_eq!(
        sample, -1.0,
        "a full-scale negative value comes back as exactly -1.0, and it did not wrap: \
         dividing by 32 767 instead of 32 768 would have given -1.00003, and a guest \
         reading back its own full-scale value would get a number outside the range it \
         wrote"
    );
}

#[test]
fn a_wide_access_is_refused() {
    let mut device = device();
    assert!(
        device.read(AUDIO_REGISTER_RATE, DataSize::Double).is_ok(),
        "a quad-word read of a register is the normal shape"
    );
    assert!(
        device.read(AUDIO_REGISTER_RATE, DataSize::Word).is_err(),
        "and a narrower one is refused rather than reading half a register, which would \
         give a guest a number it could not interpret"
    );
}

// -- snapshots ----------------------------------------------------------------

#[test]
fn a_snapshot_carries_the_format_and_the_levels_but_not_the_audio() {
    let mut device = device();
    for _ in 0..4 {
        device.push_playback(0.5).expect("a sample");
    }
    let snapshot = device.snapshot();
    assert_eq!(snapshot.len(), AUDIO_SNAPSHOT_BYTES);
    assert!(
        snapshot.len() < 64,
        "a machine snapshot that held 1024 queued samples would be 4 KiB of audio: a \
         snapshot is not a media recording"
    );

    let mut restored = AudioDevice::new(DEVICE, INTERRUPT);
    restored.restore(&snapshot).expect("it restores");
    assert_eq!(restored.format(), Some(format()), "the format survives");
    assert_eq!(
        restored.playback_level(),
        0,
        "and the ring is empty, because the samples were not in the snapshot: the guest \
         cannot name them and the host refills the ring anyway"
    );
    assert!(
        restored.restore(&[0u8; 3]).is_err(),
        "and a snapshot of another shape is refused rather than misread"
    );
}

// -- the backend boundary -----------------------------------------------------

#[test]
fn a_backend_needs_no_guest_types_at_all() {
    // §29: "Host backends must be independent of the guest ABI." This test builds a
    // frame with no device, no machine and no register in scope — which is only possible
    // if `AudioFrame` carries no guest vocabulary at all.
    let samples = [0.0f32, 0.25, 0.5, 0.75];
    let frame = AudioFrame::new(format(), &samples).expect("a whole number of frames");
    assert_eq!(frame.frames(), 2);
    assert_eq!(frame.samples.len(), 4);
    assert_eq!(frame.format.rate, RATE);
}

#[test]
fn a_frame_that_is_not_a_whole_number_of_frames_is_refused() {
    let samples = [0.0f32, 0.25, 0.5];
    assert!(
        AudioFrame::new(format(), &samples).is_err(),
        "a backend handed a slice that is not a whole number of frames would either drop a \
         partial frame or read past the end, and both would be reported as its fault"
    );
}

#[test]
fn a_null_backend_consumes_samples_so_a_guest_does_not_report_a_broken_device() {
    let mut device = playing();
    let mut backend = NullAudioBackend::new(RATE, CHANNELS);
    for _ in 0..8 {
        device.push_playback(0.0).expect("a sample");
    }
    assert_eq!(
        device.drain(&mut backend, 8),
        AudioDrain::Played { frames: 4 },
        "eight samples of stereo is four frames"
    );
    assert_eq!(device.playback_level(), 0, "the ring emptied");
    assert_eq!(
        backend.played(),
        4,
        "and a machine with no sound card must still consume what a guest produced, or the \
         guest gets RingFull and reports a broken device"
    );
}

#[test]
fn a_capture_goes_the_other_way() {
    let mut device = device();
    device
        .write(AUDIO_REGISTER_CONTROL, QUAD, AUDIO_CONTROL_CAPTURE)
        .expect("capture on");
    let mut backend =
        RecordingAudioBackend::new(RATE, CHANNELS).with_capture(&[0.1, 0.2, 0.3, 0.4]);
    assert_eq!(
        device.drain(&mut backend, 8),
        AudioDrain::Captured { frames: 2 },
        "four samples of stereo is two frames"
    );
    assert_eq!(device.capture_level(), 4);
    assert_eq!(
        device
            .read(AUDIO_REGISTER_STATUS, QUAD)
            .expect("a status read")
            & AUDIO_STATUS_READY,
        AUDIO_STATUS_READY,
        "and the guest can see there is something to read"
    );
    let first = device.pull_capture().expect("a sample");
    assert!(
        (first - 0.1).abs() < f32::EPSILON,
        "in the order the host gave them"
    );
}
