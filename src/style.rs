use crate::util::sanitize_ascii;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;

const DEFAULT_OVERLAY_WIDTH: u16 = 600;
const DEFAULT_ROW_HEIGHT: u16 = 56;
const DEFAULT_PADDING: u16 = 16;
const DEFAULT_SCREEN_MARGIN: u16 = 96;
const DEFAULT_ICON_MAX_SIZE: u16 = 40;
const DEFAULT_ICON_MARGIN: u16 = 8;
const DEFAULT_VERTICAL_TEXT_GAP: i16 = 8;
const DEFAULT_VERTICAL_TEXT_BASELINE: i16 = 34;
const DEFAULT_HORIZONTAL_ITEM_WIDTH: u16 = 120;
const DEFAULT_HORIZONTAL_ITEM_HEIGHT: u16 = 92;
const DEFAULT_HORIZONTAL_TEXT_OFFSET: i16 = 8;
const DEFAULT_HORIZONTAL_TEXT_BASELINE: i16 = 82;
const DEFAULT_HORIZONTAL_CHAR_WIDTH: u16 = 7;
const DEFAULT_OVERLAY_BORDER_WIDTH: u16 = 0;
const DEFAULT_OVERLAY_BORDER_COLOR: u32 = 0xFFFFFF;
const DEFAULT_ITEM_BORDER_WIDTH: u16 = 0;
const DEFAULT_ITEM_BORDER_COLOR: u32 = 0xFFFFFF;
const DEFAULT_ITEM_SELECTED_BORDER_COLOR: u32 = 0xFFFFFF;

#[derive(Clone)]
pub struct Style {
    pub overlay_background: u32,
    pub highlight_background: u32,
    pub text_color: u32,
    pub text_selected_color: u32,
    pub overlay_width: u16,
    pub row_height: u16,
    pub padding: u16,
    pub screen_margin: u16,
    pub icon_max_size: u16,
    pub icon_margin: u16,
    pub vertical_text_gap: i16,
    pub vertical_text_baseline: i16,
    pub horizontal_item_width: u16,
    pub horizontal_item_height: u16,
    pub horizontal_text_offset: i16,
    pub horizontal_text_baseline: i16,
    pub horizontal_char_width_estimate: u16,
    pub overlay_border_width: u16,
    pub overlay_border_color: u32,
    pub item_border_width: u16,
    pub item_border_color: u32,
    pub item_selected_border_color: u32,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            overlay_background: 0x000000,
            highlight_background: 0xFFFFFF,
            text_color: 0xFFFFFF,
            text_selected_color: 0x000000,
            overlay_width: DEFAULT_OVERLAY_WIDTH,
            row_height: DEFAULT_ROW_HEIGHT,
            padding: DEFAULT_PADDING,
            screen_margin: DEFAULT_SCREEN_MARGIN,
            icon_max_size: DEFAULT_ICON_MAX_SIZE,
            icon_margin: DEFAULT_ICON_MARGIN,
            vertical_text_gap: DEFAULT_VERTICAL_TEXT_GAP,
            vertical_text_baseline: DEFAULT_VERTICAL_TEXT_BASELINE,
            horizontal_item_width: DEFAULT_HORIZONTAL_ITEM_WIDTH,
            horizontal_item_height: DEFAULT_HORIZONTAL_ITEM_HEIGHT,
            horizontal_text_offset: DEFAULT_HORIZONTAL_TEXT_OFFSET,
            horizontal_text_baseline: DEFAULT_HORIZONTAL_TEXT_BASELINE,
            horizontal_char_width_estimate: DEFAULT_HORIZONTAL_CHAR_WIDTH,
            overlay_border_width: DEFAULT_OVERLAY_BORDER_WIDTH,
            overlay_border_color: DEFAULT_OVERLAY_BORDER_COLOR,
            item_border_width: DEFAULT_ITEM_BORDER_WIDTH,
            item_border_color: DEFAULT_ITEM_BORDER_COLOR,
            item_selected_border_color: DEFAULT_ITEM_SELECTED_BORDER_COLOR,
        }
    }
}

impl Style {
    pub fn load_from_config() -> Result<Self> {
        let mut style = Self::default();
        if let Some(path) = default_style_path() {
            if path.exists() {
                let css = fs::read_to_string(&path)
                    .with_context(|| format!("failed to read css file {}", path.display()))?;
                let rules = parse_css_rules(&css)?;
                style.apply_rules(&rules)?;
            }
        }
        Ok(style)
    }

    pub fn icon_area(&self) -> u16 {
        self.icon_max_size + self.icon_margin * 2
    }

    pub fn vertical_text_offset(&self) -> i16 {
        self.icon_area() as i16 + self.vertical_text_gap
    }

    pub fn fit_horizontal_label(&self, title: &str, cell_width: u16) -> (String, u16) {
        let sanitized = sanitize_ascii(title);
        if sanitized.is_empty() {
            return (sanitized, 0);
        }

        let margin = (self.horizontal_text_offset as u16) * 2;
        if cell_width <= margin {
            return (String::new(), 0);
        }

        let available = cell_width.saturating_sub(margin);
        let mut max_chars =
            usize::from(available) / usize::from(self.horizontal_char_width_estimate.max(1));
        if max_chars == 0 {
            max_chars = 1;
        }

        let mut label = sanitized;
        if label.len() > max_chars {
            if max_chars <= 3 {
                label = ".".repeat(max_chars);
            } else {
                let keep = max_chars - 3;
                label.truncate(keep);
                label.push_str("...");
            }
        }

        let approx_width = (label.len() as u16)
            .saturating_mul(self.horizontal_char_width_estimate.max(1))
            .min(available);
        (label, approx_width)
    }

    fn apply_rules(&mut self, rules: &CssRules) -> Result<()> {
        if let Some(root) = rules.get(":root") {
            self.apply_root(root)?;
        }

        if let Some(overlay) = rules.get("overlay") {
            if let Some(value) = overlay.get("background") {
                self.overlay_background = parse_color(value)?;
            }
            if let Some(value) = overlay.get("width") {
                self.overlay_width = parse_u16_px(value)?;
            }
            if let Some(value) = overlay.get("padding") {
                self.padding = parse_u16_px(value)?;
            }
            if let Some(value) = overlay.get("screen-margin") {
                self.screen_margin = parse_u16_px(value)?;
            }
            if let Some(value) = overlay.get("border-width") {
                self.overlay_border_width = parse_u16_px(value)?;
            }
            if let Some(value) = overlay.get("border-color") {
                self.overlay_border_color = parse_color(value)?;
            }
        }

        if let Some(item) = rules.get("item") {
            if let Some(value) = item.get("height") {
                self.row_height = parse_u16_px(value)?;
            }
            if let Some(value) = item.get("icon-size") {
                self.icon_max_size = parse_u16_px(value)?;
            }
            if let Some(value) = item.get("icon-margin") {
                self.icon_margin = parse_u16_px(value)?;
            }
            if let Some(value) = item.get("border-width") {
                self.item_border_width = parse_u16_px(value)?;
            }
            if let Some(value) = item.get("border-color") {
                self.item_border_color = parse_color(value)?;
            }
        }

        if let Some(selected) = rules.get("item:selected") {
            if let Some(value) = selected.get("background") {
                self.highlight_background = parse_color(value)?;
            }
            if let Some(value) = selected.get("color") {
                self.text_selected_color = parse_color(value)?;
            }
            if let Some(value) = selected.get("border-color") {
                self.item_selected_border_color = parse_color(value)?;
            }
        }

        if let Some(label) = rules.get("label") {
            if let Some(value) = label.get("color") {
                self.text_color = parse_color(value)?;
            }
        }

        if let Some(label_sel) = rules.get("label:selected") {
            if let Some(value) = label_sel.get("color") {
                self.text_selected_color = parse_color(value)?;
            }
        }

        if let Some(horizontal) = rules.get("horizontal") {
            if let Some(value) = horizontal.get("item-width") {
                self.horizontal_item_width = parse_u16_px(value)?;
            }
            if let Some(value) = horizontal.get("item-height") {
                self.horizontal_item_height = parse_u16_px(value)?;
            }
            if let Some(value) = horizontal.get("text-offset") {
                self.horizontal_text_offset = parse_i16_px(value)?;
            }
            if let Some(value) = horizontal.get("text-baseline") {
                self.horizontal_text_baseline = parse_i16_px(value)?;
            }
            if let Some(value) = horizontal.get("char-width") {
                self.horizontal_char_width_estimate = parse_u16_px(value)?;
            }
        }

        if let Some(vertical) = rules.get("vertical") {
            if let Some(value) = vertical.get("text-gap") {
                self.vertical_text_gap = parse_i16_px(value)?;
            }
            if let Some(value) = vertical.get("text-baseline") {
                self.vertical_text_baseline = parse_i16_px(value)?;
            }
        }

        Ok(())
    }

    fn apply_root(&mut self, declarations: &CssDeclarations) -> Result<()> {
        for (name, value) in declarations {
            match name.as_str() {
                "--overlay-background" => self.overlay_background = parse_color(value)?,
                "--highlight-background" => self.highlight_background = parse_color(value)?,
                "--text-color" => self.text_color = parse_color(value)?,
                "--text-selected-color" => {
                    self.text_selected_color = parse_color(value)?;
                }
                "--overlay-width" => self.overlay_width = parse_u16_px(value)?,
                "--padding" => self.padding = parse_u16_px(value)?,
                "--screen-margin" => self.screen_margin = parse_u16_px(value)?,
                "--row-height" => self.row_height = parse_u16_px(value)?,
                "--icon-size" => self.icon_max_size = parse_u16_px(value)?,
                "--icon-margin" => self.icon_margin = parse_u16_px(value)?,
                "--overlay-border-width" => {
                    self.overlay_border_width = parse_u16_px(value)?;
                }
                "--overlay-border-color" => {
                    self.overlay_border_color = parse_color(value)?;
                }
                "--vertical-text-gap" => self.vertical_text_gap = parse_i16_px(value)?,
                "--vertical-text-baseline" => {
                    self.vertical_text_baseline = parse_i16_px(value)?;
                }
                "--horizontal-item-width" => {
                    self.horizontal_item_width = parse_u16_px(value)?;
                }
                "--horizontal-item-height" => {
                    self.horizontal_item_height = parse_u16_px(value)?;
                }
                "--horizontal-text-offset" => {
                    self.horizontal_text_offset = parse_i16_px(value)?;
                }
                "--horizontal-text-baseline" => {
                    self.horizontal_text_baseline = parse_i16_px(value)?;
                }
                "--horizontal-char-width" => {
                    self.horizontal_char_width_estimate = parse_u16_px(value)?;
                }
                "--item-border-width" => {
                    self.item_border_width = parse_u16_px(value)?;
                }
                "--item-border-color" => {
                    self.item_border_color = parse_color(value)?;
                }
                "--item-selected-border-color" => {
                    self.item_selected_border_color = parse_color(value)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

pub fn default_style_path() -> Option<PathBuf> {
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME") {
        let mut path = PathBuf::from(config_home);
        path.push("xwitcher");
        path.push("style.css");
        return Some(path);
    }

    if let Some(home) = env::var_os("HOME") {
        let mut path = PathBuf::from(home);
        path.push(".config");
        path.push("xwitcher");
        path.push("style.css");
        return Some(path);
    }

    None
}

type CssDeclarations = HashMap<String, String>;
type CssRules = HashMap<String, CssDeclarations>;

fn parse_css_rules(source: &str) -> Result<CssRules> {
    let mut rules = HashMap::new();
    let cleaned = strip_css_comments(source);
    let chars: Vec<char> = cleaned.chars().collect();
    let mut idx = 0usize;

    while idx < chars.len() {
        while idx < chars.len() && chars[idx].is_whitespace() {
            idx += 1;
        }
        if idx >= chars.len() {
            break;
        }

        let selector_start = idx;
        while idx < chars.len() && chars[idx] != '{' {
            idx += 1;
        }
        if idx >= chars.len() {
            break;
        }
        let selector_raw: String = chars[selector_start..idx].iter().collect();
        let selectors: Vec<String> = selector_raw
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        idx += 1; // skip '{'

        let block_start = idx;
        let mut depth = 1;
        while idx < chars.len() && depth > 0 {
            match chars[idx] {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            idx += 1;
        }
        if depth != 0 {
            anyhow::bail!("unmatched braces in css");
        }
        let block_end = idx.saturating_sub(1);
        let block: String = chars[block_start..block_end].iter().collect();
        let declarations = parse_declarations(&block);

        for selector in selectors {
            if selector.is_empty() {
                continue;
            }
            let entry = rules.entry(selector).or_insert_with(HashMap::new);
            for (prop, value) in &declarations {
                entry.insert(prop.clone(), value.clone());
            }
        }
    }

    Ok(rules)
}

fn parse_declarations(block: &str) -> CssDeclarations {
    let mut map = HashMap::new();
    for declaration in block.split(';') {
        let decl = declaration.trim();
        if decl.is_empty() {
            continue;
        }
        if let Some(pos) = decl.find(':') {
            let name = decl[..pos].trim().to_lowercase();
            let value = decl[pos + 1..]
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            if !name.is_empty() && !value.is_empty() {
                map.insert(name, value);
            }
        }
    }
    map
}

fn strip_css_comments(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'/' && idx + 1 < bytes.len() && bytes[idx + 1] == b'*' {
            idx += 2;
            while idx + 1 < bytes.len() {
                if bytes[idx] == b'*' && bytes[idx + 1] == b'/' {
                    idx += 2;
                    break;
                }
                idx += 1;
            }
        } else {
            result.push(bytes[idx] as char);
            idx += 1;
        }
    }
    result
}

fn parse_color(value: &str) -> Result<u32> {
    let trimmed = value.trim();
    if let Some(hex) = trimmed.strip_prefix('#') {
        let hex = hex.trim();
        let digits = hex.len();
        let parsed =
            u32::from_str_radix(hex, 16).with_context(|| format!("invalid hex color: {value}"))?;
        return Ok(match digits {
            3 => {
                let r = ((parsed >> 8) & 0xF) as u32;
                let g = ((parsed >> 4) & 0xF) as u32;
                let b = (parsed & 0xF) as u32;
                (r * 17 << 16) | (g * 17 << 8) | (b * 17)
            }
            4 => {
                let r = ((parsed >> 12) & 0xF) as u32;
                let g = ((parsed >> 8) & 0xF) as u32;
                let b = ((parsed >> 4) & 0xF) as u32;
                ((r * 17) << 16) | ((g * 17) << 8) | (b * 17)
            }
            6 => parsed,
            8 => parsed >> 8, // drop alpha
            _ => anyhow::bail!("unsupported hex color length: #{hex}"),
        });
    }

    if let Some(body) = trimmed
        .strip_prefix("rgb(")
        .and_then(|v| v.strip_suffix(')'))
    {
        let parts: Vec<&str> = body.split(',').map(|p| p.trim()).collect();
        if parts.len() != 3 {
            anyhow::bail!("rgb() expects three components: {value}");
        }
        let mut rgb = [0u32; 3];
        for (idx, part) in parts.iter().enumerate() {
            let component: f64 = part
                .parse()
                .with_context(|| format!("invalid rgb component: {part}"))?;
            if !(0.0..=255.0).contains(&component) {
                anyhow::bail!("rgb component out of range: {part}");
            }
            rgb[idx] = component.round() as u32;
        }
        return Ok((rgb[0] << 16) | (rgb[1] << 8) | rgb[2]);
    }

    match trimmed.to_ascii_lowercase().as_str() {
        "black" => Ok(0x000000),
        "white" => Ok(0xFFFFFF),
        "red" => Ok(0xFF0000),
        "green" => Ok(0x008000),
        "blue" => Ok(0x0000FF),
        other => anyhow::bail!("unsupported color value: {other}"),
    }
}

fn parse_u16_px(value: &str) -> Result<u16> {
    let trimmed = value.trim();
    let numeric_str = if trimmed.to_ascii_lowercase().ends_with("px") {
        &trimmed[..trimmed.len() - 2]
    } else {
        trimmed
    }
    .trim();
    let parsed: f64 = numeric_str
        .parse()
        .with_context(|| format!("invalid length value: {value}"))?;
    if parsed < 0.0 {
        anyhow::bail!("length cannot be negative: {value}");
    }
    if parsed > u16::MAX as f64 {
        anyhow::bail!("length too large: {value}");
    }
    Ok(parsed.round() as u16)
}

fn parse_i16_px(value: &str) -> Result<i16> {
    let trimmed = value.trim();
    let numeric_str = if trimmed.to_ascii_lowercase().ends_with("px") {
        &trimmed[..trimmed.len() - 2]
    } else {
        trimmed
    }
    .trim();
    let parsed: f64 = numeric_str
        .parse()
        .with_context(|| format!("invalid length value: {value}"))?;
    if parsed < i16::MIN as f64 || parsed > i16::MAX as f64 {
        anyhow::bail!("length out of range: {value}");
    }
    Ok(parsed.round() as i16)
}
