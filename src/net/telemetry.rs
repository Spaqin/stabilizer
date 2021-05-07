///! Stabilizer Telemetry Capabilities
///!
///! # Design
///! Telemetry is reported regularly using an MQTT client. All telemetry is reported in SI units
///! using standard JSON format.
///!
///! In order to report ADC/DAC codes generated during the DSP routines, a telemetry buffer is
///! employed to track the latest codes. Converting these codes to SI units would result in
///! repetitive and unnecessary calculations within the DSP routine, slowing it down and limiting
///! sampling frequency. Instead, the raw codes are stored and the telemetry is generated as
///! required immediately before transmission. This ensures that any slower computation required
///! for unit conversion can be off-loaded to lower priority tasks.
use heapless::{consts, String, Vec};
use minimq::QoS;
use serde::Serialize;

use super::NetworkReference;
use crate::hardware::{
    design_parameters::MQTT_BROKER, AdcSample, AfeGain, DacCode,
};

/// The telemetry client for reporting telemetry data over MQTT.
pub struct TelemetryClient<T: Serialize> {
    mqtt: minimq::MqttClient<minimq::consts::U256, NetworkReference>,
    telemetry_topic: String<consts::U128>,
    _telemetry: core::marker::PhantomData<T>,
}

/// The telemetry buffer is used for storing sample values during execution.
///
/// # Note
/// These values can be converted to SI units immediately before reporting to save processing time.
/// This allows for the DSP process to continually update the values without incurring significant
/// run-time overhead during conversion to SI units.
#[derive(Copy, Clone)]
pub struct TelemetryBuffer {
    /// The latest input sample on ADC0/ADC1.
    pub adcs: [AdcSample; 2],
    /// The latest output code on DAC0/DAC1.
    pub dacs: [DacCode; 2],
    /// The latest digital input states during processing.
    pub digital_inputs: [bool; 2],
}

/// The telemetry structure is data that is ultimately reported as telemetry over MQTT.
///
/// # Note
/// This structure should be generated on-demand by the buffer when required to minimize conversion
/// overhead.
#[derive(Serialize)]
pub struct Telemetry {
    adcs: [f32; 2],
    dacs: [f32; 2],
    digital_inputs: [bool; 2],
}

impl Default for TelemetryBuffer {
    fn default() -> Self {
        Self {
            adcs: [AdcSample(0), AdcSample(0)],
            dacs: [DacCode(0), DacCode(0)],
            digital_inputs: [false, false],
        }
    }
}

impl TelemetryBuffer {
    /// Convert the telemetry buffer to finalized, SI-unit telemetry for reporting.
    ///
    /// # Args
    /// * `afe0` - The current AFE configuration for channel 0.
    /// * `afe1` - The current AFE configuration for channel 1.
    ///
    /// # Returns
    /// The finalized telemetry structure that can be serialized and reported.
    pub fn finalize(self, afe0: AfeGain, afe1: AfeGain) -> Telemetry {
        let in0_volts = Into::<f32>::into(self.adcs[0]) / afe0.as_multiplier();
        let in1_volts = Into::<f32>::into(self.adcs[1]) / afe1.as_multiplier();

        Telemetry {
            adcs: [in0_volts, in1_volts],
            dacs: [self.dacs[0].into(), self.dacs[1].into()],
            digital_inputs: self.digital_inputs,
        }
    }
}

impl<T: Serialize> TelemetryClient<T> {
    /// Construct a new telemetry client.
    ///
    /// # Args
    /// * `stack` - A reference to the (shared) underlying network stack.
    /// * `client_id` - The MQTT client ID of the telemetry client.
    /// * `prefix` - The device prefix to use for MQTT telemetry reporting.
    ///
    /// # Returns
    /// A new telemetry client.
    pub fn new(stack: NetworkReference, client_id: &str, prefix: &str) -> Self {
        let mqtt =
            minimq::MqttClient::new(MQTT_BROKER.into(), client_id, stack)
                .unwrap();

        let mut telemetry_topic: String<consts::U128> = String::from(prefix);
        telemetry_topic.push_str("/telemetry").unwrap();

        Self {
            mqtt,
            telemetry_topic,
            _telemetry: core::marker::PhantomData::default(),
        }
    }

    /// Publish telemetry over MQTT
    ///
    /// # Note
    /// Telemetry is reported in a "best-effort" fashion. Failure to transmit telemetry will cause
    /// it to be silently dropped.
    ///
    /// # Args
    /// * `telemetry` - The telemetry to report
    pub fn publish(&mut self, telemetry: &T) {
        let telemetry: Vec<u8, consts::U256> =
            serde_json_core::to_vec(telemetry).unwrap();
        self.mqtt
            .publish(&self.telemetry_topic, &telemetry, QoS::AtMostOnce, &[])
            .ok();
    }

    /// Update the telemetry client
    ///
    /// # Note
    /// This function is provided to force the underlying MQTT state machine to process incoming
    /// and outgoing messages. Without this, the client will never connect to the broker. This
    /// should be called regularly.
    pub fn update(&mut self) {
        match self.mqtt.poll(|_client, _topic, _message, _properties| {}) {
            Err(minimq::Error::Network(
                smoltcp_nal::NetworkError::NoIpAddress,
            )) => {}

            Err(error) => log::info!("Unexpected error: {:?}", error),
            _ => {}
        }
    }
}
