use anyhow::{Context, Result};
use std::cmp::{Ordering, max, min};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use x11::keysym::{XK_Alt_L, XK_Alt_R, XK_Escape, XK_Tab};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{
    CURRENT_TIME, NONE,
    protocol::{
        Event,
        xproto::{
            Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageData, ClientMessageEvent,
            EventMask, Gcontext, GrabMode, ImageFormat, ImageOrder, KeyButMask, KeyPressEvent,
            KeyReleaseEvent, Keycode, MapState, ModMask, PropMode, PropertyNotifyEvent, Rectangle,
            Window, WindowClass,
        },
    },
    rust_connection::RustConnection,
};

const MAX_ICON_SEARCH_DEPTH: u8 = 5;

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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Layout {
    Horizontal,
    Vertical,
}

impl Default for Layout {
    fn default() -> Self {
        Layout::Horizontal
    }
}

struct CliOptions {
    layout: Layout,
}

fn parse_cli_options<I>(args: I) -> Result<CliOptions>
where
    I: IntoIterator<Item = String>,
{
    let mut layout = Layout::default();

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--horizontal" => layout = Layout::Horizontal,
            "-v" | "--vertical" => layout = Layout::Vertical,
            other => anyhow::bail!("unknown option: {other}"),
        }
    }

    Ok(CliOptions { layout })
}

#[derive(Clone)]
struct Style {
    overlay_background: u32,
    highlight_background: u32,
    text_color: u32,
    text_selected_color: u32,
    overlay_width: u16,
    row_height: u16,
    padding: u16,
    screen_margin: u16,
    icon_max_size: u16,
    icon_margin: u16,
    vertical_text_gap: i16,
    vertical_text_baseline: i16,
    horizontal_item_width: u16,
    horizontal_item_height: u16,
    horizontal_text_offset: i16,
    horizontal_text_baseline: i16,
    horizontal_char_width_estimate: u16,
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
        }
    }
}

impl Style {
    fn icon_area(&self) -> u16 {
        self.icon_max_size + self.icon_margin * 2
    }

    fn vertical_text_offset(&self) -> i16 {
        self.icon_area() as i16 + self.vertical_text_gap
    }

    fn fit_horizontal_label(&self, title: &str, cell_width: u16) -> (String, u16) {
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

    fn load_from_config() -> Result<Self> {
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
        }

        if let Some(selected) = rules.get("item:selected") {
            if let Some(value) = selected.get("background") {
                self.highlight_background = parse_color(value)?;
            }
            if let Some(value) = selected.get("color") {
                self.text_selected_color = parse_color(value)?;
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
                _ => {}
            }
        }
        Ok(())
    }
}

fn default_style_path() -> Option<PathBuf> {
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

#[allow(non_snake_case)]
struct Atoms {
    _NET_ACTIVE_WINDOW: Atom,
    _NET_CLIENT_LIST: Atom,
    _NET_CLIENT_LIST_STACKING: Atom,
    _NET_WM_NAME: Atom,
    _NET_WM_VISIBLE_NAME: Atom,
    _NET_WM_WINDOW_TYPE: Atom,
    _NET_WM_WINDOW_TYPE_NOTIFICATION: Atom,
    _NET_WM_ICON: Atom,
    UTF8_STRING: Atom,
    WM_CLASS: Atom,
    WM_NAME: Atom,
}

impl Atoms {
    fn new(conn: &RustConnection) -> Result<Self> {
        Ok(Self {
            _NET_ACTIVE_WINDOW: intern_atom(conn, "_NET_ACTIVE_WINDOW")?,
            _NET_CLIENT_LIST: intern_atom(conn, "_NET_CLIENT_LIST")?,
            _NET_CLIENT_LIST_STACKING: intern_atom(conn, "_NET_CLIENT_LIST_STACKING")?,
            _NET_WM_NAME: intern_atom(conn, "_NET_WM_NAME")?,
            _NET_WM_VISIBLE_NAME: intern_atom(conn, "_NET_WM_VISIBLE_NAME")?,
            _NET_WM_WINDOW_TYPE: intern_atom(conn, "_NET_WM_WINDOW_TYPE")?,
            _NET_WM_WINDOW_TYPE_NOTIFICATION: intern_atom(
                conn,
                "_NET_WM_WINDOW_TYPE_NOTIFICATION",
            )?,
            _NET_WM_ICON: intern_atom(conn, "_NET_WM_ICON")?,
            UTF8_STRING: intern_atom(conn, "UTF8_STRING")?,
            WM_CLASS: intern_atom(conn, "WM_CLASS")?,
            WM_NAME: intern_atom(conn, "WM_NAME")?,
        })
    }
}

fn main() -> Result<()> {
    let options = parse_cli_options(env::args().skip(1))?;
    let style = Style::load_from_config()?;
    let (conn, screen_num) = x11rb::connect(None).context("failed to connect to X server")?;
    let atoms = Atoms::new(&conn).context("failed to intern atoms")?;
    let mut app = AltTab::new(conn, screen_num, atoms, options.layout, style)?;
    app.run()
}

struct AltTab {
    conn: RustConnection,
    screen_num: usize,
    atoms: Atoms,
    layout: Layout,
    style: Style,
    bindings: KeyBindings,
    state: Option<OverlayState>,
    icon_theme: IconTheme,
    mru: Vec<Window>,
}

impl AltTab {
    fn new(
        conn: RustConnection,
        screen_num: usize,
        atoms: Atoms,
        layout: Layout,
        style: Style,
    ) -> Result<Self> {
        let bindings = KeyBindings::load(&conn)?;
        let icon_theme = IconTheme::new(style.icon_max_size);
        let mut app = Self {
            conn,
            screen_num,
            atoms,
            layout,
            style,
            bindings,
            state: None,
            icon_theme,
            mru: Vec::new(),
        };
        app.register_root_events()?;
        app.refresh_active_window()?;
        app.grab_tab_keys()?;
        Ok(app)
    }

    fn run(&mut self) -> Result<()> {
        loop {
            match self
                .conn
                .wait_for_event()
                .context("failed waiting for X event")?
            {
                Event::KeyPress(event) => self.handle_key_press(event)?,
                Event::KeyRelease(event) => self.handle_key_release(event)?,
                Event::MappingNotify(_event) => {
                    self.bindings.refresh(&self.conn)?;
                    self.grab_tab_keys()?;
                }
                Event::Expose(event) => {
                    if self
                        .state
                        .as_ref()
                        .map_or(false, |state| state.overlay.window == event.window)
                    {
                        self.redraw_overlay()?;
                    }
                }
                Event::DestroyNotify(event) => {
                    if let Some(state) = &self.state {
                        if event.window == state.overlay.window {
                            // Overlay destroyed externally, drop state so we do not use freed resources.
                            self.state = None;
                        }
                    }
                }
                Event::PropertyNotify(event) => self.handle_property_notify(event)?,
                _ => {}
            }
        }
    }

    fn handle_key_press(&mut self, event: KeyPressEvent) -> Result<()> {
        if self.state.is_some() {
            if self.bindings.is_alt(event.detail) {
                if let Some(state) = self.state.as_mut() {
                    state.alt_count = state.alt_count.saturating_add(1);
                }
                return Ok(());
            }
            if self.bindings.is_tab(event.detail) {
                let shift_down = (u16::from(event.state) & u16::from(KeyButMask::SHIFT)) != 0;
                let direction = if shift_down {
                    Direction::Backward
                } else {
                    Direction::Forward
                };
                if let Some(state) = self.state.as_mut() {
                    state.advance(direction);
                }
                self.redraw_overlay()?;
                return Ok(());
            }
            if self.bindings.is_escape(event.detail) {
                self.finish_selection(false)?;
                return Ok(());
            }
            return Ok(());
        }

        if self.bindings.is_tab(event.detail)
            && (u16::from(event.state) & u16::from(KeyButMask::MOD1)) != 0
        {
            let shift_down = (u16::from(event.state) & u16::from(KeyButMask::SHIFT)) != 0;
            let direction = if shift_down {
                Direction::Backward
            } else {
                Direction::Forward
            };
            self.start_overlay(direction)?;
        }
        Ok(())
    }

    fn handle_key_release(&mut self, event: KeyReleaseEvent) -> Result<()> {
        if let Some(state) = self.state.as_mut() {
            if self.bindings.is_alt(event.detail) {
                if state.alt_count > 0 {
                    state.alt_count -= 1;
                }
                if state.alt_count == 0 {
                    self.finish_selection(true)?;
                }
            }
        }
        Ok(())
    }

    fn register_root_events(&self) -> Result<()> {
        let root = self.screen().root;
        self.conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        Ok(())
    }

    fn refresh_active_window(&mut self) -> Result<()> {
        if let Some(window) = self.get_active_window()? {
            self.update_mru(window);
        }
        Ok(())
    }

    fn update_mru(&mut self, window: Window) {
        if window == 0 {
            return;
        }
        if let Some(state) = &self.state {
            if window == state.overlay.window {
                return;
            }
        }
        self.mru.retain(|w| *w != window);
        self.mru.insert(0, window);
    }

    fn apply_mru_order(&self, entries: &mut Vec<WindowEntry>) {
        entries.sort_by(|a, b| {
            let ra = self.mru.iter().position(|w| *w == a.window);
            let rb = self.mru.iter().position(|w| *w == b.window);
            match (ra, rb) {
                (Some(ra), Some(rb)) => ra.cmp(&rb),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        });
    }

    fn handle_property_notify(&mut self, event: PropertyNotifyEvent) -> Result<()> {
        if event.atom == self.atoms._NET_ACTIVE_WINDOW {
            if let Some(window) = self.get_active_window()? {
                self.update_mru(window);
            }
        }
        Ok(())
    }

    fn start_overlay(&mut self, direction: Direction) -> Result<()> {
        if self.state.is_some() {
            return Ok(());
        }

        let windows = self.collect_windows()?;
        if windows.is_empty() {
            return Ok(());
        }

        let active = self.get_active_window()?;
        let mut entries = windows;
        if let Some(active) = active {
            if let Some(pos) = entries.iter().position(|entry| entry.window == active) {
                entries.rotate_left(pos);
            }
        }

        let overlay = self.create_overlay(entries.len())?;
        let mut state = OverlayState::new(entries, overlay);
        if state.windows.len() > 1 {
            state.advance(direction);
        }

        self.conn
            .grab_keyboard(
                false,
                self.screen().root,
                CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .context("grab_keyboard failed")?;

        // Assume one Alt key is depressed for the initial activation.
        state.alt_count = 1;
        self.state = Some(state);
        self.redraw_overlay()?;
        self.conn.flush()?;
        Ok(())
    }

    fn finish_selection(&mut self, accept: bool) -> Result<()> {
        if let Some(state) = self.state.take() {
            let _ = self.conn.ungrab_keyboard(CURRENT_TIME);
            self.destroy_overlay(&state.overlay)?;
            if accept {
                if let Some(target) = state.selected_window() {
                    self.focus_window(target)?;
                    self.update_mru(target);
                }
            }
            self.conn.flush()?;
        }
        Ok(())
    }

    fn redraw_overlay(&self) -> Result<()> {
        let state = match self.state.as_ref() {
            Some(state) => state,
            None => return Ok(()),
        };

        let background_rect = Rectangle {
            x: 0,
            y: 0,
            width: state.overlay.width,
            height: state.overlay.height,
        };
        self.conn.poly_fill_rectangle(
            state.overlay.window,
            state.overlay.background_gc,
            &[background_rect],
        )?;

        match state.overlay.layout {
            Layout::Vertical => {
                let padding = self.style.padding as i16;
                let row_height = self.style.row_height;
                let text_offset = self.style.vertical_text_offset();
                let text_baseline = self.style.vertical_text_baseline;
                for (idx, window_index) in state.visible_range().enumerate() {
                    let entry = &state.windows[window_index];
                    let rect_y = padding + (idx as i16) * row_height as i16;
                    let rect = Rectangle {
                        x: padding,
                        y: rect_y,
                        width: state.overlay.width.saturating_sub(self.style.padding * 2),
                        height: row_height,
                    };

                    let is_selected = window_index == state.current;
                    if is_selected {
                        self.conn.poly_fill_rectangle(
                            state.overlay.window,
                            state.overlay.highlight_gc,
                            &[rect],
                        )?;
                    }

                    if let Some(icon) = &entry.icon {
                        let icon_x = rect.x + self.style.icon_margin as i16;
                        let icon_y = rect.y + max(0, (row_height as i16 - icon.height as i16) / 2);
                        self.draw_icon(&state.overlay, icon, icon_x, icon_y, is_selected)?;
                    }

                    let gc = if is_selected {
                        state.overlay.selected_text_gc
                    } else {
                        state.overlay.text_gc
                    };

                    self.draw_text(
                        state.overlay.window,
                        gc,
                        rect.x + text_offset,
                        rect.y + text_baseline,
                        &entry.title,
                    )?;
                }
            }
            Layout::Horizontal => {
                let capacity = max(1, state.overlay.visible_capacity);
                let padding = self.style.padding;
                let available_width = state.overlay.width.saturating_sub(padding * 2);
                let mut cell_width_u32 = u32::from(self.style.horizontal_item_width);
                let available_width_u32 = u32::from(available_width);
                if cell_width_u32 * capacity as u32 > available_width_u32 {
                    cell_width_u32 = max(1, available_width_u32 / capacity as u32);
                }
                let cell_width = cell_width_u32 as u16;
                let icon_area = self.style.icon_area();
                let cell_height = max(icon_area, state.overlay.height.saturating_sub(padding * 2));
                let total_items_width = cell_width_u32 * capacity as u32;
                let extra_space = available_width_u32.saturating_sub(total_items_width);
                let leading_offset = padding as i16 + (extra_space / 2) as i16;

                for (idx, window_index) in state.visible_range().enumerate() {
                    let entry = &state.windows[window_index];
                    let cell_x = leading_offset + (idx as u32 * cell_width_u32) as i16;
                    let rect = Rectangle {
                        x: cell_x,
                        y: padding as i16,
                        width: cell_width,
                        height: cell_height,
                    };

                    let is_selected = window_index == state.current;
                    if is_selected {
                        self.conn.poly_fill_rectangle(
                            state.overlay.window,
                            state.overlay.highlight_gc,
                            &[rect],
                        )?;
                    }

                    if let Some(icon) = &entry.icon {
                        let icon_x =
                            cell_x + max(0, (cell_width as i32 - icon.width as i32) / 2) as i16;
                        let icon_y = padding as i16
                            + max(0, (icon_area as i32 - icon.height as i32) / 2) as i16;
                        self.draw_icon(&state.overlay, icon, icon_x, icon_y, is_selected)?;
                    }

                    let gc = if is_selected {
                        state.overlay.selected_text_gc
                    } else {
                        state.overlay.text_gc
                    };

                    let (label, approx_width) =
                        self.style.fit_horizontal_label(&entry.title, cell_width);
                    if !label.is_empty() && approx_width > 0 {
                        let centered_offset =
                            ((cell_width as i32 - i32::from(approx_width)) / 2).max(0) as i16;
                        let mut text_x = cell_x + centered_offset;
                        let min_x = cell_x + self.style.horizontal_text_offset;
                        if text_x < min_x {
                            text_x = min_x;
                        }
                        let max_x = cell_x + cell_width as i16 - self.style.horizontal_text_offset;
                        if text_x > max_x {
                            text_x = max_x;
                        }

                        self.draw_text(
                            state.overlay.window,
                            gc,
                            text_x,
                            padding as i16 + self.style.horizontal_text_baseline,
                            &label,
                        )?;
                    }
                }
            }
        }
        self.conn.flush()?;
        Ok(())
    }

    fn draw_text(&self, window: Window, gc: Gcontext, x: i16, y: i16, text: &str) -> Result<()> {
        let ascii = sanitize_ascii(text);
        let bytes = ascii.as_bytes();
        let truncated = if bytes.len() > u8::MAX as usize {
            &bytes[..u8::MAX as usize]
        } else {
            bytes
        };
        self.conn
            .image_text8(window, gc, x, y, truncated)
            .context("failed to draw text")?;
        Ok(())
    }

    fn draw_icon(
        &self,
        overlay: &OverlayWindow,
        icon: &Icon,
        x: i16,
        y: i16,
        selected: bool,
    ) -> Result<()> {
        if icon.pixels.is_empty() {
            return Ok(());
        }

        let width = icon.width;
        let height = icon.height;
        let little_endian = matches!(self.conn.setup().image_byte_order, ImageOrder::LSB_FIRST);
        let depth = self.screen().root_depth;
        let format = self
            .conn
            .setup()
            .pixmap_formats
            .iter()
            .find(|fmt| fmt.depth == depth)
            .with_context(|| format!("missing pixmap format for depth {depth}"))?;

        let bytes_per_pixel = usize::from(format.bits_per_pixel / 8);
        let pad = usize::from(format.scanline_pad / 8).max(1);
        let row_stride = ((width as usize * bytes_per_pixel + pad - 1) / pad) * pad;
        let mut data = vec![0u8; row_stride * height as usize];

        let bg = if selected {
            self.style.highlight_background
        } else {
            self.style.overlay_background
        };
        let bg_r = ((bg >> 16) & 0xff) as u32;
        let bg_g = ((bg >> 8) & 0xff) as u32;
        let bg_b = (bg & 0xff) as u32;

        for row in 0..height as usize {
            for col in 0..width as usize {
                let pixel = icon.pixels[row * width as usize + col];
                let alpha = ((pixel >> 24) & 0xff) as u32;
                let inv_alpha = 255 - alpha;

                let src_r = ((pixel >> 16) & 0xff) as u32;
                let src_g = ((pixel >> 8) & 0xff) as u32;
                let src_b = (pixel & 0xff) as u32;

                let out_r = ((src_r * alpha + bg_r * inv_alpha + 127) / 255) as u8;
                let out_g = ((src_g * alpha + bg_g * inv_alpha + 127) / 255) as u8;
                let out_b = ((src_b * alpha + bg_b * inv_alpha + 127) / 255) as u8;

                let offset = row * row_stride + col * bytes_per_pixel;
                if little_endian {
                    data[offset] = out_b;
                    if bytes_per_pixel > 1 {
                        data[offset + 1] = out_g;
                    }
                    if bytes_per_pixel > 2 {
                        data[offset + 2] = out_r;
                    }
                    if bytes_per_pixel > 3 {
                        data[offset + 3] = 0;
                    }
                } else {
                    let base = offset + bytes_per_pixel.saturating_sub(3);
                    data[base] = out_r;
                    if bytes_per_pixel > 1 {
                        data[base + 1] = out_g;
                    }
                    if bytes_per_pixel > 2 {
                        data[base + 2] = out_b;
                    }
                }
            }
        }

        self.conn.put_image(
            ImageFormat::Z_PIXMAP,
            overlay.window,
            overlay.icon_gc,
            width,
            height,
            x,
            y,
            0,
            depth,
            &data,
        )?;

        Ok(())
    }

    fn destroy_overlay(&self, overlay: &OverlayWindow) -> Result<()> {
        let _ = self.conn.free_gc(overlay.text_gc);
        let _ = self.conn.free_gc(overlay.selected_text_gc);
        let _ = self.conn.free_gc(overlay.highlight_gc);
        let _ = self.conn.free_gc(overlay.background_gc);
        let _ = self.conn.free_gc(overlay.icon_gc);
        let _ = self.conn.unmap_window(overlay.window);
        let _ = self.conn.destroy_window(overlay.window);
        Ok(())
    }

    fn create_overlay(&self, item_count: usize) -> Result<OverlayWindow> {
        let screen = self.screen();
        let padding = self.style.padding;
        let icon_area = self.style.icon_area();
        let screen_margin = self.style.screen_margin;
        let layout = self.layout;

        let (width_u16, height_u16, visible_capacity) = match layout {
            Layout::Vertical => {
                let width = min(self.style.overlay_width, screen.width_in_pixels);

                let mut full_height = u32::from(padding) * 2
                    + u32::from(self.style.row_height) * item_count.max(1) as u32;
                let max_height = screen.height_in_pixels.saturating_sub(screen_margin);
                if max_height > 0 {
                    full_height = min(full_height, u32::from(max_height));
                }
                let min_height = u32::from(padding) * 2 + u32::from(self.style.row_height.max(1));
                if full_height < min_height {
                    full_height = min_height;
                }

                let visible_rows = max(
                    1,
                    ((full_height.saturating_sub(u32::from(padding) * 2))
                        / u32::from(self.style.row_height.max(1))) as usize,
                );
                (width, full_height as u16, visible_rows)
            }
            Layout::Horizontal => {
                let screen_width = screen.width_in_pixels;
                let width_limit = if screen_width > screen_margin {
                    screen_width - screen_margin
                } else {
                    screen_width
                };
                let available_for_cols = width_limit.saturating_sub(padding * 2);
                let item_width = self.style.horizontal_item_width.max(1);
                let max_cols = max(
                    1,
                    (u32::from(available_for_cols) / u32::from(item_width)) as usize,
                );
                let effective_count = max(1, item_count);
                let visible_cols = min(max_cols, effective_count);

                let mut width =
                    (u32::from(padding) * 2) + u32::from(item_width) * visible_cols as u32;
                let screen_width_u32 = u32::from(screen_width);
                if width > screen_width_u32 {
                    width = screen_width_u32;
                }

                let desired_item_height = self.style.horizontal_item_height.max(icon_area);
                let mut height = (u32::from(padding) * 2) + u32::from(desired_item_height);
                let max_height = screen.height_in_pixels.saturating_sub(screen_margin);
                if max_height > 0 {
                    height = min(height, u32::from(max_height));
                }
                let min_height = (u32::from(padding) * 2) + u32::from(icon_area);
                if height < min_height {
                    height = min_height;
                }

                (width as u16, height as u16, visible_cols)
            }
        };

        let x = ((screen.width_in_pixels.saturating_sub(width_u16)) / 2) as i16;
        let y = ((screen.height_in_pixels.saturating_sub(height_u16)) / 2) as i16;

        let window = self
            .conn
            .generate_id()
            .context("failed to alloc window id")?;
        self.conn
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                screen.root,
                x,
                y,
                width_u16,
                height_u16,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &x11rb::protocol::xproto::CreateWindowAux::new()
                    .background_pixel(screen.black_pixel)
                    .event_mask(EventMask::EXPOSURE),
            )
            .context("failed to create overlay window")?;

        self.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().override_redirect(1),
        )?;

        let wm_class = b"rust-alttab\0RustAltTab\0";
        self.conn.change_property8(
            PropMode::REPLACE,
            window,
            self.atoms.WM_CLASS,
            x11rb::protocol::xproto::AtomEnum::STRING,
            wm_class,
        )?;

        self.conn.change_property32(
            PropMode::REPLACE,
            window,
            self.atoms._NET_WM_WINDOW_TYPE,
            x11rb::protocol::xproto::AtomEnum::ATOM,
            &[self.atoms._NET_WM_WINDOW_TYPE_NOTIFICATION],
        )?;

        self.conn.map_window(window)?;

        let background_gc = self.create_gc(
            window,
            self.style.overlay_background,
            self.style.overlay_background,
        )?;
        let text_gc =
            self.create_gc(window, self.style.text_color, self.style.overlay_background)?;
        let selected_text_gc = self.create_gc(
            window,
            self.style.text_selected_color,
            self.style.highlight_background,
        )?;
        let highlight_gc = self.create_gc(
            window,
            self.style.highlight_background,
            self.style.highlight_background,
        )?;
        let icon_gc = self.create_gc(
            window,
            self.style.overlay_background,
            self.style.overlay_background,
        )?;

        Ok(OverlayWindow {
            window,
            text_gc,
            selected_text_gc,
            highlight_gc,
            background_gc,
            icon_gc,
            width: width_u16,
            height: height_u16,
            layout,
            visible_capacity,
        })
    }

    fn create_gc(&self, window: Window, foreground: u32, background: u32) -> Result<Gcontext> {
        let gc = self.conn.generate_id()?;
        let aux = x11rb::protocol::xproto::CreateGCAux::new()
            .foreground(foreground)
            .background(background);
        self.conn.create_gc(gc, window, &aux)?;
        Ok(gc)
    }

    fn collect_windows(&mut self) -> Result<Vec<WindowEntry>> {
        let root = self.screen().root;
        let mut windows = self
            .get_property_window_list(root, self.atoms._NET_CLIENT_LIST_STACKING)?
            .unwrap_or_default();

        if windows.is_empty() {
            windows = self
                .get_property_window_list(root, self.atoms._NET_CLIENT_LIST)?
                .unwrap_or_default();
        }

        if windows.is_empty() {
            let tree = self.conn.query_tree(root)?.reply()?;
            windows = tree.children;
        }

        let mut result = Vec::new();
        for window in windows {
            if let Ok(attrs) = self.conn.get_window_attributes(window)?.reply() {
                if attrs.map_state != MapState::VIEWABLE || attrs.override_redirect {
                    continue;
                }
            } else {
                continue;
            }

            if let Some(state) = &self.state {
                if window == state.overlay.window {
                    continue;
                }
            }

            let title = self.window_title(window)?;
            let mut icon = self.window_icon(window)?;
            if icon.is_none() {
                let class_names = self.window_class_names(window)?;
                icon = self.icon_theme.lookup(&class_names)?;
            }
            result.push(WindowEntry {
                window,
                title,
                icon,
            });
        }
        self.apply_mru_order(&mut result);
        Ok(result)
    }

    fn get_property_window_list(
        &self,
        window: Window,
        property: u32,
    ) -> Result<Option<Vec<Window>>> {
        let reply = match self.conn.get_property(
            false,
            window,
            property,
            x11rb::protocol::xproto::AtomEnum::WINDOW,
            0,
            u32::MAX,
        ) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => reply,
                Err(_) => return Ok(None),
            },
            Err(_) => return Ok(None),
        };

        let windows: Vec<Window> = reply
            .value32()
            .into_iter()
            .flatten()
            .map(Window::from)
            .collect();
        Ok(Some(windows))
    }

    fn window_title(&self, window: Window) -> Result<String> {
        if let Some(name) = self.get_utf8_property(window, self.atoms._NET_WM_VISIBLE_NAME)? {
            if !name.is_empty() {
                return Ok(name);
            }
        }

        if let Some(name) = self.get_utf8_property(window, self.atoms._NET_WM_NAME)? {
            if !name.is_empty() {
                return Ok(name);
            }
        }

        if let Some(name) = self.get_utf8_property(window, self.atoms.WM_NAME)? {
            if !name.is_empty() {
                return Ok(name);
            }
        }

        if let Some(name) = self.get_string_property(window, self.atoms.WM_NAME)? {
            if !name.is_empty() {
                return Ok(name);
            }
        }

        Ok(format!("0x{:x}", window))
    }

    fn window_class_names(&self, window: Window) -> Result<Vec<String>> {
        let cookie = self.conn.get_property(
            false,
            window,
            self.atoms.WM_CLASS,
            x11rb::protocol::xproto::AtomEnum::STRING,
            0,
            64,
        )?;
        let reply = match cookie.reply() {
            Ok(rep) => rep,
            Err(_) => return Ok(Vec::new()),
        };
        if reply.value.is_empty() {
            return Ok(Vec::new());
        }

        let mut names = HashSet::new();
        for part in reply.value.split(|b| *b == 0) {
            if part.is_empty() {
                continue;
            }
            let text = String::from_utf8_lossy(part).trim().to_string();
            if text.is_empty() {
                continue;
            }
            names.insert(text.clone());
            names.insert(text.to_lowercase());
            names.insert(text.replace(' ', "-").to_lowercase());
        }

        Ok(names.into_iter().collect())
    }

    fn window_icon(&self, window: Window) -> Result<Option<Icon>> {
        let cookie = self.conn.get_property(
            false,
            window,
            self.atoms._NET_WM_ICON,
            AtomEnum::CARDINAL,
            0,
            u32::MAX,
        )?;

        let reply = match cookie.reply() {
            Ok(rep) => rep,
            Err(_) => return Ok(None),
        };

        let values: Vec<u32> = match reply.value32() {
            Some(iter) => iter.collect(),
            None => return Ok(None),
        };

        if values.len() < 3 {
            return Ok(None);
        }

        Ok(parse_wm_icon(&values, self.style.icon_max_size))
    }

    fn get_utf8_property(&self, window: Window, atom: u32) -> Result<Option<String>> {
        let cookie =
            self.conn
                .get_property(false, window, atom, self.atoms.UTF8_STRING, 0, 1024)?;
        let reply = match cookie.reply() {
            Ok(rep) => rep,
            Err(_) => return Ok(None),
        };
        if reply.value.is_empty() {
            return Ok(None);
        }
        let mut value = reply.value.clone();
        if let Some(pos) = value.iter().position(|b| *b == 0) {
            value.truncate(pos);
        }
        match String::from_utf8(value) {
            Ok(text) => Ok(Some(text)),
            Err(_) => Ok(None),
        }
    }

    fn get_string_property(&self, window: Window, atom: u32) -> Result<Option<String>> {
        let cookie = self.conn.get_property(false, window, atom, NONE, 0, 1024)?;
        let reply = match cookie.reply() {
            Ok(rep) => rep,
            Err(_) => return Ok(None),
        };
        if reply.value.is_empty() {
            return Ok(None);
        }
        let mut value = reply.value.clone();
        if let Some(pos) = value.iter().position(|b| *b == 0) {
            value.truncate(pos);
        }
        if value.is_empty() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&value).into_owned();
        Ok(Some(text))
    }

    fn get_active_window(&self) -> Result<Option<Window>> {
        let reply = self
            .conn
            .get_property(
                false,
                self.screen().root,
                self.atoms._NET_ACTIVE_WINDOW,
                x11rb::protocol::xproto::AtomEnum::WINDOW,
                0,
                1,
            )?
            .reply()
            .ok();
        if let Some(reply) = reply {
            if let Some(mut iter) = reply.value32() {
                if let Some(window) = iter.next() {
                    if window != 0 {
                        return Ok(Some(Window::from(window)));
                    }
                }
            }
        }
        Ok(None)
    }

    fn focus_window(&self, window: Window) -> Result<()> {
        let root = self.screen().root;
        let data = ClientMessageData::from([1, CURRENT_TIME, window, 0, 0]);
        let event = ClientMessageEvent {
            response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window,
            type_: self.atoms._NET_ACTIVE_WINDOW,
            data,
        };

        let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
        let _ = self.conn.send_event(false, root, mask, event);
        let _ = self.conn.set_input_focus(
            x11rb::protocol::xproto::InputFocus::POINTER_ROOT,
            window,
            CURRENT_TIME,
        );
        self.conn.flush()?;
        Ok(())
    }

    fn grab_tab_keys(&self) -> Result<()> {
        let root = self.screen().root;
        let ignore_masks = build_modifier_masks();
        for keycode in &self.bindings.tab {
            for mask in &ignore_masks {
                let mods = *mask | ModMask::M1;
                let mods_rev = *mask | ModMask::M1 | ModMask::SHIFT;
                let _ = self.conn.grab_key(
                    false,
                    root,
                    mods,
                    *keycode,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                );
                let _ = self.conn.grab_key(
                    false,
                    root,
                    mods_rev,
                    *keycode,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                );
            }
        }
        self.conn.flush()?;
        Ok(())
    }

    fn screen(&self) -> &x11rb::protocol::xproto::Screen {
        &self.conn.setup().roots[self.screen_num]
    }
}

#[derive(Debug, Clone)]
struct Icon {
    width: u16,
    height: u16,
    pixels: Vec<u32>, // Stored as ARGB
}

#[derive(Clone)]
enum IconSource {
    Path(PathBuf),
    Name(String),
}

struct IconTheme {
    cache: HashMap<String, Option<Icon>>,
    search_roots: Vec<PathBuf>,
    desktop_index: Option<HashMap<String, Vec<IconSource>>>,
    max_icon_size: u16,
}

impl IconTheme {
    fn new(max_icon_size: u16) -> Self {
        Self {
            cache: HashMap::new(),
            search_roots: icon_search_roots(),
            desktop_index: None,
            max_icon_size,
        }
    }

    fn lookup(&mut self, names: &[String]) -> Result<Option<Icon>> {
        for name in names {
            if name.is_empty() {
                continue;
            }
            let key = name.to_lowercase();
            if let Some(cached) = self.cache.get(&key) {
                if let Some(icon) = cached {
                    return Ok(Some(icon.clone()));
                } else {
                    continue;
                }
            }

            let mut icon = self.load_icon(&key)?;
            if icon.is_none() {
                if let Some(sources) = self.desktop_icon_sources(&key)? {
                    for source in sources {
                        icon = self.load_from_source(source)?;
                        if icon.is_some() {
                            break;
                        }
                    }
                }
            }
            self.cache.insert(key.clone(), icon.clone());
            if let Some(icon) = icon {
                return Ok(Some(icon));
            }
        }
        Ok(None)
    }

    fn load_icon(&self, name: &str) -> Result<Option<Icon>> {
        let mut variants = Vec::new();
        variants.push(name.to_string());
        if let Some(last) = name.rsplit('.').next() {
            if last != name {
                variants.push(last.to_string());
            }
        }
        if name.contains('-') {
            variants.push(name.replace('-', ""));
        }
        if name.contains('_') {
            variants.push(name.replace('_', "-"));
        }
        variants.sort();
        variants.dedup();

        let path = self.find_icon_path(&variants);
        match path {
            Some(path) => self.decode_icon(&path),
            None => Ok(None),
        }
    }

    fn find_icon_path(&self, names: &[String]) -> Option<PathBuf> {
        if names.is_empty() {
            return None;
        }
        let lowered: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
        for root in &self.search_roots {
            if let Some(found) = search_directory(root, &lowered, 0) {
                return Some(found);
            }
        }
        None
    }

    fn decode_icon(&self, path: &Path) -> Result<Option<Icon>> {
        let img = match image::open(path) {
            Ok(img) => img,
            Err(_) => return Ok(None),
        };
        let rgba = img.to_rgba8();
        let (width, height) = (rgba.width() as usize, rgba.height() as usize);
        let mut pixels = Vec::with_capacity(width * height);
        for chunk in rgba.chunks_exact(4) {
            pixels.push(
                (u32::from(chunk[3]) << 24)
                    | (u32::from(chunk[0]) << 16)
                    | (u32::from(chunk[1]) << 8)
                    | u32::from(chunk[2]),
            );
        }
        if !icon_has_visible_pixels(&pixels) {
            return Ok(None);
        }
        Ok(Some(scale_icon_to_limit(
            pixels,
            width,
            height,
            self.max_icon_size,
        )))
    }

    fn desktop_icon_sources(&mut self, key: &str) -> Result<Option<Vec<IconSource>>> {
        if self.desktop_index.is_none() {
            self.desktop_index = Some(build_desktop_index()?);
        }
        let index = self.desktop_index.as_ref().unwrap();
        Ok(index.get(key).cloned())
    }

    fn load_from_source(&self, source: IconSource) -> Result<Option<Icon>> {
        match source {
            IconSource::Path(path) => self.decode_icon(&path),
            IconSource::Name(name) => self.load_icon(&name),
        }
    }
}

struct WindowEntry {
    window: Window,
    title: String,
    icon: Option<Icon>,
}

struct OverlayWindow {
    window: Window,
    text_gc: Gcontext,
    selected_text_gc: Gcontext,
    highlight_gc: Gcontext,
    background_gc: Gcontext,
    icon_gc: Gcontext,
    width: u16,
    height: u16,
    layout: Layout,
    visible_capacity: usize,
}

struct OverlayState {
    windows: Vec<WindowEntry>,
    current: usize,
    first_visible: usize,
    overlay: OverlayWindow,
    alt_count: usize,
}

impl OverlayState {
    fn new(windows: Vec<WindowEntry>, overlay: OverlayWindow) -> Self {
        Self {
            windows,
            current: 0,
            first_visible: 0,
            overlay,
            alt_count: 0,
        }
    }

    fn advance(&mut self, direction: Direction) {
        if self.windows.is_empty() {
            return;
        }
        match direction {
            Direction::Forward => {
                self.current = (self.current + 1) % self.windows.len();
            }
            Direction::Backward => {
                if self.current == 0 {
                    self.current = self.windows.len() - 1;
                } else {
                    self.current -= 1;
                }
            }
        }
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        let capacity = max(1, self.overlay.visible_capacity);
        if self.current < self.first_visible {
            self.first_visible = self.current;
        } else if self.current >= self.first_visible + capacity {
            self.first_visible = self.current + 1 - capacity;
        }
    }

    fn visible_range(&self) -> impl Iterator<Item = usize> {
        let capacity = max(1, self.overlay.visible_capacity);
        let end = min(self.windows.len(), self.first_visible + capacity);
        self.first_visible..end
    }

    fn selected_window(&self) -> Option<Window> {
        self.windows.get(self.current).map(|entry| entry.window)
    }
}

#[derive(Copy, Clone)]
enum Direction {
    Forward,
    Backward,
}

struct KeyBindings {
    tab: Vec<Keycode>,
    alt: HashSet<Keycode>,
    escape: Option<Keycode>,
}

impl KeyBindings {
    fn load(conn: &RustConnection) -> Result<Self> {
        let mut bindings = Self {
            tab: Vec::new(),
            alt: HashSet::new(),
            escape: None,
        };
        bindings.refresh(conn)?;
        Ok(bindings)
    }

    fn refresh(&mut self, conn: &RustConnection) -> Result<()> {
        self.tab = keycodes_for_keysym(conn, XK_Tab)?;
        self.alt = keycodes_for_keysym(conn, XK_Alt_L)?
            .into_iter()
            .chain(keycodes_for_keysym(conn, XK_Alt_R)?)
            .collect();
        let escape_codes = keycodes_for_keysym(conn, XK_Escape)?;
        self.escape = escape_codes.into_iter().next();
        Ok(())
    }

    fn is_tab(&self, keycode: Keycode) -> bool {
        self.tab.contains(&keycode)
    }

    fn is_alt(&self, keycode: Keycode) -> bool {
        self.alt.contains(&keycode)
    }

    fn is_escape(&self, keycode: Keycode) -> bool {
        self.escape == Some(keycode)
    }
}

fn keycodes_for_keysym(conn: &RustConnection, keysym: u32) -> Result<Vec<Keycode>> {
    let min_keycode = conn.setup().min_keycode;
    let max_keycode = conn.setup().max_keycode;
    let count = (max_keycode + 1 - min_keycode) as u8;
    let reply = conn
        .get_keyboard_mapping(min_keycode, count)?
        .reply()
        .context("failed to query keyboard mapping")?;
    let syms_per_code = reply.keysyms_per_keycode as usize;
    let mut keycodes = Vec::new();

    for (idx, chunk) in reply.keysyms.chunks(syms_per_code).enumerate() {
        if chunk.iter().any(|sym| *sym == keysym) {
            let code = min_keycode + idx as u8;
            keycodes.push(code);
        }
    }
    Ok(keycodes)
}

fn sanitize_ascii(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_ascii() { ch } else { '?' })
        .collect()
}

fn build_modifier_masks() -> Vec<ModMask> {
    let mut masks = Vec::new();
    let extras = [
        ModMask::default(),
        ModMask::LOCK,
        ModMask::M2,
        ModMask::M5,
        ModMask::LOCK | ModMask::M2,
        ModMask::LOCK | ModMask::M5,
        ModMask::M2 | ModMask::M5,
        ModMask::LOCK | ModMask::M2 | ModMask::M5,
    ];
    masks.extend_from_slice(&extras);
    masks
}

fn intern_atom(conn: &RustConnection, name: &str) -> Result<Atom> {
    let reply = conn
        .intern_atom(false, name.as_bytes())?
        .reply()
        .with_context(|| format!("failed to intern atom {name}"))?;
    Ok(reply.atom)
}

fn parse_wm_icon(data: &[u32], max_icon_size: u16) -> Option<Icon> {
    let target = max_icon_size as usize;
    let mut best: Option<(usize, usize, Vec<u32>)> = None;
    let mut fallback: Option<(usize, usize, Vec<u32>)> = None;

    let mut idx = 0;
    while idx + 2 <= data.len() {
        let width = data[idx] as usize;
        let height = data[idx + 1] as usize;
        idx += 2;

        if width == 0 || height == 0 {
            continue;
        }

        let len = match width.checked_mul(height) {
            Some(len) => len,
            None => break,
        };
        if idx + len > data.len() {
            break;
        }

        let pixels = data[idx..idx + len].to_vec();
        idx += len;

        let max_dim = width.max(height);
        if width <= target && height <= target {
            let best_dim = best.as_ref().map(|(w, h, _)| (*w).max(*h)).unwrap_or(0);
            if max_dim > best_dim {
                best = Some((width, height, pixels));
            }
        } else {
            if let Some((fw, fh, _)) = &fallback {
                if (*fw).max(*fh) <= max_dim {
                    continue;
                }
            }
            fallback = Some((width, height, pixels));
        }
    }

    let (width, height, pixels) = if let Some(best) = best {
        best
    } else {
        fallback?
    };

    Some(scale_icon_to_limit(pixels, width, height, max_icon_size))
}

fn build_desktop_index() -> Result<HashMap<String, Vec<IconSource>>> {
    let mut map: HashMap<String, Vec<IconSource>> = HashMap::new();
    for dir in desktop_entry_dirs() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .map_or(true, |ext| ext != "desktop")
            {
                continue;
            }
            if let Some((keys, source)) = parse_desktop_file(&path) {
                for key in keys {
                    map.entry(key).or_insert_with(Vec::new).push(source.clone());
                }
            }
        }
    }
    Ok(map)
}

fn desktop_entry_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home_os) = env::var_os("HOME") {
        let home = PathBuf::from(&home_os);
        let local_apps = home.join(".local").join("share").join("applications");
        if local_apps.is_dir() {
            dirs.push(local_apps);
        }
    }

    if let Some(data_home) = env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        let apps = data_home.join("applications");
        if apps.is_dir() {
            dirs.push(apps);
        }
    }

    let data_dirs = env::var("XDG_DATA_DIRS")
        .map(|dirs| dirs.split(':').map(PathBuf::from).collect::<Vec<_>>())
        .unwrap_or_else(|_| {
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        });
    for dir in data_dirs {
        let apps = dir.join("applications");
        if apps.is_dir() {
            dirs.push(apps);
        }
    }

    if let Some(home_os) = env::var_os("HOME") {
        let home = PathBuf::from(home_os);
        let user_flatpak = home
            .join(".local")
            .join("share")
            .join("flatpak")
            .join("exports")
            .join("share")
            .join("applications");
        if user_flatpak.is_dir() {
            dirs.push(user_flatpak);
        }
    }

    let global_flatpak = PathBuf::from("/var/lib/flatpak/exports/share/applications");
    if global_flatpak.is_dir() {
        dirs.push(global_flatpak);
    }

    dirs.sort();
    dirs.dedup();
    dirs
}

fn parse_desktop_file(path: &Path) -> Option<(Vec<String>, IconSource)> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut in_entry = false;
    let mut icon_value: Option<String> = None;
    let mut name: Option<String> = None;
    let mut startup_classes: Vec<String> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_entry = trimmed.eq_ignore_ascii_case("[Desktop Entry]");
            continue;
        }
        if !in_entry || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("Icon=") {
            if icon_value.is_none() {
                icon_value = Some(value.trim().to_string());
            }
        } else if let Some(value) = trimmed.strip_prefix("Name=") {
            if name.is_none() {
                name = Some(value.trim().to_string());
            }
        } else if let Some(value) = trimmed.strip_prefix("StartupWMClass=") {
            let entry = value.trim();
            if !entry.is_empty() {
                startup_classes.push(entry.to_string());
            }
        }
    }

    let icon_value = icon_value?;
    let icon_source = if icon_value.starts_with('/') {
        IconSource::Path(PathBuf::from(icon_value))
    } else if icon_value.contains('/') {
        let base = path.parent().unwrap_or_else(|| Path::new(""));
        IconSource::Path(base.join(icon_value))
    } else {
        IconSource::Name(icon_value.to_lowercase())
    };

    let mut keys = HashSet::new();
    if let Some(name) = name {
        let lower = name.to_lowercase();
        if !lower.is_empty() {
            keys.insert(lower.clone());
            for part in lower.split_whitespace() {
                if !part.is_empty() {
                    keys.insert(part.to_string());
                }
            }
        }
    }
    for class in startup_classes {
        let lower = class.to_lowercase();
        if !lower.is_empty() {
            keys.insert(lower);
        }
    }
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
        let lower = stem.to_lowercase();
        keys.insert(lower.clone());
        for part in lower.split('.') {
            if !part.is_empty() {
                keys.insert(part.to_string());
            }
        }
    }
    match &icon_source {
        IconSource::Path(p) => {
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                let lower = stem.to_lowercase();
                keys.insert(lower.clone());
                for part in lower.split('-') {
                    if !part.is_empty() {
                        keys.insert(part.to_string());
                    }
                }
            }
        }
        IconSource::Name(name) => {
            if !name.is_empty() {
                keys.insert(name.clone());
                for part in name.split('-') {
                    if !part.is_empty() {
                        keys.insert(part.to_string());
                    }
                }
            }
        }
    }

    if keys.is_empty() {
        return None;
    }

    let mut key_list: Vec<String> = keys.into_iter().collect();
    key_list.sort();
    Some((key_list, icon_source))
}

fn icon_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        let hidden_icons = home.join(".icons");
        if hidden_icons.is_dir() {
            roots.push(hidden_icons);
        }
        let local_share = home.join(".local").join("share").join("icons");
        if local_share.is_dir() {
            roots.push(local_share);
        }
    }

    let data_home = env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| {
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".local").join("share"))
    });
    if let Some(dir) = data_home {
        let icons_dir = dir.join("icons");
        if icons_dir.is_dir() {
            roots.push(icons_dir);
        }
    }

    let data_dirs = env::var("XDG_DATA_DIRS")
        .map(|dirs| dirs.split(':').map(PathBuf::from).collect::<Vec<_>>())
        .unwrap_or_else(|_| {
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        });
    for dir in data_dirs {
        let icons_dir = dir.join("icons");
        if icons_dir.is_dir() {
            roots.push(icons_dir);
        }
    }

    let pixmaps = PathBuf::from("/usr/share/pixmaps");
    if pixmaps.is_dir() {
        roots.push(pixmaps);
    }

    roots
}

fn search_directory(dir: &Path, names: &[String], depth: u8) -> Option<PathBuf> {
    if depth > MAX_ICON_SEARCH_DEPTH {
        return None;
    }

    if !dir.exists() {
        return None;
    }

    if dir.is_file() && icon_file_matches(dir, names) {
        return Some(dir.to_path_buf());
    }

    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = search_directory(&path, names, depth + 1) {
                return Some(found);
            }
        } else if path.is_file() && icon_file_matches(&path, names) {
            return Some(path);
        }
    }
    None
}

fn icon_file_matches(path: &Path, names: &[String]) -> bool {
    let extension = match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => ext.to_lowercase(),
        None => return false,
    };

    if !matches!(extension.as_str(), "png" | "jpg" | "jpeg") {
        return false;
    }

    let stem = match path.file_stem().and_then(|stem| stem.to_str()) {
        Some(stem) => stem.to_lowercase(),
        None => return false,
    };

    names.iter().any(|name| {
        let candidate = name.as_str();
        stem == candidate || stem.starts_with(candidate)
    })
}

fn scale_icon_to_limit(pixels: Vec<u32>, width: usize, height: usize, max_icon_size: u16) -> Icon {
    if width == 0 || height == 0 {
        return Icon {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        };
    }

    let target = usize::from(max_icon_size);
    if width <= target && height <= target {
        return Icon {
            width: width as u16,
            height: height as u16,
            pixels,
        };
    }

    let max_dim = width.max(height);
    if max_dim == 0 {
        return Icon {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        };
    }

    let new_width = ((width * target + max_dim / 2) / max_dim).max(1);
    let new_height = ((height * target + max_dim / 2) / max_dim).max(1);
    let mut scaled = vec![0u32; new_width * new_height];

    for y in 0..new_height {
        let src_y = y * height / new_height;
        for x in 0..new_width {
            let src_x = x * width / new_width;
            scaled[y * new_width + x] = pixels[src_y * width + src_x];
        }
    }

    Icon {
        width: new_width as u16,
        height: new_height as u16,
        pixels: scaled,
    }
}

fn icon_has_visible_pixels(pixels: &[u32]) -> bool {
    pixels.iter().any(|&pixel| {
        let alpha = (pixel >> 24) & 0xff;
        let rgb = pixel & 0x00ff_ffff;
        alpha != 0 || rgb != 0
    })
}
