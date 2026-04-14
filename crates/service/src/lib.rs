//! Library surface of the meshmon service.
//!
//! Today only exposes the database module. T04 adds config/http/etc. as the
//! service grows its real entry point.

#![deny(rust_2018_idioms, unused_must_use)]
#![warn(missing_docs)]

pub mod db;
