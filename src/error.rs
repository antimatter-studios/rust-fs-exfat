use std::fmt;

#[derive(Debug)]
pub enum Error {
    Device(fs_core::Error),
    Invalid(&'static str),
    Bounds,
    Unsupported(&'static str),
    NotFound,
    NotDirectory,
    NoSpace,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(error) => write!(formatter, "device: {error}"),
            Self::Invalid(reason) => write!(formatter, "invalid exFAT: {reason}"),
            Self::Bounds => formatter.write_str("read outside device or arithmetic overflow"),
            Self::Unsupported(reason) => write!(formatter, "unsupported exFAT: {reason}"),
            Self::NotFound => formatter.write_str("exFAT path not found"),
            Self::NotDirectory => formatter.write_str("exFAT path component is not a directory"),
            Self::NoSpace => formatter.write_str("exFAT volume has too few free clusters"),
        }
    }
}

impl std::error::Error for Error {}

impl From<fs_core::Error> for Error {
    fn from(error: fs_core::Error) -> Self {
        Self::Device(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
