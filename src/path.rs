use crate::directory::DirectoryEntry;
use crate::volume::Volume;
use crate::{Error, Result};
use fs_core::BlockRead;

#[derive(Clone, Debug)]
pub enum Node {
    Root,
    Entry(DirectoryEntry),
}

impl Node {
    pub fn is_directory(&self) -> bool {
        match self {
            Self::Root => true,
            Self::Entry(entry) => entry.is_directory(),
        }
    }

    pub fn entry(&self) -> Option<&DirectoryEntry> {
        match self {
            Self::Root => None,
            Self::Entry(entry) => Some(entry),
        }
    }
}

impl<Device: BlockRead> Volume<Device> {
    pub fn lookup(&self, path: &str) -> Result<Node> {
        let mut current = Node::Root;
        let mut parents: Vec<Node> = Vec::new();
        for (index, component) in path
            .split('/')
            .filter(|component| !component.is_empty())
            .enumerate()
        {
            if index >= 1024 {
                return Err(Error::Unsupported("path exceeds 1024 components"));
            }
            if !current.is_directory() {
                return Err(Error::NotDirectory);
            }
            match component {
                "." => continue,
                ".." => {
                    current = parents.pop().unwrap_or(Node::Root);
                    continue;
                }
                _ => {}
            }
            let name: Vec<_> = component.encode_utf16().take(256).collect();
            if name.len() > 255
                || name
                    .iter()
                    .any(|unit| *unit < 32 || [34, 42, 58, 60, 62, 63, 92, 124].contains(unit))
            {
                return Err(Error::Invalid("path component length or character"));
            }
            let entries = match &current {
                Node::Root => self.root_directory()?,
                Node::Entry(directory) => self.read_directory(directory)?,
            };
            let found = entries
                .into_iter()
                .find(|entry| self.upcase.equal(entry.name_utf16(), &name))
                .ok_or(Error::NotFound)?;
            if found.is_directory() {
                let first = found.stream().first_cluster();
                let ancestor = |node: &Node| match node {
                    Node::Root => self.boot.root_cluster,
                    Node::Entry(entry) => entry.stream().first_cluster(),
                };
                if first != 0
                    && parents
                        .iter()
                        .chain(std::iter::once(&current))
                        .any(|node| ancestor(node) == first)
                {
                    return Err(Error::Invalid("directory ancestor cycle"));
                }
                self.stream_clusters(found.stream())?;
            }
            parents.push(current);
            current = Node::Entry(found);
        }
        if path.ends_with('/') && !current.is_directory() {
            return Err(Error::NotDirectory);
        }
        Ok(current)
    }

    pub fn read_directory_path(&self, path: &str) -> Result<Vec<DirectoryEntry>> {
        match self.lookup(path)? {
            Node::Root => self.root_directory(),
            Node::Entry(entry) if entry.is_directory() => self.read_directory(&entry),
            Node::Entry(_) => Err(Error::NotDirectory),
        }
    }
}
