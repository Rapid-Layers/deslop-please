//! The module files rust-analyzer leaves out: those that only a `cfg` it
//! left off declares. They are found as rustc finds a module's file, from the
//! `mod` declarations of the files around them, whatever their `cfg`.

use std::collections::HashSet;

use super::source::{Declared, Source};
use crate::Error;
use crate::model::{NodeId, Unanalyzed};
use crate::workspace::Workspace;

/// A file rust-analyzer analyzed.
pub struct Analyzed<'a> {
    pub path: &'a str,
    pub module: NodeId,
    pub source: &'a Source,
}

/// The module files that `analyzed`'s declarations reach, and theirs in
/// turn, that are not among `analyzed`, by path.
pub fn find(analyzed: &[Analyzed], workspace: &Workspace) -> Result<Vec<Unanalyzed>, Error> {
    let roots: HashSet<&str> = workspace
        .packages
        .iter()
        .flat_map(|package| &package.targets)
        .map(|target| target.root.as_str())
        .collect();
    let mut known: HashSet<String> = analyzed.iter().map(|file| file.path.to_owned()).collect();
    let mut pending: Vec<(String, NodeId, Vec<Declared>)> = analyzed
        .iter()
        .map(|file| {
            let declared = file.source.module_declarations();
            (file.path.to_owned(), file.module, declared)
        })
        .collect();
    let mut found = Vec::new();
    while let Some((declaring, module, declarations)) = pending.pop() {
        let mod_rs =
            roots.contains(declaring.as_str()) || declaring.rsplit('/').next() == Some("mod.rs");
        for declared in declarations {
            let Some(path) = candidates(&declaring, mod_rs, &declared)
                .into_iter()
                .find(|path| workspace.root.join(path).is_file())
            else {
                continue;
            };
            if !known.insert(path.clone()) {
                continue;
            }
            let on_disk = workspace.root.join(&path);
            let text = std::fs::read_to_string(&on_disk)
                .map_err(|e| Error::new(format!("{}: {e}", on_disk.display())))?;
            let lines = text.lines().count();
            let declared = Source::new(text).module_declarations();
            pending.push((path.clone(), module, declared));
            found.push(Unanalyzed {
                path,
                declared_in: module,
                lines,
            });
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

/// Where rustc looks for `declared`'s file, as `declaring` writes it:
/// `name.rs`, then `name/mod.rs`, in the directory of `declaring`'s module.
/// `mod_rs`: `declaring` is a crate's root or a `mod.rs`, whose modules are
/// beside it rather than in a directory of its name.
fn candidates(declaring: &str, mod_rs: bool, declared: &Declared) -> Vec<String> {
    let directory = declaring
        .rsplit_once('/')
        .map_or("", |(directory, _)| directory);
    let mut module_directory = match mod_rs {
        true => directory,
        false => declaring.strip_suffix(".rs").unwrap_or(declaring),
    }
    .to_owned();
    for inline in &declared.within {
        module_directory = format!("{module_directory}/{inline}");
    }
    let name = &declared.name;
    let written = match &declared.path {
        Some(path) if path.starts_with('/') => Vec::new(),
        Some(path) if declared.within.is_empty() => vec![format!("{directory}/{path}")],
        Some(path) => vec![format!("{module_directory}/{path}")],
        None => vec![
            format!("{module_directory}/{name}.rs"),
            format!("{module_directory}/{name}/mod.rs"),
        ],
    };
    written.iter().filter_map(|path| normalized(path)).collect()
}

/// `path` without empty parts, `.` and `..`: `a//b/../c.rs` is `a/c.rs`.
/// `None` for a path above the workspace's root.
fn normalized(path: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(within: &[&str], name: &str, path: Option<&str>) -> Declared {
        Declared {
            within: within
                .iter()
                .map(|&directory| directory.to_owned())
                .collect(),
            name: name.to_owned(),
            path: path.map(str::to_owned),
        }
    }

    #[test]
    fn a_mod_rs_or_a_crate_root_declares_beside_itself_any_other_file_below_itself() {
        let child = declared(&[], "child", None);
        assert_eq!(
            candidates("src/lib.rs", true, &child),
            ["src/child.rs", "src/child/mod.rs"]
        );
        assert_eq!(
            candidates("src/net/mod.rs", true, &child),
            ["src/net/child.rs", "src/net/child/mod.rs"]
        );
        assert_eq!(
            candidates("src/net.rs", false, &child),
            ["src/net/child.rs", "src/net/child/mod.rs"]
        );
        assert_eq!(
            candidates("build.rs", true, &child),
            ["child.rs", "child/mod.rs"]
        );
    }

    #[test]
    fn an_inline_module_is_a_directory_of_its_name_or_its_path() {
        let deep = declared(&["outer", "elsewhere"], "deep", None);
        assert_eq!(
            candidates("src/net.rs", false, &deep),
            [
                "src/net/outer/elsewhere/deep.rs",
                "src/net/outer/elsewhere/deep/mod.rs"
            ]
        );
    }

    #[test]
    fn a_path_is_from_the_files_directory_or_inside_an_inline_module_from_its() {
        let generated = declared(&[], "out", Some("../generated/out.rs"));
        assert_eq!(
            candidates("src/net.rs", false, &generated),
            ["generated/out.rs"]
        );
        let inside = declared(&["outer"], "out", Some("out.rs"));
        assert_eq!(
            candidates("src/net.rs", false, &inside),
            ["src/net/outer/out.rs"]
        );
        let above = declared(&[], "out", Some("../../out.rs"));
        assert!(candidates("src/lib.rs", true, &above).is_empty());
        let absolute = declared(&[], "out", Some("/tmp/out.rs"));
        assert!(candidates("src/lib.rs", true, &absolute).is_empty());
    }
}
