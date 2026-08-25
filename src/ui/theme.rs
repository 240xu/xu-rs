//! 设计令牌唯一来源（方向 C · 克制派）。组件禁止裸写 Color::。
#![allow(dead_code)] // 桥接期：M2+ 逐屏消费后移除

use ratatui::style::Color;

/// 页面底色之上的选中/焦点面板明度。
pub const PANEL_BG: Color = Color::Rgb(30, 33, 48);

/// 选中/按压卡片的强调底色（比 PANEL_BG 亮一档）。
pub const SEL_BG: Color = Color::Rgb(36, 41, 60);

/// Base fill for controls that should remain visually quiet.
pub const BASE_BG: Color = Color::Reset;

/// 三端身份色：OpenCode=青 · Claude=品红 · Codex=黄。
pub const IDENTITY: [Color; 3] = [Color::Cyan, Color::Magenta, Color::Yellow];

/// 语义色。
pub const OK: Color = Color::Green;
pub const WARN: Color = Color::Yellow;
pub const BAD: Color = Color::Red;
pub const ACCENT: Color = Color::Cyan;

/// 灰阶（信息密度阶梯）。
pub const INK: Color = Color::White;
pub const G1: Color = Color::Gray;
pub const G2: Color = Color::DarkGray;

/// 字符词汇。
pub const BAR_FOCUS: &str = "▍";
pub const STATE_ON: &str = "█";
pub const STATE_OFF: &str = "░";
pub const UNDERLINE: &str = "━";

/// 触屏命中下限（字符行）。
pub const MIN_HIT_ROWS: u16 = 3;
