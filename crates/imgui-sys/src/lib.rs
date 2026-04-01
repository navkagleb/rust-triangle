#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

mod backends;

include!(concat!(env!("OUT_DIR"), "/imgui_bindings.rs"));

pub use backends::*;
