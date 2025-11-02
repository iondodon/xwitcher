use crate::atoms::Atoms;
use crate::config::Layout;
use crate::icons::{Icon, IconTheme, parse_wm_icon};
use crate::overlay::{Direction, OverlayState, OverlayWindow, WindowEntry};
use crate::style::Style;
use crate::util::sanitize_ascii;
use anyhow::{Context, Result};
use std::cmp::{Ordering, max, min};
use std::collections::HashSet;
use x11::keysym::{XK_Alt_L, XK_Alt_R, XK_Escape, XK_Tab};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClientMessageData, ClientMessageEvent, CreateWindowAux,
    EventMask, Gcontext, GrabMode, ImageFormat, ImageOrder, InputFocus, KeyButMask, KeyPressEvent,
    KeyReleaseEvent, Keycode, LineStyle, MapState, ModMask, PropMode, PropertyNotifyEvent,
    Rectangle, Screen, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{CURRENT_TIME, NONE};

pub struct AltTab {
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
    pub fn new(
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

    pub fn run(&mut self) -> Result<()> {
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
                            self.state = None;
                        }
                    }
                }
                Event::MotionNotify(event) => {
                    self.update_hover_from_pointer(event.event, event.event_x, event.event_y)?;
                }
                Event::EnterNotify(event) => {
                    self.update_hover_from_pointer(event.event, event.event_x, event.event_y)?;
                }
                Event::LeaveNotify(event) => {
                    self.clear_hover_from_pointer(event.event)?;
                }
                Event::ButtonPress(event) => {
                    self.update_hover_from_pointer(event.event, event.event_x, event.event_y)?;
                }
                Event::ButtonRelease(event) => {
                    if event.detail == 1 {
                        self.activate_selection_from_pointer(
                            event.event,
                            event.event_x,
                            event.event_y,
                        )?;
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

        let hover_color = Self::blend_colors(
            self.style.overlay_background,
            self.style.highlight_background,
        );

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
                    let is_hovered = state.hovered == Some(window_index);
                    let item_background = if is_selected {
                        self.style.highlight_background
                    } else if is_hovered {
                        hover_color
                    } else {
                        self.style.overlay_background
                    };
                    if is_selected {
                        self.conn.poly_fill_rectangle(
                            state.overlay.window,
                            state.overlay.highlight_gc,
                            &[rect],
                        )?;
                    } else if is_hovered {
                        self.conn.poly_fill_rectangle(
                            state.overlay.window,
                            state.overlay.hover_gc,
                            &[rect],
                        )?;
                    }

                    if let Some(icon) = &entry.icon {
                        let icon_x = rect.x + self.style.icon_margin as i16;
                        let icon_y = rect.y + max(0, (row_height as i16 - icon.height as i16) / 2);
                        self.draw_icon(&state.overlay, icon, icon_x, icon_y, item_background)?;
                    }

                    if self.style.item_border_width > 0 {
                        let border_gc = if is_selected {
                            state
                                .overlay
                                .item_selected_border_gc
                                .or(state.overlay.item_border_gc)
                        } else {
                            state.overlay.item_border_gc
                        };
                        if let Some(gc) = border_gc {
                            self.draw_rect_border(
                                state.overlay.window,
                                gc,
                                rect,
                                self.style.item_border_width,
                            )?;
                        }
                    }

                    let gc = if is_selected {
                        state.overlay.selected_text_gc
                    } else if is_hovered {
                        state.overlay.hover_text_gc
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
                    let is_hovered = state.hovered == Some(window_index);
                    let item_background = if is_selected {
                        self.style.highlight_background
                    } else if is_hovered {
                        hover_color
                    } else {
                        self.style.overlay_background
                    };
                    if is_selected {
                        self.conn.poly_fill_rectangle(
                            state.overlay.window,
                            state.overlay.highlight_gc,
                            &[rect],
                        )?;
                    } else if is_hovered {
                        self.conn.poly_fill_rectangle(
                            state.overlay.window,
                            state.overlay.hover_gc,
                            &[rect],
                        )?;
                    }

                    if let Some(icon) = &entry.icon {
                        let icon_x =
                            cell_x + max(0, (cell_width as i32 - icon.width as i32) / 2) as i16;
                        let icon_y = padding as i16
                            + max(0, (icon_area as i32 - icon.height as i32) / 2) as i16;
                        self.draw_icon(&state.overlay, icon, icon_x, icon_y, item_background)?;
                    }

                    if self.style.item_border_width > 0 {
                        let border_gc = if is_selected {
                            state
                                .overlay
                                .item_selected_border_gc
                                .or(state.overlay.item_border_gc)
                        } else {
                            state.overlay.item_border_gc
                        };
                        if let Some(gc) = border_gc {
                            self.draw_rect_border(
                                state.overlay.window,
                                gc,
                                rect,
                                self.style.item_border_width,
                            )?;
                        }
                    }

                    let gc = if is_selected {
                        state.overlay.selected_text_gc
                    } else if is_hovered {
                        state.overlay.hover_text_gc
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

        if self.style.overlay_border_width > 0 {
            if let Some(gc) = state.overlay.border_gc {
                let rect = Rectangle {
                    x: 0,
                    y: 0,
                    width: state.overlay.width,
                    height: state.overlay.height,
                };
                self.draw_rect_border(
                    state.overlay.window,
                    gc,
                    rect,
                    self.style.overlay_border_width,
                )?;
            }
        }
        self.conn.flush()?;
        Ok(())
    }

    fn update_hover_from_pointer(&mut self, window: Window, x: i16, y: i16) -> Result<()> {
        let hover_index = match self.state.as_ref() {
            Some(state) if state.overlay.window == window => {
                self.item_index_at_position(state, x, y)
            }
            _ => return Ok(()),
        };

        if let Some(state) = self.state.as_mut() {
            if state.set_hovered(hover_index) {
                self.redraw_overlay()?;
            }
        }
        Ok(())
    }

    fn activate_selection_from_pointer(&mut self, window: Window, x: i16, y: i16) -> Result<()> {
        let index = match self.state.as_ref() {
            Some(state) if state.overlay.window == window => {
                self.item_index_at_position(state, x, y)
            }
            _ => return Ok(()),
        };

        if let Some(index) = index {
            if let Some(state) = self.state.as_mut() {
                state.set_current(index);
            }
            self.finish_selection(true)?;
        }
        Ok(())
    }

    fn clear_hover_from_pointer(&mut self, window: Window) -> Result<()> {
        if let Some(state) = self.state.as_mut() {
            if state.overlay.window == window {
                if state.set_hovered(None) {
                    self.redraw_overlay()?;
                }
            }
        }
        Ok(())
    }

    fn item_index_at_position(&self, state: &OverlayState, x: i16, y: i16) -> Option<usize> {
        match state.overlay.layout {
            Layout::Vertical => {
                let padding = self.style.padding as i16;
                let row_height = self.style.row_height;
                if row_height == 0 {
                    return None;
                }
                let width = state
                    .overlay
                    .width
                    .saturating_sub(self.style.padding.saturating_mul(2));

                for (slot, window_index) in state.visible_range().enumerate() {
                    let rect_y = padding + (slot as i16) * row_height as i16;
                    let rect = Rectangle {
                        x: padding,
                        y: rect_y,
                        width,
                        height: row_height,
                    };
                    if Self::rect_contains(&rect, x, y) {
                        return Some(window_index);
                    }
                }
                None
            }
            Layout::Horizontal => {
                let capacity = max(1, state.overlay.visible_capacity);
                let padding = self.style.padding;
                let available_width = state
                    .overlay
                    .width
                    .saturating_sub(padding.saturating_mul(2));
                let mut cell_width_u32 = u32::from(self.style.horizontal_item_width.max(1));
                let available_width_u32 = u32::from(available_width);
                if cell_width_u32 * capacity as u32 > available_width_u32 && available_width_u32 > 0
                {
                    cell_width_u32 = max(1, available_width_u32 / capacity as u32);
                }
                let cell_width = cell_width_u32 as u16;
                if cell_width == 0 {
                    return None;
                }

                let icon_area = self.style.icon_area();
                let cell_height = max(
                    icon_area,
                    state
                        .overlay
                        .height
                        .saturating_sub(padding.saturating_mul(2)),
                );
                let total_items_width = cell_width_u32 * capacity as u32;
                let extra_space = available_width_u32.saturating_sub(total_items_width);
                let leading_offset = padding as i16 + (extra_space / 2) as i16;

                for (slot, window_index) in state.visible_range().enumerate() {
                    let cell_x = leading_offset + (slot as u32 * cell_width_u32) as i16;
                    let rect = Rectangle {
                        x: cell_x,
                        y: padding as i16,
                        width: cell_width,
                        height: cell_height,
                    };
                    if Self::rect_contains(&rect, x, y) {
                        return Some(window_index);
                    }
                }
                None
            }
        }
    }

    fn rect_contains(rect: &Rectangle, x: i16, y: i16) -> bool {
        let px = i32::from(x);
        let py = i32::from(y);
        let rx1 = i32::from(rect.x);
        let ry1 = i32::from(rect.y);
        let rx2 = rx1 + i32::from(rect.width);
        let ry2 = ry1 + i32::from(rect.height);
        px >= rx1 && px < rx2 && py >= ry1 && py < ry2
    }

    fn blend_colors(a: u32, b: u32) -> u32 {
        let ar = (a >> 16) & 0xff;
        let ag = (a >> 8) & 0xff;
        let ab = a & 0xff;

        let br = (b >> 16) & 0xff;
        let bg = (b >> 8) & 0xff;
        let bb = b & 0xff;

        let r = ((ar + br) / 2) & 0xff;
        let g = ((ag + bg) / 2) & 0xff;
        let blue = ((ab + bb) / 2) & 0xff;

        (r << 16) | (g << 8) | blue
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
        background: u32,
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

        let bg_r = ((background >> 16) & 0xff) as u32;
        let bg_g = ((background >> 8) & 0xff) as u32;
        let bg_b = (background & 0xff) as u32;

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

    fn draw_rect_border(
        &self,
        window: Window,
        gc: Gcontext,
        rect: Rectangle,
        stroke: u16,
    ) -> Result<()> {
        if stroke == 0 {
            return Ok(());
        }
        let stroke_i = stroke as i16;
        let inset = stroke_i / 2;
        let width = rect.width as i32 - stroke as i32;
        let height = rect.height as i32 - stroke as i32;
        if width <= 0 || height <= 0 {
            return Ok(());
        }
        let border_rect = Rectangle {
            x: rect.x + inset,
            y: rect.y + inset,
            width: width as u16,
            height: height as u16,
        };
        self.conn
            .poly_rectangle(window, gc, &[border_rect])
            .context("failed to draw border")?;
        Ok(())
    }

    fn destroy_overlay(&self, overlay: &OverlayWindow) -> Result<()> {
        let _ = self.conn.free_gc(overlay.text_gc);
        let _ = self.conn.free_gc(overlay.selected_text_gc);
        let _ = self.conn.free_gc(overlay.hover_text_gc);
        let _ = self.conn.free_gc(overlay.highlight_gc);
        let _ = self.conn.free_gc(overlay.hover_gc);
        let _ = self.conn.free_gc(overlay.background_gc);
        let _ = self.conn.free_gc(overlay.icon_gc);
        if let Some(gc) = overlay.border_gc {
            let _ = self.conn.free_gc(gc);
        }
        if let Some(gc) = overlay.item_border_gc {
            let _ = self.conn.free_gc(gc);
        }
        if let Some(gc) = overlay.item_selected_border_gc {
            let _ = self.conn.free_gc(gc);
        }
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
                &CreateWindowAux::new()
                    .background_pixel(screen.black_pixel)
                    .event_mask(
                        EventMask::EXPOSURE
                            | EventMask::BUTTON_PRESS
                            | EventMask::BUTTON_RELEASE
                            | EventMask::POINTER_MOTION
                            | EventMask::ENTER_WINDOW
                            | EventMask::LEAVE_WINDOW,
                    ),
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
            AtomEnum::STRING,
            wm_class,
        )?;

        self.conn.change_property32(
            PropMode::REPLACE,
            window,
            self.atoms._NET_WM_WINDOW_TYPE,
            AtomEnum::ATOM,
            &[self.atoms._NET_WM_WINDOW_TYPE_NOTIFICATION],
        )?;

        self.conn.map_window(window)?;

        let hover_color = Self::blend_colors(
            self.style.overlay_background,
            self.style.highlight_background,
        );

        let background_gc = self.create_gc(
            window,
            self.style.overlay_background,
            self.style.overlay_background,
        )?;
        let text_gc =
            self.create_gc(window, self.style.text_color, self.style.overlay_background)?;
        let hover_text_gc = self.create_gc(window, self.style.text_color, hover_color)?;
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
        let hover_gc = self.create_gc(window, hover_color, self.style.overlay_background)?;
        let icon_gc = self.create_gc(
            window,
            self.style.overlay_background,
            self.style.overlay_background,
        )?;
        let border_gc = self.maybe_create_line_gc(
            window,
            self.style.overlay_border_color,
            self.style.overlay_background,
            self.style.overlay_border_width,
        )?;
        let item_border_gc = self.maybe_create_line_gc(
            window,
            self.style.item_border_color,
            self.style.overlay_background,
            self.style.item_border_width,
        )?;
        let item_selected_border_gc = self.maybe_create_line_gc(
            window,
            self.style.item_selected_border_color,
            self.style.highlight_background,
            self.style.item_border_width,
        )?;

        Ok(OverlayWindow {
            window,
            text_gc,
            selected_text_gc,
            hover_text_gc,
            highlight_gc,
            hover_gc,
            background_gc,
            icon_gc,
            border_gc,
            item_border_gc,
            item_selected_border_gc,
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

    fn create_line_gc(
        &self,
        window: Window,
        foreground: u32,
        background: u32,
        line_width: u16,
    ) -> Result<Gcontext> {
        let gc = self.conn.generate_id()?;
        let aux = x11rb::protocol::xproto::CreateGCAux::new()
            .foreground(foreground)
            .background(background)
            .line_width(Some(u32::from(line_width.max(1))))
            .line_style(LineStyle::SOLID);
        self.conn.create_gc(gc, window, &aux)?;
        Ok(gc)
    }

    fn maybe_create_line_gc(
        &self,
        window: Window,
        color: u32,
        background: u32,
        line_width: u16,
    ) -> Result<Option<Gcontext>> {
        if line_width == 0 {
            Ok(None)
        } else {
            Ok(Some(
                self.create_line_gc(window, color, background, line_width)?,
            ))
        }
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
        let reply = self
            .conn
            .get_property(false, window, property, AtomEnum::WINDOW, 0, u32::MAX)?
            .reply()
            .ok();
        if let Some(reply) = reply {
            if let Some(iter) = reply.value32() {
                let mut list = Vec::new();
                for window in iter {
                    if window != 0 {
                        list.push(Window::from(window));
                    }
                }
                return Ok(Some(list));
            }
        }
        Ok(None)
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

    fn window_title(&self, window: Window) -> Result<String> {
        if let Some(title) = self.get_utf8_property(window, self.atoms._NET_WM_VISIBLE_NAME)? {
            if !title.trim().is_empty() {
                return Ok(title);
            }
        }
        if let Some(title) = self.get_utf8_property(window, self.atoms._NET_WM_NAME)? {
            if !title.trim().is_empty() {
                return Ok(title);
            }
        }
        if let Some(title) = self.get_string_property(window, self.atoms.WM_NAME)? {
            if !title.trim().is_empty() {
                return Ok(title);
            }
        }
        Ok(format!("0x{:x}", window))
    }

    fn window_class_names(&self, window: Window) -> Result<Vec<String>> {
        let cookie =
            self.conn
                .get_property(false, window, self.atoms.WM_CLASS, AtomEnum::STRING, 0, 64)?;
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

    fn get_active_window(&self) -> Result<Option<Window>> {
        let reply = self
            .conn
            .get_property(
                false,
                self.screen().root,
                self.atoms._NET_ACTIVE_WINDOW,
                AtomEnum::WINDOW,
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
        let _ = self
            .conn
            .set_input_focus(InputFocus::POINTER_ROOT, window, CURRENT_TIME);
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

    fn screen(&self) -> &Screen {
        &self.conn.setup().roots[self.screen_num]
    }
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
