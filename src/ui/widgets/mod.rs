//! 组件库 v3：chips（工具条/按钮/开关）+ searchbox（搜索框）。
//! 实现自旧 menu/chips.rs、menu/searchbox.rs 物理迁入。

pub mod chips;
pub mod dock;
pub mod searchbox;

#[allow(unused_imports)] // 兼容旧引用路径
pub use chips::{
    icon_button_rect, toolbar_rects, IconButton, PillChip, ToggleRow, ToolChip, ICON_BUTTON_GAP,
    ICON_BUTTON_HEIGHT, ICON_BUTTON_WIDTH, TOOL_CHIP_GAP, TOOL_CHIP_HEIGHT, TOOL_CHIP_WIDTH,
};
