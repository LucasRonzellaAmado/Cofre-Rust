use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

pub fn path_to_key(path: &Path) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => {
                let raw = p.as_os_str().to_string_lossy();
                let cleaned: String = raw.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
                if !cleaned.is_empty() {
                    parts.push(cleaned);
                }
            }
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                return Err(Error::InvalidPath(format!(
                    "caminho com '..' não suportado: {}",
                    path.display()
                )));
            }
            Component::Normal(os) => {
                let s = os.to_str().ok_or_else(|| {
                    Error::InvalidPath(format!("nome não é UTF-8 válido: {}", path.display()))
                })?;
                parts.push(s.to_string());
            }
        }
    }
    Ok(parts.join("/"))
}

pub fn normalize_key(input: &str) -> String {
    let unified = input.replace('\\', "/");
    let mut parts: Vec<String> = Vec::new();
    for (i, part) in unified.split('/').enumerate() {
        if part.is_empty() || part == "." {
            continue;
        }
        if i == 0 && part.len() == 2 && part.ends_with(':') {
            parts.push(part.trim_end_matches(':').to_string());
        } else {
            parts.push(part.to_string());
        }
    }
    parts.join("/")
}

pub fn key_to_relative(key: &str) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    if key.is_empty() {
        return Err(Error::InvalidPath("caminho vazio".into()));
    }
    for part in key.split('/') {
        let bad = part.is_empty()
            || part == "."
            || part == ".."
            || part.contains('\\')
            || part.contains('\0')
            || part.contains(':');
        if bad {
            return Err(Error::InvalidPath(format!(
                "componente suspeito em '{key}'"
            )));
        }
        out.push(part);
    }
    Ok(out)
}

pub fn key_matches_prefix(key: &str, prefix: &str) -> bool {
    prefix.is_empty()
        || key == prefix
        || (key.len() > prefix.len()
            && key.starts_with(prefix)
            && key.as_bytes()[prefix.len()] == b'/')
}

pub fn parent_key(key: &str) -> Option<&str> {
    key.rfind('/').map(|i| &key[..i])
}
