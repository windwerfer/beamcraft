//! Presenter remote: taps keys (Right, Left, Space, Escape) through the xdg-desktop-portal
//! RemoteDesktop interface. No uinput, no root (decisions D1); the portal asks the user once
//! (the restore token is kept so the dialog does not return) and works inside the Flatpak
//! sandbox. The session opens lazily on the first tap, so a presenter that nobody uses as a
//! remote never shows the dialog.

use anyhow::{bail, Context, Result};
use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::{PersistMode, Session};
use mirrorwire_proto::control::RemoteKey;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Receives one human-readable line whenever the remote's state changes (shown in the
/// presenter window, AGENTS.md §1.8).
pub type RemoteStatusSink = Arc<dyn Fn(String) + Send + Sync>;

/// Start the injector task on the current tokio runtime. Send taps into the returned sender.
pub fn spawn_remote_injector(
    restore_token_path: PathBuf,
    status: RemoteStatusSink,
) -> mpsc::Sender<RemoteKey> {
    let (tx, mut rx) = mpsc::channel::<RemoteKey>(16);
    status("presenter remote: waiting for the first button press".into());
    tokio::spawn(async move {
        let mut session: Option<(RemoteDesktop, Session<RemoteDesktop>)> = None;
        while let Some(key) = rx.recv().await {
            if session.is_none() {
                status("presenter remote: asking the desktop for permission…".into());
                match open_session(&restore_token_path).await {
                    Ok(s) => {
                        status("presenter remote: ready (taps Right/Left/Space/Esc)".into());
                        session = Some(s);
                    }
                    Err(e) => {
                        tracing::warn!("presenter remote: {e:#}");
                        status(format!(
                            "presenter remote unavailable: {e:#}. Press a remote button again to retry."
                        ));
                        continue;
                    }
                }
            }
            let Some((proxy, sess)) = &session else {
                continue;
            };
            if let Err(e) = tap(proxy, sess, key).await {
                tracing::warn!("presenter remote tap failed: {e:#}");
                status(format!(
                    "presenter remote: key press failed ({e:#}); reconnecting"
                ));
                session = None;
            }
        }
    });
    tx
}

async fn open_session(token_path: &PathBuf) -> Result<(RemoteDesktop, Session<RemoteDesktop>)> {
    let proxy = RemoteDesktop::new()
        .await
        .context("the desktop has no RemoteDesktop portal")?;
    let available = proxy.available_device_types().await.unwrap_or_default();
    if !available.contains(DeviceType::KEYBOARD) {
        bail!("the RemoteDesktop portal offers no keyboard");
    }
    let session = proxy.create_session(Default::default()).await?;
    let token = tokio::fs::read_to_string(token_path)
        .await
        .ok()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    proxy
        .select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(DeviceType::KEYBOARD)
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(token.as_deref()),
        )
        .await?
        .response()?;
    let started = proxy
        .start(&session, None, Default::default())
        .await?
        .response()
        .context("keyboard access was cancelled or denied")?;
    if !started.devices().contains(DeviceType::KEYBOARD) {
        bail!("keyboard access was not granted");
    }
    if let Some(t) = started.restore_token() {
        if let Some(dir) = token_path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        tokio::fs::write(token_path, t).await?;
    }
    Ok((proxy, session))
}

async fn tap(
    proxy: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    key: RemoteKey,
) -> Result<()> {
    let sym = key.keysym();
    proxy
        .notify_keyboard_keysym(session, sym, KeyState::Pressed, Default::default())
        .await?;
    proxy
        .notify_keyboard_keysym(session, sym, KeyState::Released, Default::default())
        .await?;
    Ok(())
}
