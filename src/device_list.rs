use cpal::traits::{DeviceTrait, HostTrait};
use modular_agent_core::{
    AgentContext, AgentData, AgentError, AgentOutput, AgentSpec, AgentValue, AsAgent, ModularAgent,
    async_trait, im, modular_agent,
};

const CATEGORY: &str = "Audio";

const PORT_UNIT: &str = "unit";
const PORT_DEVICES: &str = "devices";

/// Lists available audio input devices.
///
/// Receives any value as a trigger and outputs an array of objects
/// with `id` (unique device identifier) and `name` (human-readable name).
#[modular_agent(
    title = "Audio Device List",
    category = CATEGORY,
    inputs = [PORT_UNIT],
    outputs = [PORT_DEVICES],
    hint(color = 5),
)]
struct AudioDeviceListAgent {
    data: AgentData,
}

#[async_trait]
impl AsAgent for AudioDeviceListAgent {
    fn new(ma: ModularAgent, id: String, spec: AgentSpec) -> Result<Self, AgentError> {
        Ok(Self {
            data: AgentData::new(ma, id, spec),
        })
    }

    async fn process(
        &mut self,
        ctx: AgentContext,
        _port: String,
        _value: AgentValue,
    ) -> Result<(), AgentError> {
        let host = cpal::default_host();
        let devices = host.input_devices().map_err(|e| {
            AgentError::IoError(format!("Failed to enumerate input devices: {}", e))
        })?;

        let device_list: im::Vector<AgentValue> = devices
            .filter_map(|d| {
                let id = d.id().ok()?;
                let desc = d.description().ok()?;
                Some(AgentValue::object(im::hashmap! {
                    "id".into() => AgentValue::string(id.to_string()),
                    "name".into() => AgentValue::string(desc.name()),
                }))
            })
            .collect();

        self.output(ctx, PORT_DEVICES, AgentValue::array(device_list))
            .await
    }
}
