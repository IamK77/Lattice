use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum Node {
    Directory,
    File(Vec<u8>),
    Link(PathBuf),
}

/// Preserve node kinds and link text without following symlinks. Callers include
/// link targets under the same fixture root to verify those independently too.
pub fn tree(root: &Path) -> BTreeMap<PathBuf, Node> {
    fn visit(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Node>) {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        let key = path.strip_prefix(root).unwrap().to_owned();
        let node = if metadata.file_type().is_symlink() {
            Node::Link(std::fs::read_link(path).unwrap())
        } else if metadata.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), out);
            }
            Node::Directory
        } else {
            Node::File(std::fs::read(path).unwrap())
        };
        out.insert(key, node);
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}
