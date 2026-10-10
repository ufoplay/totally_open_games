//! Port of `internal/synth_queue.h`: the single-producer single-consumer event queue between the engine (game
//! thread) and a synthesizer (audio thread). Payloads are copied; the native AdLib path queues generated OPL
//! register writes instead of MIDI events.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Events per queue.
#[doc(alias = "IM_SYNTH_QUEUE_SIZE")]
pub(crate) const QUEUE_SIZE: usize = 4096;

/// One queued synthesizer event.
#[doc(alias = "ImSynthEvent")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SynthEvent {
    #[doc(alias = "SYNTH_PROGRAM")]
    Program { channel: u8, program: u8 },
    #[doc(alias = "SYNTH_ON")]
    NoteOn { channel: u8, note: u8, velocity: u8 },
    #[doc(alias = "SYNTH_OFF")]
    NoteOff { channel: u8, note: u8 },
    #[doc(alias = "SYNTH_CONTROL")]
    Control {
        channel: u8,
        controller: u8,
        value: u8,
    },
    #[doc(alias = "SYNTH_BEND")]
    PitchBend { channel: u8, value: u16 },
    #[doc(alias = "SYNTH_OPL_WRITE")]
    OplWrite { register: u8, value: u8 },
}

/// Flags shared by both ends of a queue.
#[derive(Debug, Default)]
pub(crate) struct QueueShared {
    /// The producer overflowed. The FM4 and FluidSynth renderers reset their synth and clear it; the AdLib renderer
    /// stays silent, as upstream.
    pub overflow: AtomicBool,
    pub overflow_count: AtomicU32,
    pub reset_count: AtomicU32,
}

/// The game-thread end.
#[derive(Debug)]
pub(crate) struct QueueProducer {
    pub producer: rtrb::Producer<SynthEvent>,
    pub shared: Arc<QueueShared>,
}

/// The audio-thread end.
#[derive(Debug)]
pub(crate) struct QueueConsumer {
    pub consumer: rtrb::Consumer<SynthEvent>,
    pub shared: Arc<QueueShared>,
}

pub(crate) fn queue() -> (QueueProducer, QueueConsumer) {
    let (producer, consumer) = rtrb::RingBuffer::new(QUEUE_SIZE);
    let shared = Arc::new(QueueShared::default());
    (
        QueueProducer {
            producer,
            shared: Arc::clone(&shared),
        },
        QueueConsumer { consumer, shared },
    )
}

impl QueueProducer {
    /// Enqueues an event. Returns `false` when the queue is full; nothing is queued while an overflow is pending.
    #[doc(alias = "synth_enqueue")]
    pub fn push(&mut self, event: SynthEvent) -> bool {
        if self.shared.overflow.load(Ordering::Acquire) {
            return true;
        }
        if self.producer.push(event).is_err() {
            self.shared.overflow.store(true, Ordering::Release);
            self.shared.overflow_count.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    pub fn overflowed(&self) -> bool {
        self.shared.overflow.load(Ordering::Acquire)
    }
}

impl QueueConsumer {
    #[doc(alias = "synth_pop")]
    pub fn pop(&mut self) -> Option<SynthEvent> {
        self.consumer.pop().ok()
    }

    /// Discards everything queued and clears a pending overflow.
    pub fn reset(&mut self) {
        let available = self.consumer.slots();
        if let Ok(chunk) = self.consumer.read_chunk(available) {
            chunk.commit_all();
        }
        self.shared.reset_count.fetch_add(1, Ordering::Relaxed);
        self.shared.overflow.store(false, Ordering::Release);
    }

    pub fn overflowed(&self) -> bool {
        self.shared.overflow.load(Ordering::Acquire)
    }
}
