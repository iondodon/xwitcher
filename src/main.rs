use std::cmp::{max, min};
use std::collections::HashSet;
use anyhow::{Context, Result};

use x11::{
    keysym::{
        XK_Alt_L, XK_Alt_R, XK_Escape, XK_Tab,
    },
};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{
    protocol::{
        xproto::{
            Atom, ChangeWindowAttributesAux, ClientMessageData, ClientMessageEvent, EventMask,
            Gcontext, GrabMode, KeyButMask, KeyPressEvent, KeyReleaseEvent, Keycode, MapState,
            ModMask, PropMode, Rectangle, Window, WindowClass,
        },
        Event,
    },
    rust_connection::RustConnection,
    CURRENT_TIME, NONE,
};

const OVERLAY_WIDTH: u16 = 600;
const ROW_HEIGHT: u16 = 32;
const PADDING: u16 = 16;
const SCREEN_MARGIN: u16 = 96;
const TEXT_BASELINE: i16 = 22;

#[allow(non_snake_case)]
struct Atoms {
    _NET_ACTIVE_WINDOW: Atom,
    _NET_CLIENT_LIST: Atom,
    _NET_CLIENT_LIST_STACKING: Atom,
    _NET_WM_NAME: Atom,
    _NET_WM_VISIBLE_NAME: Atom,
    _NET_WM_WINDOW_TYPE: Atom,
    _NET_WM_WINDOW_TYPE_NOTIFICATION: Atom,
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
            _NET_WM_WINDOW_TYPE_NOTIFICATION: intern_atom(conn, "_NET_WM_WINDOW_TYPE_NOTIFICATION")?,
            UTF8_STRING: intern_atom(conn, "UTF8_STRING")?,
            WM_CLASS: intern_atom(conn, "WM_CLASS")?,
            WM_NAME: intern_atom(conn, "WM_NAME")?,
        })
    }
}

fn main() -> Result<()> {
    let (conn, screen_num) = x11rb::connect(None).context("failed to connect to X server")?;
    let atoms = Atoms::new(&conn).context("failed to intern atoms")?;
    let mut app = AltTab::new(conn, screen_num, atoms)?;
    app.run()
}

struct AltTab {
    conn: RustConnection,
    screen_num: usize,
    atoms: Atoms,
    bindings: KeyBindings,
    state: Option<OverlayState>,
}

impl AltTab {
    fn new(conn: RustConnection, screen_num: usize, atoms: Atoms) -> Result<Self> {
        let bindings = KeyBindings::load(&conn)?;
        let app = Self {
            conn,
            screen_num,
            atoms,
            bindings,
            state: None,
        };
        app.grab_tab_keys()?;
        Ok(app)
    }

    fn run(&mut self) -> Result<()> {
        loop {
            match self.conn.wait_for_event().context("failed waiting for X event")? {
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
                let shift_down =
                    (u16::from(event.state) & u16::from(KeyButMask::SHIFT)) != 0;
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
            let shift_down =
                (u16::from(event.state) & u16::from(KeyButMask::SHIFT)) != 0;
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
        self.conn.clear_area(
            false,
            state.overlay.window,
            0,
            0,
            state.overlay.width,
            state.overlay.height,
        )?;

        let visible = state.visible_range();
        for (idx, window_index) in visible.enumerate() {
            let entry = &state.windows[window_index];
            let rect_y = PADDING as i16 + (idx as i16) * ROW_HEIGHT as i16;
            let rect = Rectangle {
                x: PADDING as i16,
                y: rect_y,
                width: state.overlay.width - 2 * PADDING,
                height: ROW_HEIGHT,
            };

            if window_index == state.current {
                self.conn.poly_fill_rectangle(
                    state.overlay.window,
                    state.overlay.highlight_gc,
                    &[rect],
                )?;
                self.draw_text(
                    state.overlay.window,
                    state.overlay.selected_text_gc,
                    rect.x + 8,
                    rect.y + TEXT_BASELINE,
                    &entry.title,
                )?;
            } else {
                self.draw_text(
                    state.overlay.window,
                    state.overlay.text_gc,
                    rect.x + 8,
                    rect.y + TEXT_BASELINE,
                    &entry.title,
                )?;
            }
        }
        self.conn.flush()?;
        Ok(())
    }

    fn draw_text(
        &self,
        window: Window,
        gc: Gcontext,
        x: i16,
        y: i16,
        text: &str,
    ) -> Result<()> {
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

    fn destroy_overlay(&self, overlay: &OverlayWindow) -> Result<()> {
        let _ = self.conn.free_gc(overlay.text_gc);
        let _ = self.conn.free_gc(overlay.selected_text_gc);
        let _ = self.conn.free_gc(overlay.highlight_gc);
        let _ = self.conn.unmap_window(overlay.window);
        let _ = self.conn.destroy_window(overlay.window);
        Ok(())
    }

    fn create_overlay(&self, row_count: usize) -> Result<OverlayWindow> {
        let screen = self.screen();
        let width = min(OVERLAY_WIDTH, screen.width_in_pixels);
        let mut full_height = PADDING * 2 + (row_count as u16) * ROW_HEIGHT;
        let max_height = screen
            .height_in_pixels
            .saturating_sub(SCREEN_MARGIN);
        if max_height > 0 {
            full_height = min(full_height, max_height);
        }
        let height = max(full_height, PADDING * 2 + ROW_HEIGHT);
        let visible_rows =
            max(1, ((height.saturating_sub(PADDING * 2)) / ROW_HEIGHT) as usize);

        let x = ((screen.width_in_pixels - width) / 2) as i16;
        let y = ((screen.height_in_pixels - height) / 2) as i16;

        let window = self.conn.generate_id().context("failed to alloc window id")?;
        self.conn
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                screen.root,
                x,
                y,
                width,
                height,
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

        let text_gc = self.create_gc(window, screen.white_pixel, screen.black_pixel)?;
        let selected_text_gc = self.create_gc(window, screen.black_pixel, screen.white_pixel)?;
        let highlight_gc = self.create_gc(window, screen.white_pixel, screen.white_pixel)?;

        Ok(OverlayWindow {
            window,
            text_gc,
            selected_text_gc,
            highlight_gc,
            width,
            height,
            visible_rows,
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

    fn collect_windows(&self) -> Result<Vec<WindowEntry>> {
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
            result.push(WindowEntry { window, title });
        }
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

    fn get_utf8_property(&self, window: Window, atom: u32) -> Result<Option<String>> {
        let cookie = self.conn.get_property(
            false,
            window,
            atom,
            self.atoms.UTF8_STRING,
            0,
            1024,
        )?;
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
        let data = ClientMessageData::from([
            1,
            CURRENT_TIME,
            window,
            0,
            0,
        ]);
        let event = ClientMessageEvent {
            response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window,
            type_: self.atoms._NET_ACTIVE_WINDOW,
            data,
        };

        let mask =
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
        let _ = self.conn.send_event(false, root, mask, event);
        let _ = self
            .conn
            .set_input_focus(x11rb::protocol::xproto::InputFocus::POINTER_ROOT, window, CURRENT_TIME);
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
                let _ = self
                    .conn
                    .grab_key(false, root, mods, *keycode, GrabMode::ASYNC, GrabMode::ASYNC);
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
struct WindowEntry {
    window: Window,
    title: String,
}

struct OverlayWindow {
    window: Window,
    text_gc: Gcontext,
    selected_text_gc: Gcontext,
    highlight_gc: Gcontext,
    width: u16,
    height: u16,
    visible_rows: usize,
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
        if self.current < self.first_visible {
            self.first_visible = self.current;
        } else if self.current >= self.first_visible + self.overlay.visible_rows {
            self.first_visible = self.current + 1 - self.overlay.visible_rows;
        }
    }

    fn visible_range(&self) -> impl Iterator<Item = usize> {
        let end = min(
            self.windows.len(),
            self.first_visible + self.overlay.visible_rows,
        );
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
