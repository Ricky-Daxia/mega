// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::{env, fs};

use serde::Deserialize;
use tauri::api::process::{Command, CommandChild, CommandEvent};
use tauri::{Manager, State};

#[derive(Default)]
struct ServiceState {
    child: Option<CommandChild>,
    with_relay: bool,
}

impl Drop for ServiceState {
    fn drop(&mut self) {
        if let Some(child_process) = self.child.take() {
            child_process
                .kill()
                .expect("Failed to kill sidecar process");
        }
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
struct MegaStartParams {
    pub bootstrap_node: Option<String>,
}

fn set_up_lib(handle: tauri::AppHandle) {
    let resource_path = handle
        .path_resolver()
        .resource_dir()
        .expect("failed to resolve resource");
    let libs_dir = resource_path.join("libs");

    #[cfg(target_os = "macos")]
    std::env::set_var("DYLD_LIBRARY_PATH", libs_dir.to_str().unwrap());

    #[cfg(target_os = "linux")]
    std::env::set_var("LD_LIBRARY_PATH", libs_dir.to_str().unwrap());

    #[cfg(target_os = "windows")]
    std::env::set_var(
        "PATH",
        format!(
            "{};{}",
            libs_dir.to_str().unwrap(),
            std::env::var("PATH").unwrap()
        ),
    );
}

#[tauri::command]
fn start_mega_service(
    state: State<'_, Arc<Mutex<ServiceState>>>,
    params: MegaStartParams,
) -> Result<(), String> {
    let mut service_state = state.lock().unwrap();
    if service_state.child.is_some() {
        return Err("Service is already running".into());
    }

    let args = if let Some(ref addr) = params.bootstrap_node {
        service_state.with_relay = true;
        vec!["service", "http", "--bootstrap-node", addr]
    } else {
        service_state.with_relay = false;
        vec!["service", "http"]
    };
    let (mut rx, child) = Command::new_sidecar("mega")
        .expect("Failed to create `mega` binary command")
        .args(args)
        .spawn()
        .expect("Failed to spawn `Mega service`");

    service_state.child = Some(child);
    // Sidecar output
    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(line) => {
                    print!("{}", line);
                }
                CommandEvent::Stderr(line) => {
                    eprint!("Sidecar stderr: {}", line);
                }
                CommandEvent::Terminated(payload) => {
                    if let Some(code) = payload.code {
                        if code == 0 {
                            println!("Sidecar executed successfully.");
                        } else {
                            eprintln!("Sidecar failed with exit code: {}", code);
                        }
                    } else if let Some(signal) = payload.signal {
                        eprintln!("Sidecar terminated by signal: {}", signal);
                    }
                    break;
                }
                _ => {}
            }
        }
    });
    Ok(())
}

#[tauri::command]
fn stop_mega_service(state: State<'_, Arc<Mutex<ServiceState>>>) -> Result<(), String> {
    let mut service_state = state.lock().unwrap();
    if let Some(child) = service_state.child.take() {
        child.kill().map_err(|e| e.to_string())?;
        service_state.child = None;
    } else {
        println!("Mega Service is not running");
    }
    Ok(())
}

#[tauri::command]
fn restart_mega_service(
    state: State<'_, Arc<Mutex<ServiceState>>>,
    params: MegaStartParams,
) -> Result<(), String> {
    stop_mega_service(state.clone())?;
    start_mega_service(state, params)?;
    Ok(())
}

#[tauri::command]
fn mega_service_status(state: State<'_, Arc<Mutex<ServiceState>>>) -> Result<(bool, bool), String> {
    let service_state = state.lock().unwrap();
    Ok((service_state.child.is_some(), service_state.with_relay))
}

#[tauri::command]
fn clone_repository(repo_url: String, name: String) -> Result<(), String> {
    let home = match home::home_dir() {
        Some(path) if !path.as_os_str().is_empty() => path,
        _ => {
            println!("Unable to get your home dir!");
            PathBuf::new()
        }
    };
    let target_dir = home.join(".mega").join(name.clone());

    if target_dir.exists() {
        fs::remove_dir_all(&target_dir).unwrap();
    }

    let output = Command::new_sidecar("libra")
        .expect("Failed to create `libra` binary command")
        .args(["clone", &repo_url, (target_dir.to_str().unwrap())])
        .output()
        .map_err(|e| format!("Failed to execute process: {}", e))?;

    if output.status.success() {
        println!("{}", output.stdout);
    } else {
        eprintln!("{}", output.stderr);
    }
    change_remote_url(target_dir.clone(), name)?;
    push_to_new_remote(target_dir)?;
    Ok(())
}

fn change_remote_url(repo_path: PathBuf, name: String) -> Result<(), String> {
    Command::new_sidecar("libra")
        .expect("Failed to create `libra` binary command")
        .args(["remote", "remove", "origin"])
        .current_dir(repo_path.clone())
        .output()
        .map_err(|e| format!("Failed to execute process: {}", e))?;

    let output = Command::new_sidecar("libra")
        .expect("Failed to create `libra` binary command")
        .args([
            "remote",
            "add",
            "origin",
            &format!("http://localhost:8000/third-part/{}", name),
        ])
        .current_dir(repo_path.clone())
        .output()
        .map_err(|e| format!("Failed to execute process: {}", e))?;

    if output.status.success() {
        println!("{}", output.stdout);
    } else {
        eprintln!("{}", output.stderr);
    }
    Ok(())
}

fn push_to_new_remote(repo_path: PathBuf) -> Result<(), String> {
    let output = Command::new_sidecar("libra")
        .expect("Failed to create `libra` binary command")
        .args(["push", "origin", "master"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to execute process: {}", e))?;

    if output.status.success() {
        println!("{}", output.stdout);
    } else {
        eprintln!("{}", output.stderr);
    }
    Ok(())
}

fn main() {
    let params = MegaStartParams::default();
    tauri::Builder::default()
        .manage(Arc::new(Mutex::new(ServiceState::default())))
        .invoke_handler(tauri::generate_handler![
            start_mega_service,
            stop_mega_service,
            restart_mega_service,
            mega_service_status,
            clone_repository
        ])
        .setup(|app| {
            let app_handle = app.handle().clone();
            set_up_lib(app_handle);
            let state = app.state::<Arc<Mutex<ServiceState>>>().clone();
            if let Err(e) = start_mega_service(state, params) {
                eprintln!("Failed to restart rust_service: {}", e);
            } else {
                println!("Rust service restarted successfully");
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
