// A GUI-subsystem entry point shares the exact same command parser and runtime.
// cargo install installs this alongside dsh.exe; double-clicking starts `app`.
#![cfg_attr(windows, windows_subsystem = "windows")]
include!("main.rs");
