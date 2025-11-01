use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};

const MAX_ICON_SEARCH_DEPTH: u8 = 5;

#[derive(Debug, Clone)]
pub struct Icon {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u32>, // Stored as ARGB
}

#[derive(Clone)]
enum IconSource {
    Path(PathBuf),
    Name(String),
}

pub struct IconTheme {
    cache: HashMap<String, Option<Icon>>,
    search_roots: Vec<PathBuf>,
    desktop_index: Option<HashMap<String, Vec<IconSource>>>,
    max_icon_size: u16,
}

impl IconTheme {
    pub fn new(max_icon_size: u16) -> Self {
        Self {
            cache: HashMap::new(),
            search_roots: icon_search_roots(),
            desktop_index: None,
            max_icon_size,
        }
    }

    pub fn lookup(&mut self, names: &[String]) -> Result<Option<Icon>> {
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

    fn load_from_source(&self, source: IconSource) -> Result<Option<Icon>> {
        match source {
            IconSource::Path(path) => self.decode_icon(&path),
            IconSource::Name(name) => self.load_icon(&name),
        }
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
}

pub fn parse_wm_icon(data: &[u32], max_icon_size: u16) -> Option<Icon> {
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

    dirs
}

fn parse_desktop_file(path: &Path) -> Option<(Vec<String>, IconSource)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut name_variants = HashSet::new();
    let mut icon_name: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Name=") {
            push_variants(&mut name_variants, rest);
        } else if let Some(rest) = trimmed.strip_prefix("GenericName=") {
            push_variants(&mut name_variants, rest);
        } else if let Some(rest) = trimmed.strip_prefix("Icon=") {
            icon_name = Some(rest.trim().to_string());
        }
    }
    let icon_name = icon_name?;
    let icon_source = if icon_name.contains('/') {
        IconSource::Path(PathBuf::from(icon_name))
    } else {
        IconSource::Name(icon_name)
    };
    let mut keys = name_variants.into_iter().collect::<Vec<_>>();
    keys.push(
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_lowercase(),
    );
    Some((keys, icon_source))
}

fn push_variants(set: &mut HashSet<String>, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let lowered = trimmed.to_lowercase();
    set.insert(lowered.clone());
    set.insert(lowered.replace(' ', "-"));
}

fn icon_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home_os) = env::var_os("HOME") {
        let home = PathBuf::from(home_os);
        let local_icons = home.join(".icons");
        if local_icons.is_dir() {
            roots.push(local_icons);
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

fn icon_has_visible_pixels(pixels: &[u32]) -> bool {
    pixels.iter().any(|pixel| (*pixel >> 24) & 0xff != 0)
}

pub fn scale_icon_to_limit(
    pixels: Vec<u32>,
    width: usize,
    height: usize,
    max_icon_size: u16,
) -> Icon {
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
