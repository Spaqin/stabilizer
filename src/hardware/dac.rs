///! Stabilizer DAC management interface
///!
///! # Design
///!
///! Stabilizer DACs are connected to the MCU via a simplex, SPI-compatible interface. Each DAC
///! accepts a 16-bit output code.
///!
///! In order to maximize CPU processing time, the DAC code updates are offloaded to hardware using
///! a timer compare channel, DMA stream, and the DAC SPI interface.
///!
///! The timer comparison channel is configured to generate a DMA request whenever the comparison
///! occurs. Thus, whenever a comparison happens, a single DAC code can be written to the output. By
///! configuring a DMA stream for a number of successive DAC codes, hardware can regularly update
///! the DAC without requiring the CPU.
///!
///! In order to ensure alignment between the ADC sample batches and DAC output code batches, a DAC
///! output batch is always exactly 3 batches after the ADC batch that generated it.
///!
///! The DMA transfer for the DAC output codes utilizes a double-buffer mode to avoid losing any
///! transfer events generated by the timer (for example, when 2 update cycles occur before the DMA
///! transfer completion is handled). In this mode, by the time DMA swaps buffers, there is always a valid buffer in the
///! "next-transfer" double-buffer location for the DMA transfer. Once a transfer completes,
///! software then has exactly one batch duration to fill the next buffer before its
///! transfer begins. If software does not meet this deadline, old data will be repeatedly generated
///! on the output and output will be shifted by one batch.
///!
///! ## Multiple Samples to Single DAC Codes
///!
///! For some applications, it may be desirable to generate a single DAC code from multiple ADC
///! samples. In order to maintain timing characteristics between ADC samples and DAC code outputs,
///! applications are required to generate one DAC code for each ADC sample. To accomodate mapping
///! multiple inputs to a single output, the output code can be repeated a number of times in the
///! output buffer corresponding with the number of input samples that were used to generate it.
///!
///!
///! # Note
///!
///! There is a very small amount of latency between updating the two DACs due to bus matrix
///! priority. As such, one of the DACs will be updated marginally earlier before the other because
///! the DMA requests are generated simultaneously. This can be avoided by providing a known offset
///! to other DMA requests, which can be completed by setting e.g. DAC0's comparison to a
///! counter value of 2 and DAC1's comparison to a counter value of 3. This will have the effect of
///! generating the DAC updates with a known latency of 1 timer tick to each other and prevent the
///! DMAs from racing for the bus. As implemented, the DMA channels utilize natural priority of the
///! DMA channels to arbitrate which transfer occurs first.
///!
///!
///! # Limitations
///!
///! While double-buffered mode is used for DMA to avoid lost DAC-update events, there is no check
///! for re-use of a previously provided DAC output buffer. It is assumed that the DMA request is
///! served promptly after the transfer completes.
use stm32h7xx_hal as hal;

use super::design_parameters::SAMPLE_BUFFER_SIZE;
use super::timers;

use hal::dma::{
    dma::{DMAReq, DmaConfig},
    traits::TargetAddress,
    MemoryToPeripheral, Transfer,
};

// The following global buffers are used for the DAC code DMA transfers. Two buffers are used for
// each transfer in a ping-pong buffer configuration (one is being prepared while the other is being
// processed). Note that the contents of AXI SRAM is uninitialized, so the buffer contents on
// startup are undefined. The dimensions are `ADC_BUF[adc_index][ping_pong_index][sample_index]`.
#[link_section = ".axisram.buffers"]
static mut DAC_BUF: [[[u16; SAMPLE_BUFFER_SIZE]; 3]; 2] =
    [[[0; SAMPLE_BUFFER_SIZE]; 3]; 2];

/// Custom type for referencing DAC output codes.
/// The internal integer is the raw code written to the DAC output register.
#[derive(Copy, Clone)]
pub struct DacCode(pub u16);

impl Into<f32> for DacCode {
    fn into(self) -> f32 {
        // The output voltage is generated by the DAC with an output range of +/- 4.096 V. This
        // signal then passes through a 2.5x gain stage. Note that the DAC operates using unsigned
        // integers, and u16::MAX / 2 is considered zero voltage output. Thus, the dynamic range of
        // the output stage is +/- 10.24 V. At a DAC code of zero, there is an output of -10.24 V,
        // and at a max DAC code, there is an output of (slightly less than) 10.24 V.

        let dac_volts_per_lsb = 4.096 * 2.5 / (1u16 << 15) as f32;

        // Note that the bipolar table is an offset-binary code, but it is much more logical and
        // correct to treat it as a twos-complement value. TO do that, we XOR the most significant
        // bit to convert it.
        (self.0 ^ 0x8000) as i16 as f32 * dac_volts_per_lsb
    }
}

impl From<i16> for DacCode {
    /// Generate a DAC code from a 16-bit signed value.
    ///
    /// # Note
    /// This is provided as a means to convert between the DACs internal 16-bit, unsigned register
    /// and a 2s-complement integer that is convenient when using the DAC in the bipolar
    /// configuration.
    fn from(value: i16) -> Self {
        Self(value as u16 ^ 0x8000)
    }
}

macro_rules! dac_output {
    ($name:ident, $index:literal, $data_stream:ident,
     $spi:ident, $trigger_channel:ident, $dma_req:ident) => {
        /// $spi is used as a type for indicating a DMA transfer into the SPI TX FIFO
        struct $spi {
            spi: hal::spi::Spi<hal::stm32::$spi, hal::spi::Disabled, u16>,
            _channel: timers::tim2::$trigger_channel,
        }

        impl $spi {
            pub fn new(
                _channel: timers::tim2::$trigger_channel,
                spi: hal::spi::Spi<hal::stm32::$spi, hal::spi::Disabled, u16>,
            ) -> Self {
                Self { _channel, spi }
            }

            /// Start the SPI and begin operating in a DMA-driven transfer mode.
            pub fn start_dma(&mut self) {
                // Allow the SPI FIFOs to operate using only DMA data channels.
                self.spi.enable_dma_tx();

                // Enable SPI and start it in infinite transaction mode.
                self.spi.inner().cr1.modify(|_, w| w.spe().set_bit());
                self.spi.inner().cr1.modify(|_, w| w.cstart().started());
            }
        }

        // Note(unsafe): This is safe because the DMA request line is logically owned by this module.
        // Additionally, the SPI is owned by this structure and is known to be configured for u16 word
        // sizes.
        unsafe impl TargetAddress<MemoryToPeripheral> for $spi {
            /// SPI is configured to operate using 16-bit transfer words.
            type MemSize = u16;

            /// SPI DMA requests are generated whenever TIM2 CHx ($dma_req) comparison occurs.
            const REQUEST_LINE: Option<u8> = Some(DMAReq::$dma_req as u8);

            /// Whenever the DMA request occurs, it should write into SPI's TX FIFO.
            fn address(&self) -> usize {
                &self.spi.inner().txdr as *const _ as usize
            }
        }

        /// Represents data associated with DAC.
        pub struct $name {
            next_buffer: Option<&'static mut [u16; SAMPLE_BUFFER_SIZE]>,
            // Note: SPI TX functionality may not be used from this structure to ensure safety with DMA.
            transfer: Transfer<
                hal::dma::dma::$data_stream<hal::stm32::DMA1>,
                $spi,
                MemoryToPeripheral,
                &'static mut [u16; SAMPLE_BUFFER_SIZE],
                hal::dma::DBTransfer,
            >,
        }

        impl $name {
            /// Construct the DAC output channel.
            ///
            /// # Args
            /// * `spi` - The SPI interface used to communicate with the ADC.
            /// * `stream` - The DMA stream used to write DAC codes over SPI.
            /// * `trigger_channel` - The sampling timer output compare channel for update triggers.
            pub fn new(
                spi: hal::spi::Spi<hal::stm32::$spi, hal::spi::Enabled, u16>,
                stream: hal::dma::dma::$data_stream<hal::stm32::DMA1>,
                trigger_channel: timers::tim2::$trigger_channel,
            ) -> Self {
                // Generate DMA events when an output compare of the timer hitting zero (timer roll over)
                // occurs.
                trigger_channel.listen_dma();
                trigger_channel.to_output_compare(4 + $index);

                // The stream constantly writes to the TX FIFO to write new update codes.
                let trigger_config = DmaConfig::default()
                    .memory_increment(true)
                    .double_buffer(true)
                    .peripheral_increment(false);

                // Listen for any potential SPI error signals, which may indicate that we are not generating
                // update codes.
                let mut spi = spi.disable();
                spi.listen(hal::spi::Event::Error);

                // AXISRAM is uninitialized. As such, we manually zero-initialize it here before
                // starting the transfer.
                // Note(unsafe): We currently own all DAC_BUF[index] buffers and are not using them
                // elsewhere, so it is safe to access them here.
                for buf in unsafe { DAC_BUF[$index].iter_mut() } {
                    for byte in buf.iter_mut() {
                        *byte = 0;
                    }
                }

                // Construct the trigger stream to write from memory to the peripheral.
                let transfer: Transfer<_, _, MemoryToPeripheral, _, _> =
                    Transfer::init(
                        stream,
                        $spi::new(trigger_channel, spi),
                        // Note(unsafe): This buffer is only used once and provided for the DMA transfer.
                        unsafe { &mut DAC_BUF[$index][0] },
                        // Note(unsafe): This buffer is only used once and provided for the DMA transfer.
                        unsafe { Some(&mut DAC_BUF[$index][1]) },
                        trigger_config,
                    );

                Self {
                    transfer,
                    // Note(unsafe): This buffer is only used once and provided for the next DMA transfer.
                    next_buffer: unsafe { Some(&mut DAC_BUF[$index][2]) },
                }
            }

            pub fn start(&mut self) {
                self.transfer.start(|spi| spi.start_dma());
            }

            /// Acquire the next output buffer to populate it with DAC codes.
            pub fn acquire_buffer(&mut self) -> &mut [u16; SAMPLE_BUFFER_SIZE] {
                // Note: If a device hangs up, check that this conditional is passing correctly, as
                // there is no time-out checks here in the interest of execution speed.
                while !self.transfer.get_transfer_complete_flag() {}

                let next_buffer = self.next_buffer.take().unwrap();

                // Start the next transfer.
                let (prev_buffer, _, _) =
                    self.transfer.next_transfer(next_buffer).unwrap();

                // .unwrap_none() https://github.com/rust-lang/rust/issues/62633
                self.next_buffer.replace(prev_buffer);

                self.next_buffer.as_mut().unwrap()
            }
        }
    };
}

dac_output!(Dac0Output, 0, Stream6, SPI4, Channel3, TIM2_CH3);
dac_output!(Dac1Output, 1, Stream7, SPI5, Channel4, TIM2_CH4);
