//! macOS input backends, isolated from the portable validation/controller layer.

mod injector;

pub use injector::{MacDisplayMap, MacQuartzInputInjector};
