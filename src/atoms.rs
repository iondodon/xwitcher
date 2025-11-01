use anyhow::{Context, Result};
use x11rb::protocol::xproto::{Atom, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

#[allow(non_snake_case)]
pub struct Atoms {
    pub _NET_ACTIVE_WINDOW: Atom,
    pub _NET_CLIENT_LIST: Atom,
    pub _NET_CLIENT_LIST_STACKING: Atom,
    pub _NET_WM_NAME: Atom,
    pub _NET_WM_VISIBLE_NAME: Atom,
    pub _NET_WM_WINDOW_TYPE: Atom,
    pub _NET_WM_WINDOW_TYPE_NOTIFICATION: Atom,
    pub _NET_WM_ICON: Atom,
    pub UTF8_STRING: Atom,
    pub WM_CLASS: Atom,
    pub WM_NAME: Atom,
}

impl Atoms {
    pub fn new(conn: &RustConnection) -> Result<Self> {
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

fn intern_atom(conn: &RustConnection, name: &str) -> Result<Atom> {
    let reply = conn
        .intern_atom(false, name.as_bytes())?
        .reply()
        .with_context(|| format!("failed to intern atom {name}"))?;
    Ok(reply.atom)
}
