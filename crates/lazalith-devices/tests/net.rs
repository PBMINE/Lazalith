//! B11: networking.
//!
//! # What is being held here
//!
//! **There is no way to transmit successfully without a host.** That is the whole of
//! B11's design and it is a *type* property, not a convention: `NetworkDevice` has no
//! method that completes a transmit without a `&mut dyn NetworkBackend`, so a device
//! that reported "sent" on its own could not be written. The tests attack that from
//! three sides — the accepted path, the refused path, and the path where the host is
//! unreachable — because a boundary that only holds on the happy path is not a boundary.
//!
//! **A frame is opaque.** §32 does not name a link layer, so `LazFrame` is a length and
//! bytes. An Ethernet-shaped frame would have decided the link layer inside the
//! guest-visible device, where every later decision inherits it. `a_frame_is_opaque`
//! checks that any bytes at all are a valid frame.
//!
//! **Loss is counted, never silent.** A network drops packets; that is what a network
//! does. What is not acceptable is a host that fills a receive queue and discards without
//! saying so, and `a_full_receive_queue_drops_and_counts` is the test for that.

use lazalith_devices::{
    Device, DeviceError, LazFrame, MAX_FRAME_BYTES, MTU, NET_CONTROL_TX_ACTIVE,
    NET_CONTROL_TX_COMMIT, NET_CONTROL_UP, NET_REGISTER_CONTROL, NET_REGISTER_RX_DATA,
    NET_REGISTER_RX_LENGTH, NET_REGISTER_STATUS, NET_REGISTER_TX_DATA, NET_REGISTER_TX_LENGTH,
    NET_SNAPSHOT_BYTES, NET_STATUS_LINK_DOWN, NET_STATUS_RX_READY, NET_STATUS_TX_REFUSED, NetError,
    NetworkDevice, NullNetworkBackend, RX_QUEUE_FRAMES, RecordingNetworkBackend, TX_BUFFER_BYTES,
    TransmitOutcome,
};
use lazalith_isa::DataSize;
use lazalith_types::{CycleCount, DeviceId, InterruptId};

const DEVICE: DeviceId = DeviceId::new(6);
const INTERRUPT: InterruptId = InterruptId::new(11);
const QUAD: DataSize = DataSize::Double;

fn device() -> NetworkDevice {
    NetworkDevice::new(DEVICE, INTERRUPT)
}

fn frame_bytes(tag: u8, length: usize) -> Vec<u8> {
    vec![tag; length]
}

// -- a frame is opaque ---------------------------------------------------------

#[test]
fn a_frame_is_opaque() {
    // §32 does not name a link layer, and neither does this. Any bytes are a frame.
    for bytes in [vec![0xFF; 14], vec![0x00; 60], frame_bytes(0xAB, MTU)] {
        let frame = LazFrame::new(bytes.clone()).expect("a frame");
        assert_eq!(frame.bytes(), &bytes[..]);
        assert_eq!(frame.len(), bytes.len() as u64);
        assert!(!frame.is_empty());
    }
    assert!(
        LazFrame::new(Vec::new()).is_err(),
        "and a frame of no bytes is not a frame"
    );
    assert!(
        LazFrame::new(vec![0; MAX_FRAME_BYTES + 1]).is_err(),
        "while one over the MTU the host will carry is refused: a device that accepted it \
         would be accepting a frame the network will never deliver"
    );
}

// -- transmit, and the three outcomes ------------------------------------------

#[test]
fn a_frame_the_host_accepts_is_a_delivery() {
    let mut device = device();
    let mut backend = RecordingNetworkBackend::new(true);
    device.begin_transmit().expect("a transmit starts");
    device.push_transmit(&frame_bytes(0x01, 64)).expect("bytes");
    assert_eq!(
        device
            .commit_transmit(&mut backend)
            .expect("the host is asked"),
        TransmitOutcome::Accepted { bytes: 64 }
    );
    assert_eq!(device.transmitted(), 1);
    assert_eq!(backend.sent().len(), 1, "and the host really has the frame");
    assert_eq!(backend.sent()[0], frame_bytes(0x01, 64));
}

#[test]
fn a_host_that_declines_is_told_so_rather_than_told_sent() {
    let mut device = device();
    let mut backend = NullNetworkBackend::up();
    device.begin_transmit().expect("a transmit starts");
    device.push_transmit(&frame_bytes(0x02, 32)).expect("bytes");
    assert_eq!(
        device
            .commit_transmit(&mut backend)
            .expect("the host is asked"),
        TransmitOutcome::Refused {
            reason: NetError::HostRefused
        },
        "a machine with no network has nowhere to put the frame, and a guest that \
         believes otherwise will not retransmit it"
    );
    assert_eq!(device.transmitted(), 0, "and nothing counted as delivered");
    assert_ne!(
        device.status() & NET_STATUS_TX_REFUSED,
        0,
        "the guest can see the refusal in the status register rather than having to guess \
         why its queue emptied"
    );
}

#[test]
fn a_link_that_is_down_is_a_refusal_with_a_reason() {
    let mut device = device();
    let mut backend = NullNetworkBackend::new();
    device.begin_transmit().expect("a transmit starts");
    device.push_transmit(&frame_bytes(0x03, 16)).expect("bytes");
    assert_eq!(
        device
            .commit_transmit(&mut backend)
            .expect("the host is asked"),
        TransmitOutcome::Refused {
            reason: NetError::LinkDown
        },
        "link down and host refusing are different facts, and a guest debugging a network \
         needs to tell them apart"
    );
    assert_ne!(device.status() & NET_STATUS_LINK_DOWN, 0);
}

#[test]
fn a_host_that_cannot_be_asked_is_a_drop_rather_than_a_silent_success() {
    let mut device = device();
    let mut backend = RecordingNetworkBackend::new(true).failing(NetError::HostUnavailable);
    device.begin_transmit().expect("a transmit starts");
    device.push_transmit(&frame_bytes(0x04, 16)).expect("bytes");
    assert_eq!(
        device
            .commit_transmit(&mut backend)
            .expect("the outcome is reported"),
        TransmitOutcome::Dropped {
            reason: NetError::HostUnavailable
        },
        "a user-mode backend that could not hand the frame to a real interface did not \
         deliver it, and 'accepted' would be a lie a guest would not retransmit around"
    );
    assert_eq!(device.transmitted(), 0);
}

#[test]
fn an_empty_transmit_is_refused_rather_than_sent_as_nothing() {
    let mut device = device();
    let mut backend = RecordingNetworkBackend::new(true);
    device.begin_transmit().expect("a transmit starts");
    assert_eq!(
        device
            .commit_transmit(&mut backend)
            .expect_err("an empty frame"),
        NetError::EmptyFrame
    );
    assert!(backend.sent().is_empty(), "and the host was never asked");
}

#[test]
fn writing_the_transmit_port_with_no_transmit_is_refused() {
    let mut device = device();
    assert!(
        matches!(
            device.write(NET_REGISTER_TX_DATA, QUAD, 1),
            Err(DeviceError::Capacity)
        ),
        "a byte written into a port with no transmit in progress is a guest bug, and \
         silently buffering it would produce a frame the guest never meant to send"
    );
}

// -- receive ------------------------------------------------------------------

#[test]
fn a_frame_the_host_delivered_reaches_the_guest_a_byte_at_a_time() {
    let mut device = device();
    let payload = frame_bytes(0x55, 4);
    let mut backend = RecordingNetworkBackend::new(true).with_inbound(vec![payload.clone()]);
    assert_eq!(
        device.pump_receive(&mut backend, 8).expect("a pump"),
        1,
        "one frame was waiting"
    );
    assert_ne!(device.status() & NET_STATUS_RX_READY, 0);
    assert_eq!(device.receive_queue_len(), 1);
    assert_eq!(device.receive_length(), 4);

    let mut read = Vec::new();
    while let Ok(byte) = device.pull_receive() {
        read.push(byte);
    }
    assert_eq!(read, payload, "and the bytes came out in order");
    assert_eq!(device.received(), 1, "counted as received");
    assert_eq!(
        device.receive_queue_len(),
        0,
        "and the queue is empty again"
    );
}

#[test]
fn reading_the_receive_port_with_nothing_waiting_is_refused() {
    let mut device = device();
    assert_eq!(device.pull_receive(), Err(NetError::NoReceive));
}

#[test]
fn a_full_receive_queue_drops_and_counts() {
    let mut device = device();
    let inbound: Vec<Vec<u8>> = (0..(RX_QUEUE_FRAMES + 4))
        .map(|index| frame_bytes(index as u8, 8))
        .collect();
    let mut backend = RecordingNetworkBackend::new(true).with_inbound(inbound);

    // The first pump fills the queue to its bound and then finds one more waiting.
    assert_eq!(
        device.pump_receive(&mut backend, 64).expect("a pump"),
        RX_QUEUE_FRAMES as u64
    );
    assert_eq!(device.receive_queue_len(), RX_QUEUE_FRAMES);
    assert_eq!(
        device.dropped(),
        1,
        "one frame did not fit and was lost: the host.s queue is finite and a guest that \
         does not drain cannot stop the network delivering"
    );

    // The second finds it full and reports one more loss, and stops rather than pulling
    // the whole host queue into a void.
    device.pump_receive(&mut backend, 64).expect("a pump");
    assert_eq!(
        device.receive_queue_len(),
        RX_QUEUE_FRAMES,
        "the queue did not grow past its bound"
    );
    assert_eq!(
        device.dropped(),
        2,
        "and the loss is counted exactly: a network drops packets, but a host that fills a \
         queue and discards silently is a host nobody can debug"
    );
}

#[test]
fn a_pump_is_bounded() {
    let mut device = device();
    let inbound: Vec<Vec<u8>> = (0..RX_QUEUE_FRAMES)
        .map(|index| frame_bytes(index as u8, 8))
        .collect();
    let mut backend = RecordingNetworkBackend::new(true).with_inbound(inbound);
    assert_eq!(
        device.pump_receive(&mut backend, 3).expect("a pump"),
        3,
        "a host on a fast link can have more queued than a caller wants to move in one \
         call, and an unbounded drain would let it monopolise a pump"
    );
    assert_eq!(device.receive_queue_len(), 3);
}

// -- the interrupt ------------------------------------------------------------

#[test]
fn a_waiting_frame_raises_an_interrupt_once() {
    let mut device = device();
    let mut backend = RecordingNetworkBackend::new(true).with_inbound(vec![frame_bytes(9, 4)]);
    device.pump_receive(&mut backend, 4).expect("a pump");
    device.tick(CycleCount::new(1));
    assert_eq!(
        device.take_interrupt(),
        Some(INTERRUPT),
        "a guest polling a status register busy-waits at its own rate, and the device \
         knows a frame is waiting and the guest does not"
    );
    assert_eq!(
        device.take_interrupt(),
        None,
        "and it is an edge, not a level"
    );
}

#[test]
fn an_empty_device_raises_nothing() {
    let mut device = device();
    device.tick(CycleCount::new(1_000));
    assert_eq!(device.take_interrupt(), None);
}

// -- the register interface ---------------------------------------------------

#[test]
fn a_transmit_is_assembled_through_the_control_register() {
    let mut device = device();
    let mut backend = RecordingNetworkBackend::new(true);
    device
        .write(
            NET_REGISTER_CONTROL,
            QUAD,
            NET_CONTROL_TX_ACTIVE | NET_CONTROL_UP,
        )
        .expect("start a transmit");
    device
        .write(NET_REGISTER_TX_DATA, QUAD, 0xAB)
        .expect("a word of frame");
    assert_eq!(
        device.read(NET_REGISTER_TX_LENGTH, QUAD).expect("a read"),
        8,
        "and the guest can see how much it has written"
    );
    // The guest sets the commit bit and the *host* pumps: the device never transmits
    // from inside a register write.
    device
        .write(
            NET_REGISTER_CONTROL,
            QUAD,
            NET_CONTROL_UP | NET_CONTROL_TX_COMMIT,
        )
        .expect("commit");
    let outcome = device.pump(&mut backend, 8).expect("a pump");
    assert_eq!(
        outcome.transmit,
        Some(TransmitOutcome::Accepted { bytes: 8 })
    );
    assert_eq!(backend.sent().len(), 1);
    assert_eq!(
        backend.sent()[0],
        vec![0xAB, 0, 0, 0, 0, 0, 0, 0],
        "the port carries the little-endian encoding of the word the guest wrote, not eight \
         copies of its low byte"
    );
}

#[test]
fn a_transmit_bigger_than_the_staging_buffer_is_refused() {
    let mut device = device();
    device.begin_transmit().expect("a transmit starts");
    let chunk = vec![0; 512];
    let mut refused = false;
    for _ in 0..(TX_BUFFER_BYTES / 512 + 1) {
        if device.push_transmit(&chunk).is_err() {
            refused = true;
            break;
        }
    }
    assert!(
        refused,
        "a guest writing past the staging buffer must be told, not silently truncated: a \
         truncated frame that reached the host would be a corrupt packet the guest \
         believes is intact"
    );
}

#[test]
fn the_status_and_length_registers_are_read_only() {
    let mut device = device();
    assert!(device.write(NET_REGISTER_STATUS, QUAD, 0).is_err());
    assert!(device.write(NET_REGISTER_RX_LENGTH, QUAD, 0).is_err());
}

#[test]
fn peeking_the_receive_port_does_not_consume() {
    let mut device = device();
    let mut backend = RecordingNetworkBackend::new(true).with_inbound(vec![frame_bytes(0x7E, 4)]);
    device.pump_receive(&mut backend, 4).expect("a pump");

    let mut peeked = [0u8; 8];
    device
        .peek(NET_REGISTER_RX_DATA, &mut peeked)
        .expect("a peek");
    assert_eq!(u64::from_le_bytes(peeked), 0x7E, "the next byte");
    assert_eq!(device.receive_length(), 4, "and the frame is still whole");
    assert_eq!(
        device.pull_receive().expect("a byte"),
        0x7E,
        "because a peek that consumed would be a hidden mutation"
    );
}

// -- snapshots ----------------------------------------------------------------

#[test]
fn a_snapshot_carries_the_counters_and_not_the_packets() {
    let mut device = device();
    let mut backend = RecordingNetworkBackend::new(true)
        .with_inbound(vec![frame_bytes(1, 64), frame_bytes(2, 64)]);
    device.pump_receive(&mut backend, 4).expect("a pump");
    while device.pull_receive().is_ok() {}

    let snapshot = device.snapshot();
    assert_eq!(snapshot.len(), NET_SNAPSHOT_BYTES);
    assert!(
        snapshot.len() < 128,
        "a machine that had sixteen 1500-byte frames queued would have a 24 KiB snapshot, \
         and a machine snapshot is not a packet capture: {} bytes",
        snapshot.len()
    );

    let mut restored = NetworkDevice::new(DEVICE, INTERRUPT);
    restored.restore(&snapshot).expect("it restores");
    assert_eq!(
        restored.receive_queue_len(),
        0,
        "the queues are empty after a restore: a restored machine must not hand the guest \
         packets that were in flight when the snapshot was taken, because the host may \
         since have moved them"
    );
    assert!(restored.restore(&[0u8; 5]).is_err());
}
