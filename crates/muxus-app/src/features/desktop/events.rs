//! What the desktop shell tells the app: later launches handed over by a second process
//! (`second-instance`), links the system opens with Muxus (`open-url` on macOS), and requests
//! to bring the window forward. The app subscribes once with [`DesktopEvents::recv`].

use muxus_core::api::CommandLineLaunch;
use muxus_import::launch::{link_launch, parse_command_line_launch};

/// One request for the app (`deliverCommandLineLaunch`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopEvent {
    /// Open something (`muxus:command-line-launch-requested`): show the focused window (or
    /// the one used last, or a new one) and run the request there. Plan it with
    /// [`plan_launch`](super::launch::plan_launch).
    Launch(CommandLineLaunch),
    /// A launch without a target: restore, show and focus the main window.
    Focus,
}

impl DesktopEvent {
    /// A URL the system handed to the running app (macOS `open-url`). Anything that does not
    /// look like a link (`scheme://…`, `ssh:`, `telnet:`) only brings the window forward.
    pub fn from_open_url(url: &str) -> Self {
        link_launch(url).map_or(DesktopEvent::Focus, DesktopEvent::Launch)
    }
}

/// The launch this process was started with (`initialCommandLineLaunch`); `argv` is the whole
/// command line, executable first.
pub fn initial_launch(argv: &[String]) -> Option<CommandLineLaunch> {
    parse_command_line_launch(argv)
}

/// Sends desktop events; cheap to clone, never blocks.
#[derive(Clone, Debug)]
pub struct DesktopEventSender(async_channel::Sender<DesktopEvent>);

impl DesktopEventSender {
    pub fn send(&self, event: DesktopEvent) {
        let _ = self.0.try_send(event);
    }

    /// Nobody listens any more.
    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}

/// The app's end of the event stream. Clones share one queue: every event reaches exactly
/// one of them, so subscribe once.
#[derive(Clone, Debug)]
pub struct DesktopEvents(async_channel::Receiver<DesktopEvent>);

impl DesktopEvents {
    /// The next event (on any executor); `None` once every sender is gone.
    pub async fn recv(&self) -> Option<DesktopEvent> {
        self.0.recv().await.ok()
    }

    pub fn recv_blocking(&self) -> Option<DesktopEvent> {
        self.0.recv_blocking().ok()
    }

    pub fn try_recv(&self) -> Option<DesktopEvent> {
        self.0.try_recv().ok()
    }
}

/// A fresh event stream.
pub fn desktop_events() -> (DesktopEventSender, DesktopEvents) {
    let (sender, receiver) = async_channel::unbounded();
    (DesktopEventSender(sender), DesktopEvents(receiver))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_opened_urls_into_link_launches() {
        assert_eq!(
            DesktopEvent::from_open_url("telnet://router.lab:2323"),
            DesktopEvent::Launch(CommandLineLaunch::Url {
                name: "telnet://router.lab:2323".into()
            })
        );
        // Any scheme://… is handed on, so the window can say which links Muxus opens.
        assert!(matches!(
            DesktopEvent::from_open_url("https://example.com"),
            DesktopEvent::Launch(CommandLineLaunch::Url { .. })
        ));
        assert_eq!(
            DesktopEvent::from_open_url("mailto-ish text"),
            DesktopEvent::Focus
        );
    }

    #[test]
    fn reads_the_initial_launch() {
        let argv: Vec<String> = ["muxus", "--folder", "Lab/Core"]
            .iter()
            .map(|arg| arg.to_string())
            .collect();
        assert_eq!(
            initial_launch(&argv),
            Some(CommandLineLaunch::Folder {
                name: "Lab/Core".into()
            })
        );
        assert_eq!(initial_launch(&["muxus".to_string()]), None);
    }

    #[test]
    fn delivers_events_in_order() {
        let (sender, events) = desktop_events();
        sender.send(DesktopEvent::Focus);
        sender.send(DesktopEvent::from_open_url("ssh://a"));
        assert_eq!(events.try_recv(), Some(DesktopEvent::Focus));
        assert!(matches!(events.try_recv(), Some(DesktopEvent::Launch(_))));
        assert_eq!(events.try_recv(), None);
        drop(events);
        assert!(sender.is_closed());
    }
}
