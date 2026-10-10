//! Port of the audio-thread half of `midi_xwing_adlib.c`: the OPL chip fed with the driver's register writes.

use super::opl3::Chip;
use crate::synth_queue::{QueueConsumer, SynthEvent};

pub(crate) struct AdlibRenderer {
    queue: QueueConsumer,
    chip: Chip,
}

impl AdlibRenderer {
    /// The chip is reset at the output rate, as `adlib_open` does.
    pub fn new(queue: QueueConsumer, sample_rate: u32) -> Self {
        Self {
            queue,
            chip: Chip::new(sample_rate),
        }
    }

    /// Overwrites `out` with interleaved stereo frames. After a queue overflow the output stays silent.
    #[doc(alias = "adlib_render")]
    pub fn render_float(&mut self, out: &mut [f32]) {
        if self.queue.overflowed() {
            out.fill(0.0);
            return;
        }
        for frame in out.as_chunks_mut::<2>().0 {
            while self.chip.write_space() != 0 {
                let Some(SynthEvent::OplWrite { register, value }) = self.queue.pop() else {
                    break;
                };
                self.chip.write_reg_buffered(u16::from(register), value);
            }
            let mut sample = [0i16; 2];
            self.chip.generate_resampled(&mut sample);
            frame[0] = f32::from(sample[0]) / 32768.0;
            frame[1] = f32::from(sample[1]) / 32768.0;
        }
    }
}
