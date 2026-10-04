//! Tilde expansion and cached filesystem checks.

/// What a path argument refers to on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    File,
    Directory,
    /// Not an existing path, but a prefix of one.
    Prefix,
    Missing,
}

#[derive(Debug, Default)]
pub struct PathChecker {}
