use super::*;
use crate::ComputerService;
use windows::Win32::UI::Controls::EM_SETPASSWORDCHAR;
#[path = "../fixture.rs"]
mod fixture;
// Native fixture windows share the desktop; concurrent fixture activation would
// invalidate the separate test that checks foreground preservation.
static NATIVE_FIXTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn execute(service: &ComputerService, action: ComputerAction) -> Value {
    let mutating = !action.is_read_only();
    let action_name = serde_json::to_value(&action).unwrap()["action"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut before_cursor = POINT::default();
    unsafe {
        GetCursorPos(&mut before_cursor).unwrap();
    }
    let before_foreground = unsafe { GetForegroundWindow() };
    let (_sender, cancel) = watch::channel(false);
    let value = service.execute(action, cancel).await.unwrap();
    let mut after_cursor = POINT::default();
    unsafe {
        GetCursorPos(&mut after_cursor).unwrap();
    }
    if mutating {
        assert_eq!(
            (before_cursor.x, before_cursor.y),
            (after_cursor.x, after_cursor.y),
            "System cursor changed during native action {action_name}"
        );
        assert_eq!(
            before_foreground,
            unsafe { GetForegroundWindow() },
            "Native action {action_name} stole foreground focus"
        );
    }
    value
}
fn edit_node(snapshot: &Value) -> String {
    snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| {
            node["patterns"]
                .as_array()
                .is_some_and(|values| values.contains(&json!("type_text")))
        })
        .unwrap()["node_id"]
        .as_str()
        .unwrap()
        .into()
}
fn scroll_node(snapshot: &Value) -> String {
    snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| {
            node["value"]
                .as_str()
                .is_some_and(|value| value.starts_with("Background scroll line"))
        })
        .unwrap()["node_id"]
        .as_str()
        .unwrap()
        .into()
}
fn id(value: &Value, field: &str) -> String {
    value[field].as_str().unwrap().into()
}
fn node(snapshot: &Value, name: &str) -> String {
    snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == name)
        .unwrap_or_else(|| panic!("Missing {name}: {snapshot}"))
        .get("node_id")
        .unwrap()
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn timed_out_message_can_have_applied_and_reports_unknown_outcome() {
    let _exclusive_fixture = NATIVE_FIXTURE_LOCK.blocking_lock();
    let fixture = fixture::Fixture::start(&format!(
        "DSH Delivery Uncertainty Test {}",
        uuid::Uuid::new_v4()
    ))
    .unwrap();
    let canvas = HWND(fixture.handles.canvas as _);
    // Our own control applies a character, then deliberately delays its return
    // past SendMessageTimeout. A timeout must not imply the input was rejected.
    control_message(canvas, WM_APP + 15, 700, 0).unwrap();
    let error = control_message(canvas, WM_CHAR, 'A' as usize, 1)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("outcome is unknown")
            && error.contains("partially applied")
            && error.contains("do not blindly retry"),
        "{error}"
    );
    assert_eq!(
        control_message(canvas, WM_APP + 12, 0, 0).unwrap(),
        1,
        "The timed-out message really changed the self-owned control"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_win32_fixture_observation_redaction_input_and_stale_targets() {
    let _exclusive_fixture = NATIVE_FIXTURE_LOCK.lock().await;
    let title = format!("DSH Computer Test {}", uuid::Uuid::new_v4());
    let mut fixture = fixture::Fixture::start(&title).unwrap();
    // A second self-owned window keeps foreground focus while the target is
    // observed/controlled behind it. No user application is modified.
    let _witness = fixture::Fixture::start("DSH Computer Background Witness").unwrap();
    // Let the fixture's own initial foreground transition finish before checking
    // whether any subsequent service operation changes the foreground window.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let service = ComputerService::new();
    let windows = execute(&service, ComputerAction::ListWindows).await;
    let target = windows["windows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["title"] == title)
        .unwrap();
    let window_id = id(target, "window_id");
    let mut snapshot = execute(
        &service,
        ComputerAction::Snapshot {
            window_id: window_id.clone(),
        },
    )
    .await;
    assert!(!snapshot.to_string().contains("fixture-secret-never-export"));
    assert!(snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["password"] == true && node["value"].is_null()));
    let capture = execute(
        &service,
        ComputerAction::Screenshot {
            window_id: window_id.clone(),
            snapshot_id: id(&snapshot, "snapshot_id"),
        },
    )
    .await;
    if capture["recognition"]["status"] == "ok" {
        let recognized = capture["recognition"]["text"].as_str().unwrap();
        assert!(
            recognized.to_ascii_uppercase().contains("VISUAL TARGET 42"),
            "Painted canvas text must be discovered from pixels: {recognized}"
        );
    }
    let image = base64::engine::general_purpose::STANDARD
        .decode(capture["image_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(&image[..8], b"\x89PNG\r\n\x1a\n");
    let decoder = png::Decoder::new(std::io::Cursor::new(image));
    let mut reader = decoder.read_info().unwrap();
    let mut data = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut data).unwrap();
    assert!(info.width > 400 && info.height > 200);
    let password = snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["password"] == true)
        .unwrap();
    let x = (password["bounds"]["x"].as_i64().unwrap() - snapshot["rect"]["x"].as_i64().unwrap()
        + 4) as usize;
    let y = (password["bounds"]["y"].as_i64().unwrap() - snapshot["rect"]["y"].as_i64().unwrap()
        + 4) as usize;
    assert_eq!(
        &data[(y * info.width as usize + x) * 4..(y * info.width as usize + x) * 4 + 4],
        &[20, 24, 26, 255]
    );
    let first = id(&snapshot, "snapshot_id");
    snapshot = execute(
        &service,
        ComputerAction::Invoke {
            window_id: window_id.clone(),
            snapshot_id: first.clone(),
            node_id: node(&snapshot, "Increment counter"),
        },
    )
    .await;
    assert_eq!(fixture.text(fixture.handles.counter), "Counter: 1");
    let pointer = service
        .native
        .pointer
        .handle()
        .expect("Desktop pointer window should exist after an action");
    let pointer_style = unsafe { GetWindowLongPtrW(pointer, GWL_EXSTYLE) } as u32;
    assert_eq!(
        pointer_style & (WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW).0,
        (WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW).0
    );
    assert!(unsafe { IsWindowVisible(pointer) }.as_bool());
    let pointer_bounds = rect(pointer).unwrap();
    assert_eq!(
        (pointer_bounds.left, pointer_bounds.top),
        (
            snapshot["pointer"]["desktop"]["x"].as_i64().unwrap() as i32,
            snapshot["pointer"]["desktop"]["y"].as_i64().unwrap() as i32
        )
    );
    assert_eq!(
        control_message(pointer, WM_NCHITTEST, 0, 0).unwrap() as isize,
        HTTRANSPARENT as isize
    );
    unsafe {
        let dc = GetDC(None);
        let pixel = GetPixel(dc, pointer_bounds.left + 5, pointer_bounds.top + 12);
        ReleaseDC(None, dc);
        assert_eq!(
            pixel.0, 0x0028a4ff,
            "The DSH arrow must be visibly rendered on the desktop"
        );
    }
    let (_sender, cancel) = watch::channel(false);
    assert!(service
        .execute(
            ComputerAction::Invoke {
                window_id: window_id.clone(),
                snapshot_id: first,
                node_id: "n0".into()
            },
            cancel
        )
        .await
        .is_err());
    assert_eq!(fixture.text(fixture.handles.counter), "Counter: 1");
    let editable = snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["value"] == "initial")
        .unwrap()["node_id"]
        .as_str()
        .unwrap()
        .to_owned();
    snapshot = execute(
        &service,
        ComputerAction::SetValue {
            window_id: window_id.clone(),
            snapshot_id: id(&snapshot, "snapshot_id"),
            node_id: editable,
            text: "native Unicode 中文".into(),
        },
    )
    .await;
    assert_eq!(fixture.text(fixture.handles.edit), "native Unicode 中文");
    let editable = snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["value"] == "native Unicode 中文")
        .unwrap();
    let x =
        (editable["bounds"]["x"].as_f64().unwrap() - snapshot["rect"]["x"].as_f64().unwrap()) + 12.;
    let y =
        (editable["bounds"]["y"].as_f64().unwrap() - snapshot["rect"]["y"].as_f64().unwrap()) + 12.;
    snapshot = execute(
        &service,
        ComputerAction::Click {
            window_id: window_id.clone(),
            snapshot_id: id(&snapshot, "snapshot_id"),
            x,
            y,
            button: "left".into(),
        },
    )
    .await;
    assert_eq!(snapshot["performed"], "select_control");
    assert_eq!(snapshot["pointer"]["visible"], true);
    // These are directed EDIT selection messages, not physical key injection.
    snapshot = execute(
        &service,
        ComputerAction::Key {
            window_id: window_id.clone(),
            snapshot_id: id(&snapshot, "snapshot_id"),
            node_id: edit_node(&snapshot),
            key: "HOME".into(),
        },
    )
    .await;
    snapshot = execute(
        &service,
        ComputerAction::Key {
            window_id: window_id.clone(),
            snapshot_id: id(&snapshot, "snapshot_id"),
            node_id: edit_node(&snapshot),
            key: "SHIFT+END".into(),
        },
    )
    .await;
    snapshot = execute(
        &service,
        ComputerAction::TypeText {
            window_id: window_id.clone(),
            snapshot_id: id(&snapshot, "snapshot_id"),
            node_id: edit_node(&snapshot),
            text: "Typed by DSH 蓝色鱼".into(),
        },
    )
    .await;
    assert_eq!(fixture.text(fixture.handles.edit), "Typed by DSH 蓝色鱼");
    snapshot = execute(
        &service,
        ComputerAction::Scroll {
            window_id: window_id.clone(),
            snapshot_id: id(&snapshot, "snapshot_id"),
            node_id: scroll_node(&snapshot),
            delta: -1,
        },
    )
    .await;
    let multiline = scroll_node(&snapshot);
    snapshot = execute(
        &service,
        ComputerAction::Key {
            window_id: window_id.clone(),
            snapshot_id: id(&snapshot, "snapshot_id"),
            node_id: multiline.clone(),
            key: "END".into(),
        },
    )
    .await;
    snapshot = execute(
        &service,
        ComputerAction::Key {
            window_id: window_id.clone(),
            snapshot_id: id(&snapshot, "snapshot_id"),
            node_id: multiline,
            key: "ENTER".into(),
        },
    )
    .await;
    assert!(fixture
        .text(fixture.handles.multiline)
        .ends_with("line 59\r\n\r\n"));
    let editable = edit_node(&snapshot);
    control_message(HWND(fixture.handles.edit as _), EM_SETPASSWORDCHAR, 0x2a, 0).unwrap();
    let (_sender, cancel) = watch::channel(false);
    let changed = service
        .execute(
            ComputerAction::SetValue {
                window_id: window_id.clone(),
                snapshot_id: id(&snapshot, "snapshot_id"),
                node_id: editable,
                text: "must not be written after password change".into(),
            },
            cancel,
        )
        .await
        .unwrap_err();
    assert!(
        changed.to_string().contains("password state changed"),
        "{changed}"
    );
    assert_eq!(fixture.text(fixture.handles.edit), "Typed by DSH 蓝色鱼");
    control_message(HWND(fixture.handles.edit as _), EM_SETPASSWORDCHAR, 0, 0).unwrap();
    snapshot = execute(
        &service,
        ComputerAction::Snapshot {
            window_id: window_id.clone(),
        },
    )
    .await;
    // This custom child draws its text itself and handles only window-directed
    // messages. UIA does not expose the painted target as an editable control.
    let canvas_bounds = rect(HWND(fixture.handles.canvas as _)).unwrap();
    let x = (canvas_bounds.left - snapshot["rect"]["x"].as_i64().unwrap() as i32 + 32) as f64;
    let y = (canvas_bounds.top - snapshot["rect"]["y"].as_i64().unwrap() as i32 + 32) as f64;
    snapshot = execute(
        &service,
        ComputerAction::Click {
            window_id: window_id.clone(),
            snapshot_id: id(&snapshot, "snapshot_id"),
            x,
            y,
            button: "left".into(),
        },
    )
    .await;
    assert_eq!(snapshot["delivery"], "unverified");
    assert_eq!(snapshot["input_target"]["node_id"], "background");
    assert_eq!(
        control_message(HWND(fixture.handles.canvas as _), WM_APP + 11, 0, 0).unwrap(),
        1
    );
    snapshot = execute(
        &service,
        ComputerAction::TypeText {
            window_id: window_id.clone(),
            snapshot_id: id(&snapshot, "snapshot_id"),
            node_id: "background".into(),
            text: "Canvas DSH".into(),
        },
    )
    .await;
    assert_eq!(
        control_message(HWND(fixture.handles.canvas as _), WM_APP + 12, 0, 0).unwrap(),
        10
    );
    snapshot = execute(
        &service,
        ComputerAction::Key {
            window_id: window_id.clone(),
            snapshot_id: id(&snapshot, "snapshot_id"),
            node_id: "background".into(),
            key: "ENTER".into(),
        },
    )
    .await;
    assert_eq!(
        control_message(HWND(fixture.handles.canvas as _), WM_APP + 14, 0, 0).unwrap(),
        1
    );
    snapshot = execute(
        &service,
        ComputerAction::Scroll {
            window_id: window_id.clone(),
            snapshot_id: id(&snapshot, "snapshot_id"),
            node_id: "background".into(),
            delta: 2,
        },
    )
    .await;
    assert_eq!(
        control_message(HWND(fixture.handles.canvas as _), WM_APP + 13, 0, 0).unwrap(),
        2
    );
    service.hide_pointer();
    assert!(!unsafe { IsWindowVisible(pointer) }.as_bool());
    let (_sender, cancel) = watch::channel(true);
    assert!(service
        .execute(
            ComputerAction::Invoke {
                window_id: window_id.clone(),
                snapshot_id: id(&snapshot, "snapshot_id"),
                node_id: node(&snapshot, "Increment counter")
            },
            cancel
        )
        .await
        .is_err());
    assert_eq!(fixture.text(fixture.handles.counter), "Counter: 1");
    // Exercise expiry against an actual UIA snapshot without a minute-long sleep.
    let expiry_window = window_id.clone();
    std::thread::spawn(move || {
        let mut state = State::new().unwrap();
        state.list_windows().unwrap();
        let (_sender, cancel) = watch::channel(false);
        let (reply, _receive) = oneshot::channel();
        let request = Request {
            action: ComputerAction::Snapshot {
                window_id: expiry_window.clone(),
            },
            cancel,
            deadline: Instant::now() + REQUEST_LIMIT,
            reply,
        };
        let observed = state.snapshot(&expiry_window, &request).unwrap();
        state.snapshots.get_mut(&expiry_window).unwrap().created =
            Instant::now() - Duration::from_secs(61);
        let target = state.target(&expiry_window).unwrap();
        assert!(state
            .verify_snapshot(
                &expiry_window,
                observed["snapshot_id"].as_str().unwrap(),
                &target
            )
            .unwrap_err()
            .to_string()
            .contains("stale"));
    })
    .join()
    .unwrap();
    fixture.move_window();
    tokio::time::sleep(Duration::from_millis(120)).await;
    let (_sender, cancel) = watch::channel(false);
    assert!(service
        .execute(
            ComputerAction::Click {
                window_id: window_id.clone(),
                snapshot_id: id(&snapshot, "snapshot_id"),
                x: 10.,
                y: 10.,
                button: "left".into()
            },
            cancel
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("moved"));
    snapshot = execute(
        &service,
        ComputerAction::Snapshot {
            window_id: window_id.clone(),
        },
    )
    .await;
    fixture.close();
    let (_sender, cancel) = watch::channel(false);
    assert!(service
        .execute(
            ComputerAction::Invoke {
                window_id,
                snapshot_id: id(&snapshot, "snapshot_id"),
                node_id: node(&snapshot, "Increment counter")
            },
            cancel
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("closed"));
    drop(service);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !unsafe { IsWindow(Some(pointer)) }.as_bool(),
        "Dropping the controller must close its desktop cursor layer"
    );
}
