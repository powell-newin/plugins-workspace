// Copyright 2019-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use std::{
    io::{BufWriter, ErrorKind, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
};

#[cfg(feature = "semver")]
use crate::semver_compat::semver_compat_string;
use crate::{SingleInstanceCallback, destroy, manage_instance_lock};
#[cfg(debug_assertions)]
use tauri::RunEvent;
use tauri::{
    AppHandle, Config, Manager, Resource, Runtime,
    plugin::{self, TauriPlugin},
};
use tokio::io::AsyncReadExt;

/// Owns the Unix socket file, removed when the app exits (see [`crate::destroy`]).
struct InstanceLock(PathBuf);

impl Resource for InstanceLock {}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        socket_cleanup(&self.0);
    }
}

#[cfg(debug_assertions)]
static IS_PRIMARY_APP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn init<R: Runtime>(cb: Box<SingleInstanceCallback<R>>) -> TauriPlugin<R> {
    plugin::Builder::new("single-instance")
        .setup(|app, _api| {
            let socket = socket_path(app.config(), app.package_info());

            // Notify the singleton which may or may not exist.
            match notify_singleton(&socket, &std::env::args().collect::<Vec<_>>()) {
                Ok(_) => {
                    #[cfg(debug_assertions)]
                    IS_PRIMARY_APP.set(false).unwrap();
                    #[cfg(not(debug_assertions))]
                    std::process::exit(0);
                }
                Err(e) => {
                    #[cfg(debug_assertions)]
                    IS_PRIMARY_APP.set(true).unwrap();
                    match e.kind() {
                        ErrorKind::NotFound | ErrorKind::ConnectionRefused => {
                            // This process claims itself as singleton as likely none exists
                            socket_cleanup(&socket);
                            manage_instance_lock(app, InstanceLock(socket.clone()));
                            listen_for_other_instances(socket, app.clone(), cb);
                        }
                        _ => {
                            tracing::debug!(
                                "single_instance failed to notify - launching normally: {}",
                                e
                            );
                        }
                    }
                }
            }
            Ok(())
        })
        .on_event(|app, event| match event {
            RunEvent::Exit => {
                destroy(app);
            }
            #[cfg(debug_assertions)]
            RunEvent::Opened { urls } => {
                if let Some(true) = IS_PRIMARY_APP.get() {
                    return;
                }
                let socket = socket_path(app.config(), app.package_info());
                match notify_singleton(
                    &socket,
                    &std::env::args()
                        .take(1)
                        .chain(urls.iter().map(|url| url.to_string()))
                        .collect::<Vec<String>>(),
                ) {
                    Ok(_) => {
                        std::process::exit(0);
                    }
                    Err(e) => {
                        tracing::error!(?e);
                    }
                }
            }
            _ => {}
        })
        .build()
}

fn socket_path(config: &Config, _package_info: &tauri::PackageInfo) -> PathBuf {
    let identifier = config.identifier.replace(['.', '-'].as_ref(), "_");

    #[cfg(feature = "semver")]
    let identifier = format!(
        "{identifier}_{}",
        semver_compat_string(&_package_info.version),
    );

    // Use /tmp as socket path must be shorter than 100 chars.
    PathBuf::from(format!("/tmp/{}_si.sock", identifier))
}

fn socket_cleanup(socket: &PathBuf) {
    let _ = std::fs::remove_file(socket);
}

fn notify_singleton(socket: &PathBuf, args: &[String]) -> Result<(), std::io::Error> {
    let stream = UnixStream::connect(socket)?;
    let mut bf = BufWriter::new(&stream);
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    bf.write_all(cwd.as_bytes())?;
    bf.write_all(b"\0\0")?;
    let args_joined = args.join("\0");
    bf.write_all(args_joined.as_bytes())?;
    bf.flush()?;
    drop(bf);
    Ok(())
}

fn listen_for_other_instances<A: Runtime>(
    socket: PathBuf,
    app: AppHandle<A>,
    mut cb: Box<SingleInstanceCallback<A>>,
) {
    tauri::async_runtime::spawn(async move {
        match tokio::net::UnixListener::bind(socket) {
            Ok(listener) => loop {
                match listener.accept().await {
                    Ok((mut stream, _addr)) => {
                        let mut s = String::new();
                        match stream.read_to_string(&mut s).await {
                            Ok(_) => {
                                let (cwd, args) = s.split_once("\0\0").unwrap_or_default();
                                let args: Vec<String> =
                                    args.split('\0').map(String::from).collect();
                                cb(app.app_handle(), args, cwd.to_string());
                            }
                            Err(e) => {
                                tracing::debug!("single_instance failed to be notified: {e}")
                            }
                        }
                    }
                    Err(err) => {
                        tracing::debug!("single_instance failed to be notified: {}", err);
                        continue;
                    }
                }
            },
            Err(err) => {
                tracing::error!(
                    "single_instance failed to listen to other processes - launching normally: {}",
                    err
                );
            }
        }
    });
}
