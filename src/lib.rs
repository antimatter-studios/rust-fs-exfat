#![forbid(unsafe_code)]

pub mod boot;
pub mod device;
pub mod error;

pub use error::{Error, Result};
