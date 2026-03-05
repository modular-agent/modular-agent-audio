use base64::{Engine, engine::general_purpose::STANDARD};
use modular_agent_core::{
    Agent, AgentContext, AgentData, AgentError, AgentSpec, AgentValue, AsAgent,
    ModularAgent, async_trait, modular_agent,
};
use rodio::{Decoder, OutputStream, Sink};
use std::io::Cursor;
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread::JoinHandle;

const CATEGORY: &str = "Audio";

const PORT_AUDIO: &str = "audio";

const CONFIG_VOLUME: &str = "volume";
const CONFIG_INTERRUPT: &str = "interrupt";

enum AudioCommand {
    Play(Vec<u8>),
    SetVolume(f32),
    Clear,
    Shutdown,
}

/// Plays audio data through system speakers.
///
/// Accepts data URI strings (e.g. `data:audio/wav;base64,...`),
/// decodes and plays them through the default audio output device.
/// Multiple audio inputs are queued and played sequentially.
#[modular_agent(
    title = "Audio Player",
    category = CATEGORY,
    inputs = [PORT_AUDIO],
    outputs = [],
    number_config(name = CONFIG_VOLUME, default = 1.0, description = "Playback volume (0.0-1.0)"),
    boolean_config(name = CONFIG_INTERRUPT, description = "Interrupt current playback when new audio arrives"),
    hint(color = 5, width = 1, height = 1),
)]
struct AudioPlayerAgent {
    data: AgentData,
    sender: Mutex<Option<mpsc::Sender<AudioCommand>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

fn audio_thread(rx: mpsc::Receiver<AudioCommand>, initial_volume: f32) {
    let (_stream, handle) = match OutputStream::try_default() {
        Ok(v) => v,
        Err(e) => {
            log::error!("No audio output device available: {}", e);
            return;
        }
    };
    let sink = match Sink::try_new(&handle) {
        Ok(v) => v,
        Err(e) => {
            log::error!("Failed to create audio sink: {}", e);
            return;
        }
    };
    sink.set_volume(initial_volume);

    while let Ok(cmd) = rx.recv() {
        match cmd {
            AudioCommand::Play(bytes) => match Decoder::new(Cursor::new(bytes)) {
                Ok(source) => sink.append(source),
                Err(e) => log::error!("Audio decode error: {}", e),
            },
            AudioCommand::SetVolume(v) => sink.set_volume(v),
            AudioCommand::Clear => sink.clear(),
            AudioCommand::Shutdown => break,
        }
    }
}

/// Parse a data URI and return the decoded bytes.
/// Accepts format: `data:<mimetype>;base64,<data>`
fn decode_data_uri(data_uri: &str) -> Result<Vec<u8>, AgentError> {
    let base64_data = data_uri
        .strip_prefix("data:")
        .and_then(|s| s.split_once(";base64,"))
        .map(|(_, data)| data)
        .ok_or_else(|| {
            AgentError::InvalidValue("Expected data URI format: data:<mime>;base64,<data>".into())
        })?;

    STANDARD
        .decode(base64_data)
        .map_err(|e| AgentError::InvalidValue(format!("Failed to decode base64 audio data: {}", e)))
}

#[async_trait]
impl AsAgent for AudioPlayerAgent {
    fn new(ma: ModularAgent, id: String, spec: AgentSpec) -> Result<Self, AgentError> {
        Ok(Self {
            data: AgentData::new(ma, id, spec),
            sender: Mutex::new(None),
            thread: Mutex::new(None),
        })
    }

    async fn start(&mut self) -> Result<(), AgentError> {
        let volume = self
            .configs()
            .ok()
            .map(|c| c.get_number_or(CONFIG_VOLUME, 1.0))
            .unwrap_or(1.0);
        let volume = volume.clamp(0.0, 1.0) as f32;

        let (tx, rx) = mpsc::channel();

        let handle = std::thread::Builder::new()
            .name("audio-player".into())
            .spawn(move || audio_thread(rx, volume))
            .map_err(|e| AgentError::IoError(format!("Failed to spawn audio thread: {}", e)))?;

        *self.sender.lock().unwrap() = Some(tx);
        *self.thread.lock().unwrap() = Some(handle);

        Ok(())
    }

    async fn stop(&mut self) -> Result<(), AgentError> {
        let tx = self.sender.lock().unwrap().take();
        if let Some(tx) = tx {
            let _ = tx.send(AudioCommand::Shutdown);
        }
        let handle = self.thread.lock().unwrap().take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        Ok(())
    }

    fn configs_changed(&mut self) -> Result<(), AgentError> {
        let config = self.configs()?;
        let volume = config.get_number_or(CONFIG_VOLUME, 1.0).clamp(0.0, 1.0) as f32;
        let guard = self.sender.lock().unwrap();
        if let Some(ref tx) = *guard {
            let _ = tx.send(AudioCommand::SetVolume(volume));
        }
        Ok(())
    }

    async fn process(
        &mut self,
        _ctx: AgentContext,
        _port: String,
        value: AgentValue,
    ) -> Result<(), AgentError> {
        let data_uri = value.as_str().ok_or_else(|| {
            AgentError::InvalidValue("Input must be a string containing a data URI".into())
        })?;

        if data_uri.is_empty() {
            return Err(AgentError::InvalidValue("Input data URI is empty".into()));
        }

        let audio_bytes = decode_data_uri(data_uri)?;

        let config = self.configs()?;
        let interrupt = config.get_bool_or(CONFIG_INTERRUPT, false);

        {
            let guard = self.sender.lock().expect("audio sender lock poisoned");
            if let Some(ref tx) = *guard {
                if interrupt {
                    let _ = tx.send(AudioCommand::Clear);
                }
                if tx.send(AudioCommand::Play(audio_bytes)).is_err() {
                    log::error!("Audio thread is not running");
                }
            } else {
                log::error!("Audio player not started");
            }
        }

        Ok(())
    }
}
