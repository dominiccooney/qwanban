//! Input injection + Anthropic computer-use action mapping (§input-injection).
//! `cuxec` (the computer-use executor) maps Anthropic `computer_20250124`
//! actions to normalized `InputEvent`s, scales coordinates API↔screen, and
//! injects via the per-OS backend.

use crate::id::InputEventId;
use crate::timeline::TimelineNs;
use serde::{Deserialize, Serialize};

/// A normalized input event after coordinate scaling (internal to cuxec).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputEvent {
    pub event_id: InputEventId,
    pub kind: InputKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputKind {
    MouseMove { x: i32, y: i32 },
    MouseButton {
        x: Option<i32>,
        y: Option<i32>,
        button: Button,
        action: BtnAction,
    },
    Drag { x1: i32, y1: i32, x2: i32, y2: i32 },
    Scroll {
        x: i32,
        y: i32,
        dir: ScrollDir,
        amount: u32,
    },
    TypeText { text: String },
    KeyChord { combo: String },
    HoldKey { combo: String, duration_s: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BtnAction {
    Down,
    Up,
    Click,
    DoubleClick,
    TripleClick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrollDir {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputAck {
    pub event_id: InputEventId,
    pub injected_ts: TimelineNs,
    pub ok: bool,
}

/// The Anthropic computer-use action (as the model sends it). `cuxec` executes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputerAction {
    pub action: ComputerActionKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ComputerActionKind {
    Screenshot,
    LeftClick { coordinate: (i32, i32), key: Option<String> },
    RightClick { coordinate: (i32, i32), key: Option<String> },
    MiddleClick { coordinate: (i32, i32), key: Option<String> },
    DoubleClick { coordinate: (i32, i32), key: Option<String> },
    TripleClick { coordinate: (i32, i32), key: Option<String> },
    LeftClickDrag { start_coordinate: (i32, i32), coordinate: (i32, i32) },
    LeftMouseDown,
    LeftMouseUp,
    MouseMove { coordinate: (i32, i32) },
    CursorPosition,
    Scroll {
        coordinate: (i32, i32),
        scroll_direction: ScrollDir,
        scroll_amount: u32,
        text: Option<String>,
    },
    Type { text: String },
    Key { text: String },
    HoldKey { text: String, duration: f32 },
    Wait { duration: f32 },
}

/// The tool_result cuxec returns to the agent loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub image_base64: Option<String>,
    pub text: Option<String>,
    pub error: Option<String>,
}

/// A scaled display resolution advertised to the model (§input-injection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}
