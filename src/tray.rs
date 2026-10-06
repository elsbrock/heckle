//! StatusNotifierItem tray icon: enable/disable, auto mode, narrate now.

use ksni::TrayMethods;
use ksni::menu::{CheckmarkItem, StandardItem};
use tokio::sync::mpsc::UnboundedSender;

use crate::ipc::{Cmd, Set, Status};

pub struct Tray {
    tx: UnboundedSender<Cmd>,
    pub status: Status,
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "narrator".into()
    }

    fn title(&self) -> String {
        "narrator".into()
    }

    fn icon_name(&self) -> String {
        if self.status.enabled {
            "audio-volume-high"
        } else {
            "audio-volume-muted"
        }
        .into()
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            CheckmarkItem {
                label: "Enabled".into(),
                checked: self.status.enabled,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Cmd::Enabled(Set::Toggle));
                }),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Auto narrate".into(),
                checked: self.status.auto,
                enabled: self.status.enabled,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Cmd::Auto(Set::Toggle));
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Narrate now".into(),
                enabled: self.status.enabled,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Cmd::Poke(None));
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Stop speaking".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Cmd::Stop);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Cmd::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Show the tray icon; `None` if no StatusNotifier host is available.
pub async fn spawn(tx: UnboundedSender<Cmd>, status: Status) -> Option<ksni::Handle<Tray>> {
    match (Tray { tx, status }).spawn().await {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::warn!("tray unavailable: {e}");
            None
        }
    }
}
