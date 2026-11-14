//! The system libavif, bound by bindgen (see build.rs). Shared by the decoder and the encoder.

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code)]

include!(concat!(env!("OUT_DIR"), "/avif.rs"));
