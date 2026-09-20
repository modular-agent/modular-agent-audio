use cpal::traits::{DeviceTrait, HostTrait};
use modular_agent_core::{
    AsModule, Error, ModularAgent, ModuleContext, ModuleData, ModuleOutput, ModuleSpec, Result,
    Value, async_trait, im, modular_agent,
};

const CATEGORY: &str = "Audio";

const PORT_UNIT: &str = "unit";
const PORT_DEVICES: &str = "devices";

/// Builds a user-facing device name that stays unique across same-named endpoints.
///
/// cpal 0.17 splits the Windows friendly name into `name` (e.g. "マイク") and
/// `driver` (e.g. "Wireless Microphone RX"), so several devices can share a
/// bare `name`. Recombining them matches what the OS sound settings show.
pub(crate) fn display_name(desc: &cpal::DeviceDescription) -> String {
    match desc.driver() {
        Some(driver) if driver != desc.name() => format!("{} ({})", desc.name(), driver),
        _ => desc.name().to_string(),
    }
}

const KIND_INPUT: &str = "input";
const KIND_LOOPBACK: &str = "loopback";

fn device_entry(device: &cpal::Device, kind: &str) -> Option<Value> {
    let id = device.id().ok()?;
    let desc = device.description().ok()?;
    Some(Value::object(im::hashmap! {
        "id".into() => Value::string(id.to_string()),
        "name".into() => Value::string(display_name(&desc)),
        "kind".into() => Value::string(kind),
    }))
}

/// Lists available audio capture devices.
///
/// Receives any value as a trigger and outputs an array of objects
/// with `id` (unique device identifier), `name` (human-readable name),
/// and `kind` (`"input"` or `"loopback"`). On Windows, output devices are
/// included as `"loopback"` entries: passing their `id` to Mic Transcribe
/// captures whatever is being played through them (WASAPI loopback).
#[modular_agent(
    title = "Audio Device List",
    category = CATEGORY,
    inputs = [PORT_UNIT],
    outputs = [PORT_DEVICES],
    hint(color = 5),
)]
struct AudioDeviceListModule {
    data: ModuleData,
}

#[async_trait]
impl AsModule for AudioDeviceListModule {
    fn new(ma: ModularAgent, id: String, spec: ModuleSpec) -> Result<Self> {
        Ok(Self {
            data: ModuleData::new(ma, id, spec),
        })
    }

    async fn process(&mut self, ctx: ModuleContext, _port: String, _value: Value) -> Result<()> {
        let host = cpal::default_host();
        let devices = host
            .input_devices()
            .map_err(|e| Error::IoError(format!("Failed to enumerate input devices: {}", e)))?;

        let mut device_list: im::Vector<Value> = devices
            .filter_map(|d| device_entry(&d, KIND_INPUT))
            .collect();

        // WASAPI is the only cpal backend that opens a render endpoint as a
        // loopback input, so output devices are only useful on Windows.
        if cfg!(target_os = "windows") {
            let outputs = host.output_devices().map_err(|e| {
                Error::IoError(format!("Failed to enumerate output devices: {}", e))
            })?;
            device_list.extend(outputs.filter_map(|d| device_entry(&d, KIND_LOOPBACK)));
        }

        self.output(ctx, PORT_DEVICES, Value::array(device_list))
            .await
    }
}
