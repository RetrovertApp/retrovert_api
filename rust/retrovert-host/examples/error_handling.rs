use retrovert_host::loader::LoadError;
use retrovert_host::service::SettingsError;
use retrovert_host::session::{AbiViolation, OwnedSessionError, SessionError};
use retrovert_host::visualization::VisualizationError;

fn load_error(error: &LoadError) -> &'static str {
    match error {
        LoadError::NoEntryPoint => "missing entry point",
        _ => "plugin load failed",
    }
}

fn abi_violation(error: &AbiViolation) -> &'static str {
    match error {
        AbiViolation::ZeroSampleRate => "zero sample rate",
        _ => "plugin violated the ABI",
    }
}

fn session_error(error: &SessionError) -> &'static str {
    match error {
        SessionError::NotPlayback => "not a playback plugin",
        _ => "session failed",
    }
}

fn owned_session_error(error: &OwnedSessionError) -> &'static str {
    match error {
        OwnedSessionError::Session(_) => "session failed",
        OwnedSessionError::Visualization(_) => "visualization failed",
        _ => "owned session failed",
    }
}

fn settings_error(error: &SettingsError) -> &'static str {
    match error {
        SettingsError::NotFound { .. } => "registration not found",
        _ => "settings operation failed",
    }
}

fn visualization_error(error: &VisualizationError) -> &'static str {
    match error {
        VisualizationError::NotPrepared => "visualization not prepared",
        _ => "visualization failed",
    }
}

fn main() {
    let _ = (
        load_error,
        abi_violation,
        session_error,
        owned_session_error,
        settings_error,
        visualization_error,
    );
}
