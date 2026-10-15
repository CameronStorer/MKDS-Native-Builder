#![allow(non_upper_case_globals)]
//! Compile-check runner for the generated user-ROM functions. The actual runtime/game entry path
//! is not implemented yet; this executable proves Cargo compiled the generated Rust lift table.
include!(concat!(env!("OUT_DIR"), "/lifted.rs"));

fn main() {
    println!(
        "Compiled {} lifted functions. Whole-game runtime is not implemented.",
        LIFTED.len()
    );
}
