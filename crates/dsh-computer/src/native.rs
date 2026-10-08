use super::{parse_key, ComputerAction};
use anyhow::{bail, ensure, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{mpsc, Arc, Weak},
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, watch};
use windows::{
    core::{w, Interface, BOOL},
    Win32::{
        Foundation::{
            CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT, WAIT_ABANDONED, WAIT_OBJECT_0, WPARAM,
        },
        Graphics::Gdi::*,
        Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS},
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_MULTITHREADED,
            },
            Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
        },
        UI::{
            Accessibility::*,
            Controls::{EM_GETSEL, EM_REPLACESEL, EM_SETSEL},
            HiDpi::{
                SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            },
            WindowsAndMessaging::*,
        },
    },
};

const REQUEST_LIMIT: Duration = Duration::from_secs(8);
const SNAPSHOT_LIFETIME: Duration = Duration::from_secs(60);
const UNKNOWN_INPUT_OUTCOME: &str = "Input outcome is unknown and may have partially applied; observe the target again before deciding what remains, and do not blindly retry";

struct Request {
    action: ComputerAction,
    cancel: watch::Receiver<bool>,
    deadline: Instant,
    reply: oneshot::Sender<Result<Value>>,
}
impl Request {
    fn alive(&self) -> Result<()> {
        ensure!(
            !self.reply.is_closed() && !*self.cancel.borrow(),
            "Computer action cancelled"
        );
        ensure!(
            Instant::now() < self.deadline,
            "Computer observation/action timed out; refresh the target"
        );
        Ok(())
    }
}

#[derive(Clone)]
pub struct Worker {
    sender: Option<mpsc::SyncSender<Request>>,
    pointer: Arc<crate::pointer::DesktopPointer>,
}
impl Worker {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Request>(8);
        let pointer = Arc::new(crate::pointer::DesktopPointer::default());
        let desktop_pointer = Arc::downgrade(&pointer);
        let thread = std::thread::Builder::new()
            .name("dsh-computer-mta".into())
            .spawn(move || {
                // UIA objects never cross the MTA thread boundary. Construction does
                // not inspect the desktop; initialize lazily on the first request.
                let mut state = None;
                while let Ok(request) = receiver.recv() {
                    let result = (|| {
                        request.alive()?;
                        if state.is_none() {
                            let mut initialized = State::new()?;
                            initialized.desktop_pointer = Some(desktop_pointer.clone());
                            state = Some(initialized);
                        }
                        state.as_mut().unwrap().execute(&request)
                    })();
                    let _ = request.reply.send(result);
                }
            });
        Self {
            sender: thread.ok().map(|_| sender),
            pointer,
        }
    }
    pub fn hide_pointer(&self) {
        self.pointer.hide();
    }
    pub async fn execute(
        &self,
        action: ComputerAction,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Value> {
        let mutating = !action.is_read_only();
        let sender = self
            .sender
            .as_ref()
            .context("Could not start Windows automation worker")?;
        let (reply, received) = oneshot::channel();
        sender
            .try_send(Request {
                action,
                cancel: cancel.clone(),
                deadline: Instant::now() + REQUEST_LIMIT,
                reply,
            })
            .map_err(|_| {
                anyhow::anyhow!(
                    "Computer service is busy or unavailable; retry after the current operation"
                )
            })?;
        tokio::select! {
            result = tokio::time::timeout(REQUEST_LIMIT + Duration::from_secs(1), received) => result.context(if mutating { UNKNOWN_INPUT_OUTCOME } else { "Computer observation timed out" })?.context(if mutating { UNKNOWN_INPUT_OUTCOME } else { "Computer worker stopped" })?,
            _ = async { loop { if *cancel.borrow() || cancel.changed().await.is_err() { break; } } } => {
                if mutating { bail!("Computer action cancelled. {UNKNOWN_INPUT_OUTCOME}"); }
                bail!("Computer observation cancelled");
            },
        }
    }
}

struct Apartment {
    old_dpi: DPI_AWARENESS_CONTEXT,
}
impl Apartment {
    fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        let old_dpi =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        Ok(Self { old_dpi })
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            if !self.old_dpi.0.is_null() {
                SetThreadDpiAwarenessContext(self.old_dpi);
            }
            CoUninitialize();
        }
    }
}

#[derive(Clone)]
struct Target {
    hwnd: HWND,
    pid: u32,
    title: String,
    class: String,
}
struct Node {
    element: IUIAutomationElement,
    rect: RECT,
    password: bool,
    name: String,
}
struct Snapshot {
    id: String,
    window_id: String,
    created: Instant,
    rect: RECT,
    root: IUIAutomationElement,
    nodes: Vec<Node>,
    value: Value,
}
struct SelectedInput {
    window_id: String,
    hwnd: HWND,
    pid: u32,
    class: String,
    rect: RECT,
    point: POINT,
}
struct State {
    snapshots: HashMap<String, Snapshot>,
    targets: HashMap<String, Target>,
    pointers: HashMap<String, Value>,
    desktop_pointer: Option<Weak<crate::pointer::DesktopPointer>>,
    selected_input: Option<SelectedInput>,
    automation: IUIAutomation,
    _apartment: Apartment,
}
impl State {
    fn new() -> Result<Self> {
        let apartment = Apartment::new()?;
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)? };
        if let Ok(timeouts) = automation.cast::<IUIAutomation2>() {
            unsafe {
                timeouts.SetConnectionTimeout(700)?;
                timeouts.SetTransactionTimeout(700)?;
            }
        }
        Ok(Self {
            snapshots: HashMap::new(),
            targets: HashMap::new(),
            pointers: HashMap::new(),
            desktop_pointer: None,
            selected_input: None,
            automation,
            _apartment: apartment,
        })
    }

    fn execute(&mut self, request: &Request) -> Result<Value> {
        request.alive()?;
        match &request.action {
            ComputerAction::ListWindows => return self.list_windows(),
            ComputerAction::Snapshot { window_id } => return self.snapshot(window_id, request),
            _ => (),
        }
        let window_id = request.action.window_id().unwrap();
        let snapshot_id = request.action.snapshot_id().unwrap();
        let target = self.target(window_id)?;
        self.verify_snapshot(window_id, snapshot_id, &target)?;
        if matches!(request.action, ComputerAction::Screenshot { .. }) {
            let snapshot = self.snapshots.get(window_id).unwrap();
            let mut value = snapshot.value.clone();
            let (bytes, width, height) = capture(&target, snapshot, request)?;
            request.alive()?;
            value["recognition"] = crate::recognition::recognize_masked_png(&bytes, width, height);
            request.alive()?;
            value["expires_in_ms"] = json!(SNAPSHOT_LIFETIME
                .saturating_sub(snapshot.created.elapsed())
                .as_millis());
            value["image_base64"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes));
            value["mime"] = json!("image/png");
            value["width"] = json!(width);
            value["height"] = json!(height);
            return Ok(value);
        }
        let _input = InputLease::acquire()?;
        request.alive()?;
        let target = self.target(window_id)?;
        self.verify_snapshot(window_id, snapshot_id, &target)?;
        // Consume the snapshot even if the OS refuses the action. No stale
        // action can silently be replayed by a second model/tool invocation.
        let snapshot = self.snapshots.remove(window_id).unwrap();
        let mut delivery = "verified";
        let performed = match &request.action {
            ComputerAction::Invoke { node_id, .. } => {
                let node = self.node(&snapshot, node_id)?;
                let button = background_button(&node.element, &target)?;
                request.alive()?;
                self.point_to_node(window_id, node, &snapshot)?;
                request.alive()?;
                invoke_background_button(button)?;
                "invoke"
            }
            ComputerAction::SetValue { node_id, text, .. } => {
                let node = self.node(&snapshot, node_id)?;
                let edit = standard_edit(node, &target)?;
                let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
                request.alive()?;
                self.point_to_node(window_id, node, &snapshot)?;
                request.alive()?;
                ensure!(
                    control_message(edit, WM_SETTEXT, 0, text.as_ptr() as isize)? != 0,
                    "Edit control refused the background value update"
                );
                "set_value"
            }
            ComputerAction::Click { x, y, button, .. } => {
                ensure!(button == "left", "Only a single left background invocation is supported; right/double clicks have no physical mouse fallback");
                ensure!(
                    *x < (snapshot.rect.right - snapshot.rect.left) as f64
                        && *y < (snapshot.rect.bottom - snapshot.rect.top) as f64,
                    "Click is outside the observed window"
                );
                let point = POINT {
                    x: snapshot.rect.left + x.round() as i32,
                    y: snapshot.rect.top + y.round() as i32,
                };
                ensure!(
                    !snapshot
                        .nodes
                        .iter()
                        .any(|node| node.password && contains(node.rect, point)),
                    "Password controls are redacted; enter them manually"
                );
                let chosen = snapshot
                    .nodes
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, node)| {
                        point.x >= node.rect.left
                            && point.x < node.rect.right
                            && point.y >= node.rect.top
                            && point.y < node.rect.bottom
                            && !node.password
                            && (background_button(&node.element, &target).is_ok()
                                || standard_edit(node, &target).is_ok())
                    })
                    .map(|(index, _)| format!("n{index}"));
                request.alive()?;
                self.show_pointer(window_id, point, snapshot.rect, chosen.as_deref())?;
                if let Some(chosen) = chosen {
                    let node = self.node(&snapshot, &chosen)?;
                    let handle = unsafe { node.element.CurrentNativeWindowHandle()? };
                    self.select_input(window_id, handle, point)?;
                    if let Ok(button) = background_button(&node.element, &target) {
                        request.alive()?;
                        invoke_background_button(button)?;
                        "invoke"
                    } else {
                        "select_control"
                    }
                } else {
                    let handle = child_at_point(target.hwnd, point)?;
                    self.select_input(window_id, handle, point)?;
                    self.message_target(window_id, "background", &snapshot, &target)?;
                    let local = client_point(handle, point)?;
                    request.alive()?;
                    control_message(handle, WM_MOUSEMOVE, 0, packed_point(local)?)?;
                    request.alive()?;
                    control_message(handle, WM_LBUTTONDOWN, 1, packed_point(local)?).with_context(
                        || {
                            format!(
                                "Background mouse-down was interrupted. {UNKNOWN_INPUT_OUTCOME}"
                            )
                        },
                    )?;
                    // Complete the pair even if cancellation arrives after down.
                    control_message(handle, WM_LBUTTONUP, 0, packed_point(local)?)
                        .with_context(|| format!("Background mouse-up failed after mouse-down was sent. {UNKNOWN_INPUT_OUTCOME}"))?;
                    delivery = "unverified";
                    "background_messages_sent"
                }
            }
            ComputerAction::TypeText { node_id, text, .. } => {
                if node_id != "background"
                    && self
                        .node(&snapshot, node_id)
                        .is_ok_and(|node| standard_edit(node, &target).is_ok())
                {
                    let node = self.node(&snapshot, node_id)?;
                    let edit = standard_edit(node, &target)?;
                    self.point_to_node(window_id, node, &snapshot)?;
                    request.alive()?;
                    replace_selection(edit, text)?;
                    "type_text"
                } else {
                    let (handle, point) =
                        self.message_target(window_id, node_id, &snapshot, &target)?;
                    self.show_pointer(window_id, point, snapshot.rect, Some(node_id))?;
                    ensure!(
                        unsafe { IsWindowUnicode(handle) }.as_bool(),
                        "Background text requires a Unicode window"
                    );
                    for (sent, character) in text.encode_utf16().enumerate() {
                        request.alive().with_context(|| format!("Background text interrupted after {sent} UTF-16 units were sent. {UNKNOWN_INPUT_OUTCOME}"))?;
                        control_message(handle, WM_CHAR, character as usize, 1)
                            .with_context(|| format!("Background text delivery interrupted after {sent} completed UTF-16 messages. {UNKNOWN_INPUT_OUTCOME}"))?;
                    }
                    delivery = "unverified";
                    "background_messages_sent"
                }
            }
            ComputerAction::Key { node_id, key, .. } => {
                if node_id != "background"
                    && self
                        .node(&snapshot, node_id)
                        .is_ok_and(|node| standard_edit(node, &target).is_ok())
                {
                    let node = self.node(&snapshot, node_id)?;
                    let edit = standard_edit(node, &target)?;
                    self.point_to_node(window_id, node, &snapshot)?;
                    request.alive()?;
                    background_key(edit, key, node)?;
                    "key"
                } else {
                    let keys = parse_key(key)?;
                    ensure!(keys.len()==1 && !matches!(keys[0],0x10..=0x12), "Background messages cannot emulate global modifier state; use a single key or a verified Edit node");
                    let (handle, point) =
                        self.message_target(window_id, node_id, &snapshot, &target)?;
                    self.show_pointer(window_id, point, snapshot.rect, Some(node_id))?;
                    request.alive()?;
                    control_message(handle, WM_KEYDOWN, keys[0] as usize, 1).with_context(
                        || format!("Background key-down was interrupted. {UNKNOWN_INPUT_OUTCOME}"),
                    )?;
                    control_message(
                        handle,
                        WM_KEYUP,
                        keys[0] as usize,
                        (1u32 | (3u32 << 30)) as isize,
                    ).with_context(|| format!("Background key-up failed after key-down was sent. {UNKNOWN_INPUT_OUTCOME}"))?;
                    delivery = "unverified";
                    "background_messages_sent"
                }
            }
            ComputerAction::Scroll { node_id, delta, .. } => {
                if node_id != "background"
                    && self
                        .node(&snapshot, node_id)
                        .is_ok_and(|node| standard_edit(node, &target).is_ok_and(is_multiline))
                {
                    let node = self.node(&snapshot, node_id)?;
                    let edit = standard_edit(node, &target)?;
                    self.point_to_node(window_id, node, &snapshot)?;
                    for _ in 0..delta.unsigned_abs() {
                        request.alive()?;
                        control_message(
                            edit,
                            WM_VSCROLL,
                            if *delta > 0 {
                                SB_LINEUP.0 as usize
                            } else {
                                SB_LINEDOWN.0 as usize
                            },
                            0,
                        )?;
                    }
                    "scroll"
                } else {
                    let (handle, point) =
                        self.message_target(window_id, node_id, &snapshot, &target)?;
                    self.show_pointer(window_id, point, snapshot.rect, Some(node_id))?;
                    request.alive()?;
                    control_message(
                        handle,
                        WM_MOUSEWHEEL,
                        ((*delta * 120) as u16 as usize) << 16,
                        packed_point(point)?,
                    )?;
                    delivery = "unverified";
                    "background_messages_sent"
                }
            }
            _ => unreachable!(),
        };
        std::thread::sleep(Duration::from_millis(80));
        if let Err(error) = request.alive() {
            return Ok(
                json!({"performed":performed,"delivery":delivery,"window_id":window_id,"refresh_required":true,"observation_error":error.to_string()}),
            );
        }
        // A successful close/dialog action may remove the target. Report the
        // performed action honestly rather than pretending the input failed.
        match self.snapshot(window_id, request) {
            Ok(mut value) => {
                value["performed"] = json!(performed);
                value["delivery"] = json!(delivery);
                Ok(value)
            }
            Err(error) => Ok(
                json!({"performed":performed,"delivery":delivery,"window_id":window_id,"refresh_required":true,"observation_error":error.to_string()}),
            ),
        }
    }

    fn list_windows(&mut self) -> Result<Value> {
        let mut windows = Vec::<Target>::new();
        unsafe {
            EnumWindows(
                Some(enumerate),
                LPARAM((&mut windows as *mut Vec<Target>) as isize),
            )?;
        }
        let mut next = HashMap::new();
        let mut values = Vec::new();
        for target in windows.into_iter().take(128) {
            // Stable targeting reference across CLI windows/observe invocations;
            // this is not an authorization token. Actions additionally require
            // a random, process-local snapshot with a verified UIA root.
            let id = uuid::Uuid::from_u128(
                ((target.pid as u128) << 64) | target.hwnd.0 as usize as u128,
            )
            .to_string();
            if let Ok(bounds) = rect(target.hwnd) {
                values.push(json!({"window_id":id,"title":target.title,"pid":target.pid,"rect":rect_json(bounds),"foreground":unsafe {GetForegroundWindow()} == target.hwnd}));
                next.insert(id, target);
            }
        }
        self.targets = next;
        self.snapshots.retain(|id, _| self.targets.contains_key(id));
        Ok(json!({"supported":true,"windows":values,"coordinate_space":"physical_pixels"}))
    }

    fn target(&self, id: &str) -> Result<Target> {
        let target = self
            .targets
            .get(id)
            .context("Window was not observed; list windows again")?;
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(target.hwnd, Some(&mut pid));
        }
        ensure!(
            unsafe { IsWindow(Some(target.hwnd)) }.as_bool()
                && pid == target.pid
                && window_class(target.hwnd) == target.class,
            "Window closed or was replaced; list windows again"
        );
        ensure!(
            unsafe { IsWindowVisible(target.hwnd) }.as_bool()
                && !unsafe { IsIconic(target.hwnd) }.as_bool(),
            "Window is hidden or minimized; restore it before observing"
        );
        Ok(target.clone())
    }

    fn snapshot(&mut self, id: &str, request: &Request) -> Result<Value> {
        let target = self.target(id)?;
        if self
            .selected_input
            .as_ref()
            .is_some_and(|selected| selected.window_id != id || !selected.valid(&target))
        {
            self.selected_input = None;
        }
        let bounds = rect(target.hwnd)?;
        request.alive()?;
        let root = unsafe { self.automation.ElementFromHandle(target.hwnd) }
            .context("Window does not expose Windows UI Automation")?;
        let walker = unsafe { self.automation.ControlViewWalker()? };
        let mut nodes = Vec::new();
        let mut values = Vec::new();
        let mut truncated = false;
        let mut pending = vec![(root.clone(), None::<String>, 0usize)];
        while let Some((element, parent, depth)) = pending.pop() {
            request.alive()?;
            if nodes.len() >= 160 || depth > 10 {
                truncated = true;
                continue;
            }
            let mut password = unsafe { element.CurrentIsPassword() }
                .map(|value| value.as_bool())
                .unwrap_or(true);
            if let Ok(handle) = unsafe { element.CurrentNativeWindowHandle() } {
                if !handle.0.is_null() && window_class(handle).eq_ignore_ascii_case("Edit") {
                    password |= unsafe { GetWindowLongPtrW(handle, GWL_STYLE) } as u32
                        & ES_PASSWORD as u32
                        != 0;
                }
            }
            let name = if password {
                "[password field]".into()
            } else {
                clip(
                    &unsafe { element.CurrentName() }
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    160,
                )
            };
            let element_rect = unsafe { element.CurrentBoundingRectangle() }.unwrap_or_default();
            let role = unsafe { element.CurrentLocalizedControlType() }
                .map(|value| value.to_string())
                .unwrap_or_else(|_| "control".into());
            let mut patterns = Vec::new();
            if background_button(&element, &target).is_ok() {
                patterns.push("invoke");
            }
            let value_pattern = unsafe {
                element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            }
            .ok();
            let editable_value = value_pattern.as_ref().is_some_and(|pattern| {
                unsafe { pattern.CurrentIsReadOnly() }.is_ok_and(|value| !value.as_bool())
            });
            if !password {
                let native = unsafe { element.CurrentNativeWindowHandle() }.ok();
                if editable_value
                    && native.is_some_and(|handle| {
                        !handle.0.is_null()
                            && window_class(handle).eq_ignore_ascii_case("Edit")
                            && unsafe { GetAncestor(handle, GA_ROOT) } == target.hwnd
                    })
                {
                    patterns.extend(["set_value", "type_text", "key"]);
                    if native.is_some_and(is_multiline) {
                        patterns.push("scroll");
                    }
                }
            }
            let value = if password {
                None
            } else {
                value_pattern
                    .as_ref()
                    .and_then(|value| unsafe { value.CurrentValue() }.ok())
                    .map(|value| clip(&value.to_string(), 512))
            };
            let node_id = format!("n{}", nodes.len());
            let background_patterns = if !password
                && unsafe { element.CurrentNativeWindowHandle() }.is_ok_and(|handle| {
                    !handle.0.is_null() && unsafe { GetAncestor(handle, GA_ROOT) } == target.hwnd
                }) {
                vec!["type_text", "key", "scroll"]
            } else {
                Vec::new()
            };
            values.push(json!({"node_id":node_id,"parent_id":parent,"role":role,"name":name,"value":value,"bounds":rect_json(element_rect),
                "enabled":unsafe {element.CurrentIsEnabled()}.is_ok_and(|value|value.as_bool()),"offscreen":unsafe {element.CurrentIsOffscreen()}.map(|value|value.as_bool()).unwrap_or(true),"password":password,"patterns":patterns,"background_patterns":background_patterns}));
            nodes.push(Node {
                element: element.clone(),
                rect: element_rect,
                password,
                name,
            });
            if depth == 10 {
                truncated = true;
                continue;
            }
            if let Ok(mut child) = unsafe { walker.GetFirstChildElement(&element) } {
                let mut children = Vec::new();
                for _ in 0..160 {
                    request.alive()?;
                    children.push((child.clone(), Some(node_id.clone()), depth + 1));
                    match unsafe { walker.GetNextSiblingElement(&child) } {
                        Ok(next) => child = next,
                        Err(_) => break,
                    }
                }
                pending.extend(children.into_iter().rev());
            }
        }
        let snapshot_id = uuid::Uuid::new_v4().to_string();
        let input_target = self.selected_input.as_ref().filter(|selected| selected.window_id==id).map(|selected| json!({"node_id":"background","window_id":id,"class":selected.class,"delivery":"unverified","bounds":rect_json(selected.rect)}));
        let value = json!({"window_id":id,"title":target.title,"snapshot_id":snapshot_id,"rect":rect_json(bounds),"nodes":values,"truncated":truncated,"coordinate_space":"physical_pixels","expires_in_ms":SNAPSHOT_LIFETIME.as_millis(),"pointer":self.pointers.get(id),"input_target":input_target});
        ensure!(
            same_rect(rect(target.hwnd)?, bounds),
            "Window changed during observation; retry"
        );
        self.snapshots.insert(
            id.into(),
            Snapshot {
                id: snapshot_id,
                window_id: id.into(),
                created: Instant::now(),
                rect: bounds,
                root,
                nodes,
                value: value.clone(),
            },
        );
        Ok(value)
    }

    fn verify_snapshot(&self, window_id: &str, snapshot_id: &str, target: &Target) -> Result<()> {
        let snapshot = self
            .snapshots
            .get(window_id)
            .context("Observe the selected window before acting")?;
        ensure!(
            snapshot.id == snapshot_id
                && snapshot.window_id == window_id
                && snapshot.created.elapsed() <= SNAPSHOT_LIFETIME,
            "Snapshot is stale; observe the window again"
        );
        ensure!(
            same_rect(rect(target.hwnd)?, snapshot.rect),
            "Window moved or resized; observe it again"
        );
        let fresh = unsafe { self.automation.ElementFromHandle(target.hwnd)? };
        ensure!(
            unsafe { self.automation.CompareElements(&snapshot.root, &fresh)? }.as_bool(),
            "Window was replaced; observe again"
        );
        Ok(())
    }
    fn node<'a>(&self, snapshot: &'a Snapshot, id: &str) -> Result<&'a Node> {
        let index: usize = id.strip_prefix('n').context("Invalid node id")?.parse()?;
        let node = snapshot
            .nodes
            .get(index)
            .context("Node is not in the current snapshot")?;
        ensure!(
            unsafe { node.element.CurrentIsPassword()? }.as_bool() == node.password,
            "Control password state changed; observe again"
        );
        if let Ok(handle) = unsafe { node.element.CurrentNativeWindowHandle() } {
            if !handle.0.is_null() && window_class(handle).eq_ignore_ascii_case("Edit") {
                // The UIA proxy can lag behind EM_SETPASSWORDCHAR; the native
                // Edit style is authoritative immediately before a mutation.
                let masked = unsafe { GetWindowLongPtrW(handle, GWL_STYLE) } as u32
                    & ES_PASSWORD as u32
                    != 0;
                ensure!(
                    masked == node.password,
                    "Control password state changed; observe again"
                );
            }
        }
        ensure!(
            unsafe { node.element.CurrentIsEnabled()? }.as_bool()
                && !unsafe { node.element.CurrentIsOffscreen()? }.as_bool(),
            "Control is disabled or no longer visible"
        );
        ensure!(
            same_rect(
                unsafe { node.element.CurrentBoundingRectangle()? },
                node.rect
            ),
            "Control moved; observe again"
        );
        if !node.password {
            ensure!(
                clip(&unsafe { node.element.CurrentName()? }.to_string(), 160) == node.name,
                "Control changed; observe again"
            );
        }
        Ok(node)
    }
    fn point_to_node(&mut self, window_id: &str, node: &Node, snapshot: &Snapshot) -> Result<()> {
        self.show_pointer(
            window_id,
            POINT {
                x: (node.rect.left + node.rect.right) / 2,
                y: (node.rect.top + node.rect.bottom) / 2,
            },
            snapshot.rect,
            None,
        )
    }
    fn show_pointer(
        &mut self,
        window_id: &str,
        point: POINT,
        bounds: RECT,
        node: Option<&str>,
    ) -> Result<()> {
        let pointer = self
            .desktop_pointer
            .as_ref()
            .and_then(Weak::upgrade)
            .context("DSH desktop cursor controller closed")?;
        pointer.show(point)?;
        self.pointers.insert(window_id.into(),json!({"x":point.x-bounds.left,"y":point.y-bounds.top,"visible":true,"mode":"desktop_overlay","desktop":{"x":point.x,"y":point.y},"node_id":node,"hide_after_ms":5000}));
        Ok(())
    }
    fn select_input(&mut self, window_id: &str, hwnd: HWND, point: POINT) -> Result<()> {
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        self.selected_input = Some(SelectedInput {
            window_id: window_id.into(),
            hwnd,
            pid,
            class: window_class(hwnd),
            rect: rect(hwnd)?,
            point,
        });
        Ok(())
    }
    fn message_target(
        &self,
        window_id: &str,
        node_id: &str,
        snapshot: &Snapshot,
        target: &Target,
    ) -> Result<(HWND, POINT)> {
        let (handle, point) = if node_id == "background" {
            let selected = self.selected_input.as_ref().context(
                "Click a coordinate in this window before using the background input target",
            )?;
            ensure!(
                selected.window_id == window_id && selected.valid(target),
                "Selected background control changed; observe and click it again"
            );
            (selected.hwnd, selected.point)
        } else {
            let node = self.node(snapshot, node_id)?;
            ensure!(
                !node.password,
                "Password controls are redacted; enter them manually"
            );
            let handle = unsafe { node.element.CurrentNativeWindowHandle()? };
            ensure!(
                !handle.0.is_null() && unsafe { GetAncestor(handle, GA_ROOT) } == target.hwnd,
                "Node has no background window target; use an observed coordinate click first"
            );
            (
                handle,
                POINT {
                    x: (node.rect.left + node.rect.right) / 2,
                    y: (node.rect.top + node.rect.bottom) / 2,
                },
            )
        };
        ensure!(
            unsafe { IsWindowVisible(handle) }.as_bool()
                && unsafe { GetWindowLongPtrW(handle, GWL_STYLE) } as u32 & WS_DISABLED.0 == 0,
            "Background control is hidden or disabled"
        );
        for (index, node) in snapshot.nodes.iter().enumerate() {
            if contains(node.rect, point) {
                self.node(snapshot, &format!("n{index}"))?;
                ensure!(
                    !node.password,
                    "Password controls are redacted; enter them manually"
                );
            }
        }
        if window_class(handle).eq_ignore_ascii_case("Edit") {
            let style = unsafe { GetWindowLongPtrW(handle, GWL_STYLE) } as u32;
            ensure!(
                style & (ES_PASSWORD | ES_READONLY) as u32 == 0,
                "Password/read-only Edit cannot receive background text or keys"
            );
        }
        Ok((handle, point))
    }
}

impl SelectedInput {
    fn valid(&self, target: &Target) -> bool {
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(self.hwnd, Some(&mut pid));
        }
        unsafe { IsWindow(Some(self.hwnd)) }.as_bool()
            && pid == self.pid
            && window_class(self.hwnd) == self.class
            && unsafe { GetAncestor(self.hwnd, GA_ROOT) } == target.hwnd
            && rect(self.hwnd).is_ok_and(|bounds| same_rect(bounds, self.rect))
    }
}
fn contains(rect: RECT, point: POINT) -> bool {
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}
fn client_point(handle: HWND, screen: POINT) -> Result<POINT> {
    let mut point = screen;
    ensure!(
        unsafe { ScreenToClient(handle, &mut point) }.as_bool(),
        "Cannot translate background control coordinates"
    );
    let mut bounds = RECT::default();
    unsafe {
        GetClientRect(handle, &mut bounds)?;
    }
    ensure!(
        contains(bounds, point),
        "Background messages only target the observed client area"
    );
    Ok(point)
}
fn child_at_point(root: HWND, screen: POINT) -> Result<HWND> {
    let mut target = root;
    for _ in 0..16 {
        let local = client_point(target, screen)?;
        let child = unsafe {
            ChildWindowFromPointEx(
                target,
                local,
                CWP_SKIPINVISIBLE | CWP_SKIPDISABLED | CWP_SKIPTRANSPARENT,
            )
        };
        if child.0.is_null() || child == target {
            return Ok(target);
        }
        ensure!(
            unsafe { GetAncestor(child, GA_ROOT) } == root,
            "Background child escaped the selected window"
        );
        target = child;
    }
    bail!("Background window nesting exceeds the observation limit")
}
fn packed_point(point: POINT) -> Result<isize> {
    ensure!(
        i16::try_from(point.x).is_ok() && i16::try_from(point.y).is_ok(),
        "Background message coordinate exceeds Windows signed 16-bit range"
    );
    Ok(((point.x as u16 as u32) | ((point.y as u16 as u32) << 16)) as isize)
}

unsafe extern "system" fn enumerate(hwnd: HWND, parameter: LPARAM) -> BOOL {
    // SAFETY: EnumWindows is synchronous and receives a live Vec<Target> pointer.
    let windows = unsafe { &mut *(parameter.0 as *mut Vec<Target>) };
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() || unsafe { IsIconic(hwnd) }.as_bool() {
        return BOOL(1);
    }
    if window_class(hwnd) == crate::pointer::CLASS {
        return BOOL(1);
    }
    let mut buffer = [0u16; 512];
    let count = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    if count <= 0 {
        return BOOL(1);
    }
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    windows.push(Target {
        hwnd,
        pid,
        title: String::from_utf16_lossy(&buffer[..count as usize]),
        class: window_class(hwnd),
    });
    BOOL(1)
}
fn window_class(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    let count = unsafe { GetClassNameW(hwnd, &mut buffer) };
    String::from_utf16_lossy(&buffer[..count.max(0) as usize])
}
fn rect(hwnd: HWND) -> Result<RECT> {
    let mut rect = RECT::default();
    unsafe {
        GetWindowRect(hwnd, &mut rect)?;
    }
    ensure!(
        rect.right > rect.left && rect.bottom > rect.top,
        "Window has no visible size"
    );
    Ok(rect)
}
fn rect_json(rect: RECT) -> Value {
    json!({"x":rect.left,"y":rect.top,"width":rect.right-rect.left,"height":rect.bottom-rect.top})
}
fn same_rect(a: RECT, b: RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}
fn clip(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .take(limit)
        .collect()
}

struct InputLease(HANDLE);
impl InputLease {
    fn acquire() -> Result<Self> {
        unsafe {
            let handle = CreateMutexW(None, false, w!("Local\\DSHNativeComputerInput"))?;
            let waited = WaitForSingleObject(handle, 0);
            if waited != WAIT_OBJECT_0 && waited != WAIT_ABANDONED {
                let _ = CloseHandle(handle);
                bail!("Another DSH computer action is in progress");
            }
            Ok(Self(handle))
        }
    }
}
impl Drop for InputLease {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}
fn standard_edit(node: &Node, target: &Target) -> Result<HWND> {
    ensure!(
        !node.password,
        "Password controls are redacted; enter them manually"
    );
    let handle = unsafe { node.element.CurrentNativeWindowHandle()? };
    ensure!(!handle.0.is_null() && window_class(handle).eq_ignore_ascii_case("Edit") && unsafe {GetAncestor(handle,GA_ROOT)}==target.hwnd,
        "This control does not support verified background Edit messages; no focus-changing UIA SetValue or physical input fallback is used");
    ensure!(
        unsafe { GetWindowLongPtrW(handle, GWL_STYLE) } as u32 & ES_READONLY as u32 == 0,
        "Edit control is read-only"
    );
    Ok(handle)
}
fn background_button(element: &IUIAutomationElement, target: &Target) -> Result<HWND> {
    let handle = unsafe { element.CurrentNativeWindowHandle()? };
    ensure!(
        !handle.0.is_null()
            && window_class(handle).eq_ignore_ascii_case("Button")
            && unsafe { GetAncestor(handle, GA_ROOT) } == target.hwnd,
        "This control has no verified background invocation; UIA Invoke may steal foreground focus, so no physical fallback is used"
    );
    let kind = unsafe { GetWindowLongPtrW(handle, GWL_STYLE) } as u32 & BS_TYPEMASK as u32;
    ensure!(
        kind == BS_PUSHBUTTON as u32 || kind == BS_DEFPUSHBUTTON as u32,
        "Only standard push buttons support verified background invocation"
    );
    Ok(handle)
}
fn invoke_background_button(button: HWND) -> Result<()> {
    // UIA's Win32 Invoke provider activates the target window. BM_CLICK can also
    // synthesize focus-changing mouse messages. Deliver only the documented
    // push-button command notification to its own parent instead.
    let parent = unsafe { GetParent(button)? };
    let control_id = unsafe { GetDlgCtrlID(button) };
    ensure!(control_id >= 0, "Button has no valid command identifier");
    control_message(
        parent,
        WM_COMMAND,
        control_id as u16 as usize,
        button.0 as isize,
    )?;
    Ok(())
}
fn is_multiline(handle: HWND) -> bool {
    (unsafe { GetWindowLongPtrW(handle, GWL_STYLE) }) as u32 & ES_MULTILINE as u32 != 0
}
fn control_message(handle: HWND, message: u32, wparam: usize, lparam: isize) -> Result<usize> {
    let mut result = 0usize;
    ensure!(
        unsafe {
            SendMessageTimeoutW(
                handle,
                message,
                WPARAM(wparam),
                LPARAM(lparam),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                500,
                Some(&mut result),
            )
        }
        .0 != 0,
        "Background control delivery could not be confirmed (the window may have timed out, closed, or rejected the message). {UNKNOWN_INPUT_OUTCOME}"
    );
    Ok(result)
}
fn replace_selection(edit: HWND, text: &str) -> Result<()> {
    let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    control_message(edit, EM_REPLACESEL, 1, text.as_ptr() as isize)?;
    Ok(())
}
fn background_key(edit: HWND, key: &str, node: &Node) -> Result<()> {
    let keys = parse_key(key)?;
    let len = control_message(edit, WM_GETTEXTLENGTH, 0, 0)?;
    ensure!(
        len <= 32768,
        "Background key editing is limited to 32768 UTF-16 units; use set_value"
    );
    let selection = control_message(edit, EM_GETSEL, 0, 0)?;
    let start = (selection & 0xffff).min(len);
    let end = ((selection >> 16) & 0xffff).min(len);
    let value: IUIAutomationValuePattern =
        unsafe { node.element.GetCurrentPatternAs(UIA_ValuePatternId)? };
    let text: Vec<_> = unsafe { value.CurrentValue()? }
        .to_string()
        .encode_utf16()
        .collect();
    let previous = |position: usize| {
        if position >= 2
            && text
                .get(position - 1)
                .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
        {
            position - 2
        } else {
            position.saturating_sub(1)
        }
    };
    let next = |position: usize| {
        if text
            .get(position)
            .is_some_and(|unit| (0xd800..=0xdbff).contains(unit))
        {
            (position + 2).min(len)
        } else {
            (position + 1).min(len)
        }
    };
    let (from,to)=match keys.as_slice() {
        [0x11,0x41] => (0,len), [0x24] => (0,0), [0x23] => (len,len),
        [0x10,0x24] => (start,0), [0x10,0x23] => (start,len),
        [0x25] => {let at=if start!=end {start}else {previous(start)};(at,at)},
        [0x27] => {let at=if start!=end {end}else {next(end)};(at,at)},
        [0x10,0x25] => (start,previous(end)), [0x10,0x27] => (start,next(end)),
        [0x08] | [0x2e] => {let (from,to)=if start!=end {(start,end)}else if keys[0]==0x08 {(previous(start),end)}else {(start,next(end))};control_message(edit,EM_SETSEL,from,to as isize)?;return replace_selection(edit,"");},
        [0x0d] if is_multiline(edit) => return replace_selection(edit,"\r\n"),
        _=>bail!("Background Edit keys support Ctrl+A, Home, End, Shift+Home/End, Left/Right, Backspace/Delete, and Enter for multiline fields; no global key injection is used"),
    };
    control_message(edit, EM_SETSEL, from, to as isize)?;
    Ok(())
}

struct CaptureSurface {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}
impl Drop for CaptureSurface {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.0.is_null() {
                SelectObject(self.memory, self.previous);
            }
            if !self.bitmap.0.is_null() {
                let _ = DeleteObject(self.bitmap.into());
            }
            if !self.memory.0.is_null() {
                let _ = DeleteDC(self.memory);
            }
            if !self.screen.0.is_null() {
                ReleaseDC(None, self.screen);
            }
        }
    }
}
fn capture(target: &Target, snapshot: &Snapshot, request: &Request) -> Result<(Vec<u8>, u32, u32)> {
    let width = (snapshot.rect.right - snapshot.rect.left) as u32;
    let height = (snapshot.rect.bottom - snapshot.rect.top) as u32;
    ensure!(
        (width as u64) * (height as u64) <= 16_000_000,
        "Selected window is too large to capture"
    );
    let mut surface = CaptureSurface {
        screen: unsafe { GetDC(None) },
        memory: HDC::default(),
        bitmap: HBITMAP::default(),
        previous: HGDIOBJ::default(),
    };
    ensure!(
        !surface.screen.0.is_null(),
        "No interactive desktop is available"
    );
    surface.memory = unsafe { CreateCompatibleDC(Some(surface.screen)) };
    ensure!(
        !surface.memory.0.is_null(),
        "Could not create capture surface"
    );
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = std::ptr::null_mut();
    surface.bitmap = unsafe {
        CreateDIBSection(
            Some(surface.screen),
            &info,
            DIB_RGB_COLORS,
            &mut pixels,
            None,
            0,
        )?
    };
    surface.previous = unsafe { SelectObject(surface.memory, surface.bitmap.into()) };
    request.alive()?;
    // PrintWindow is a synchronous OS call with no cancellation API. Reject
    // already-hung UI threads before entering it; the request/reply deadline
    // still bounds the caller if a previously responsive provider stalls.
    let mut response = 0usize;
    ensure!(
        unsafe {
            SendMessageTimeoutW(
                target.hwnd,
                WM_NULL,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                300,
                Some(&mut response),
            )
        }
        .0 != 0,
        "Target is not responding; screenshot was not attempted"
    );
    request.alive()?;
    ensure!(
        unsafe { PrintWindow(target.hwnd, surface.memory, PRINT_WINDOW_FLAGS(2)) }.as_bool(),
        "This window cannot be captured with Windows PrintWindow"
    );
    request.alive()?;
    ensure!(!pixels.is_null(), "Capture pixels unavailable");
    let mut rgba =
        unsafe { std::slice::from_raw_parts(pixels as *const u8, (width * height * 4) as usize) }
            .to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    // Mask password controls even if an unusual provider draws plaintext.
    for node in snapshot.nodes.iter().filter(|node| node.password) {
        let left = (node.rect.left - snapshot.rect.left).clamp(0, width as i32);
        let right = (node.rect.right - snapshot.rect.left).clamp(0, width as i32);
        let top = (node.rect.top - snapshot.rect.top).clamp(0, height as i32);
        let bottom = (node.rect.bottom - snapshot.rect.top).clamp(0, height as i32);
        for y in top..bottom {
            for x in left..right {
                let index = ((y as u32 * width + x as u32) * 4) as usize;
                rgba[index..index + 4].copy_from_slice(&[20, 24, 26, 255]);
            }
        }
    }
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&rgba)?;
    }
    Ok((output, width, height))
}

#[cfg(test)]
mod tests;
