//! Media keys and the system media session ("what is playing").

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Win32::System::Com::COINIT_MULTITHREADED;

use super::input::SendInputDevice;
use super::with_com;
use crate::sys::{Input, Media, MediaAction, Track};

pub struct WinMedia;

fn media_key(action: MediaAction) -> u16 {
    match action {
        MediaAction::Next => 0xB0,
        MediaAction::Previous => 0xB1,
        MediaAction::Stop => 0xB2,
        MediaAction::PlayPause => 0xB3,
    }
}

/// `Spotify.exe` or `Yandex.Music_abc!App` as a short program name.
fn app_name(id: &str) -> String {
    let head = id.split('!').next().unwrap_or(id);
    let head = head.split('_').next().unwrap_or(head);
    head.trim_end_matches(".exe")
        .trim_end_matches(".EXE")
        .to_string()
}

fn current() -> windows::core::Result<Option<Track>> {
    let manager = SessionManager::RequestAsync()?.join()?;
    // No session: the call fails with a null result.
    let Ok(session) = manager.GetCurrentSession() else {
        return Ok(None);
    };
    let props = session.TryGetMediaPropertiesAsync()?.join()?;
    let title = props.Title()?.to_string();
    if title.trim().is_empty() {
        return Ok(None);
    }
    let playing = session
        .GetPlaybackInfo()
        .and_then(|i| i.PlaybackStatus())
        .is_ok_and(|s| s == PlaybackStatus::Playing);
    Ok(Some(Track {
        title,
        artist: props.Artist().map(|a| a.to_string()).unwrap_or_default(),
        app: session
            .SourceAppUserModelId()
            .map(|a| app_name(&a.to_string()))
            .unwrap_or_default(),
        playing,
    }))
}

impl Media for WinMedia {
    fn control(&self, action: MediaAction) -> Result<(), String> {
        SendInputDevice.chord(&[media_key(action)])
    }

    fn now_playing(&self) -> Result<Option<Track>, String> {
        with_com(COINIT_MULTITHREADED, || {
            current().map_err(|e| format!("Windows не отдал сведения о плеере: {}", e.message()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_name_is_short() {
        assert_eq!(app_name("Spotify.exe"), "Spotify");
        assert_eq!(app_name("Yandex.Music_abc123!App"), "Yandex.Music");
        assert_eq!(app_name("chrome"), "chrome");
    }
}
